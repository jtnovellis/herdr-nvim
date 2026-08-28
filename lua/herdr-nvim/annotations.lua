-- In-memory, extmark-tracked annotations. Nothing is persisted: they follow
-- your edits, survive reloads, and disappear when sent, deleted, or when the
-- buffer is deleted.
local A = {}

local ns = vim.api.nvim_create_namespace("herdr-nvim")
A.ns = ns
A.config = { signs = false }

local items = {} -- id -> item
local next_id = 0
local pending_count = 0
local emit_scheduled = false

A.on_change = nil

--- Recompute the cached count and let statuslines know.
local function changed()
  A.recount()
  if emit_scheduled then
    return
  end
  emit_scheduled = true
  vim.schedule(function()
    emit_scheduled = false
    if A.on_change then
      pcall(A.on_change)
    end
    pcall(vim.api.nvim_exec_autocmds, "User", { pattern = "HerdrNvimAnnotationsChanged", modeline = false })
    pcall(vim.cmd.redrawstatus)
  end)
end

local function line_len(buf, row)
  local line = vim.api.nvim_buf_get_lines(buf, row, row + 1, false)[1] or ""
  return #line
end

local function first_line(buf, row)
  return vim.api.nvim_buf_get_lines(buf, row, row + 1, false)[1] or ""
end

local function short(text)
  local first = text:gsub("\n.*", " …")
  if vim.fn.strchars(first) > 60 then
    first = vim.fn.strcharpart(first, 0, 57) .. "…"
  end
  return first
end

local function groups(item)
  if item.stale then
    return "HerdrNvimStale", "HerdrNvimStale", "HerdrNvimStale"
  elseif item.delivered then
    return "HerdrNvimDelivered", "HerdrNvimDelivered", "HerdrNvimDelivered"
  end
  return "HerdrNvimAnnotation", "HerdrNvimSign", "HerdrNvimVirt"
end

--- (Re)place the extmark for an item over rows srow..erow (0-indexed, inclusive).
local function place(item, srow, erow)
  local last = vim.api.nvim_buf_line_count(item.buf) - 1
  srow = math.max(0, math.min(srow, last))
  erow = math.max(srow, math.min(erow, last))
  local line_hl, sign_hl, virt_hl = groups(item)
  local marker = item.stale and "~" or (item.delivered and "✓" or "●")
  local opts = {
    id = item.mark,
    right_gravity = true,
    invalidate = true,
    undo_restore = true,
    priority = 120,
    line_hl_group = line_hl,
    virt_text = { { " " .. marker .. " " .. short(item.text), virt_hl } },
    virt_text_pos = "eol",
  }
  if erow > srow then
    opts.hl_group = line_hl
    opts.hl_eol = true
    opts.end_right_gravity = false
    if erow < last then
      opts.end_row, opts.end_col = erow + 1, 0
    else
      opts.end_row, opts.end_col = erow, line_len(item.buf, erow)
    end
  end
  if A.config.signs then
    opts.sign_text = marker
    opts.sign_hl_group = sign_hl
  else
    opts.number_hl_group = sign_hl
  end
  item.mark = vim.api.nvim_buf_set_extmark(item.buf, ns, srow, 0, opts)
  item.row, item.end_row = srow, erow
end

--- Refresh an item's live position. Never deletes: a vanished range marks
--- the item stale (an undo may bring it back).
function A.refresh(item)
  if not (vim.api.nvim_buf_is_valid(item.buf) and vim.api.nvim_buf_is_loaded(item.buf)) then
    item.stale = true
    return item
  end
  local mark = vim.api.nvim_buf_get_extmark_by_id(item.buf, ns, item.mark, { details = true })
  if not mark or #mark == 0 then
    item.stale = true
    return item
  end
  local details = mark[3] or {}
  if details.invalid then
    item.stale = true
    return item
  end
  local row = mark[1]
  local end_row = details.end_row or row
  if (details.end_col or 0) == 0 and end_row > row then
    end_row = end_row - 1
  end
  if end_row < row then
    end_row = row
  end
  item.row, item.end_row = row, end_row
  item.stale = item.reload_mismatch == true
  return item
end

function A.add(buf, srow, erow, text)
  if buf == 0 or buf == nil then
    buf = vim.api.nvim_get_current_buf()
  end
  next_id = next_id + 1
  local item = {
    id = next_id,
    buf = buf,
    file = vim.api.nvim_buf_get_name(buf),
    text = text,
    stale = false,
    delivered = false,
    created = os.time(),
  }
  place(item, srow, erow)
  item.snapshot = first_line(buf, item.row)
  items[item.id] = item
  changed()
  return item
end

function A.get(id)
  return items[id]
end

function A.update(id, text)
  local item = items[id]
  if not item then
    return
  end
  item.text = text
  item.delivered = false
  item.reload_mismatch = nil
  A.refresh(item)
  if vim.api.nvim_buf_is_loaded(item.buf) then
    item.stale = false
    place(item, item.row, item.end_row)
    item.snapshot = first_line(item.buf, item.row)
  end
  changed()
end

function A.remove(id)
  local item = items[id]
  if not item then
    return
  end
  if vim.api.nvim_buf_is_valid(item.buf) then
    pcall(vim.api.nvim_buf_del_extmark, item.buf, ns, item.mark)
  end
  items[id] = nil
  changed()
end

local function remove_where(pred)
  local removed = 0
  for id, item in pairs(items) do
    if pred(item) then
      if vim.api.nvim_buf_is_valid(item.buf) then
        pcall(vim.api.nvim_buf_del_extmark, item.buf, ns, item.mark)
      end
      items[id] = nil
      removed = removed + 1
    end
  end
  if removed > 0 then
    changed()
  end
  return removed
end

function A.clear()
  return remove_where(function()
    return true
  end)
end

--- Forget stale and delivered items.
function A.reap_stale()
  return remove_where(function(item)
    A.refresh(item)
    return item.stale or item.delivered
  end)
end

--- Mark every pending item as delivered (kept, shown with ✓).
function A.mark_delivered()
  for _, item in pairs(items) do
    A.refresh(item)
    if not item.stale then
      item.delivered = true
      if vim.api.nvim_buf_is_loaded(item.buf) then
        place(item, item.row, item.end_row)
      end
    end
  end
  changed()
end

--- Sorted snapshot with live positions. By default only pending items
--- (not stale, not delivered); `opts.include_stale` / `opts.include_delivered`.
function A.list(opts)
  opts = opts or {}
  local out = {}
  for id, item in pairs(items) do
    A.refresh(item)
    local keep = (not item.stale or opts.include_stale) and (not item.delivered or opts.include_delivered)
    if keep then
      table.insert(out, {
        id = id,
        buf = item.buf,
        text = item.text,
        row = item.row,
        end_row = item.end_row,
        file = item.file,
        stale = item.stale,
        delivered = item.delivered,
      })
    end
  end
  table.sort(out, function(a, b)
    if a.file ~= b.file then
      return a.file < b.file
    end
    if a.row ~= b.row then
      return a.row < b.row
    end
    return a.id < b.id
  end)
  return out
end

function A.recount()
  local n = 0
  for _, item in pairs(items) do
    A.refresh(item)
    if not item.stale and not item.delivered then
      n = n + 1
    end
  end
  pending_count = n
  return n
end

--- Cached pending count (cheap enough for a statusline).
function A.count()
  return pending_count
end

function A.total()
  local n = 0
  for _ in pairs(items) do
    n = n + 1
  end
  return n
end

--- The first item whose range intersects rows srow..erow in buf.
function A.find_overlapping(buf, srow, erow)
  for _, item in ipairs(A.list({ include_stale = true, include_delivered = true })) do
    if item.buf == buf and item.row <= erow and item.end_row >= srow then
      return item
    end
  end
  return nil
end

function A.find_at(buf, row)
  return A.find_overlapping(buf, row, row)
end

--- After a reload, clamp every item of the buffer back onto real lines and
--- flag the ones whose text no longer matches what was annotated.
function A.reattach_buffer(buf)
  local touched = false
  for _, item in pairs(items) do
    if item.buf == buf and vim.api.nvim_buf_is_loaded(buf) then
      touched = true
      local mark = vim.api.nvim_buf_get_extmark_by_id(buf, ns, item.mark, { details = true })
      local last = vim.api.nvim_buf_line_count(buf) - 1
      local row, end_row = item.row or 0, item.end_row or 0
      if mark and #mark > 0 and not (mark[3] or {}).invalid then
        row = mark[1]
        end_row = (mark[3] or {}).end_row or row
        if ((mark[3] or {}).end_col or 0) == 0 and end_row > row then
          end_row = end_row - 1
        end
      end
      row = math.max(0, math.min(row, last))
      end_row = math.max(row, math.min(end_row, last))
      item.reload_mismatch = first_line(buf, row) ~= item.snapshot or nil
      item.stale = item.reload_mismatch == true
      place(item, row, end_row)
    end
  end
  if touched then
    changed()
  end
end

local group = vim.api.nvim_create_augroup("HerdrNvimAnnotations", { clear = true })
vim.api.nvim_create_autocmd({ "BufDelete", "BufWipeout" }, {
  group = group,
  callback = function(ev)
    remove_where(function(item)
      return item.buf == ev.buf
    end)
  end,
})
-- :e / :e! fire BufReadPost; an autoread reload fires FileChangedShellPost.
vim.api.nvim_create_autocmd({ "BufReadPost", "FileChangedShellPost" }, {
  group = group,
  callback = function(ev)
    A.reattach_buffer(ev.buf)
  end,
})
vim.api.nvim_create_autocmd("BufWritePost", {
  group = group,
  callback = function()
    changed()
  end,
})

return A
