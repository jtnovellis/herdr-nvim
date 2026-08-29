-- Review what the agent changed.
--
-- The agent writes straight to disk and the reload watcher swaps the new text
-- into the buffer, which is fast but tells you nothing: a file you were
-- reading simply becomes a different file. The transcript records every
-- before/after pair, so the changes can be marked where they landed and
-- stepped through -- and, since the "before" text is right there, undone one
-- at a time.
--
-- This is a review layer, not a staging area: by the time a hunk appears the
-- edit is already on disk, so rejecting one *reverts* it rather than
-- declining to apply it. That is what Cursor's reject button does too.
local R = {}

local ns = vim.api.nvim_create_namespace("herdr-nvim-review")
R.ns = ns

-- Hunks by absolute file path, so an edit to a file that is not open yet is
-- kept and placed when the file is opened.
local pending = {}
local items = {} -- id -> item
local next_id = 0

local function hn()
  return require("herdr-nvim")
end

local function emit()
  pcall(vim.api.nvim_exec_autocmds, "User", {
    pattern = "HerdrNvimReviewChanged",
    modeline = false,
    data = { count = R.count() },
  })
  pcall(vim.cmd.redrawstatus)
end

--- Byte offset (1-based, into the newline-joined buffer) where each line
--- starts. Built once per search so locating a hunk is a scan, not a
--- per-line `line2byte` call.
local function line_starts(lines)
  local starts, acc = {}, 1
  for i, line in ipairs(lines) do
    starts[i] = acc
    acc = acc + #line + 1
  end
  return starts
end

--- The 0-based (row, col) of a 1-based byte offset.
local function pos_of(starts, offset)
  local lo, hi = 1, #starts
  while lo < hi do
    local mid = math.floor((lo + hi + 1) / 2)
    if starts[mid] <= offset then
      lo = mid
    else
      hi = mid - 1
    end
  end
  return lo - 1, offset - starts[lo]
end

--- Where `needle` sits in `buf`, skipping any span already claimed by another
--- hunk. `nil` when the text is not there -- an edit the agent later replaced,
--- or one a human has since rewritten, which is not worth marking.
local function locate(buf, needle, claimed)
  if needle == nil or needle == "" then
    return nil
  end
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local text = table.concat(lines, "\n")
  local starts = line_starts(lines)
  local from = 1
  while true do
    local s, e = string.find(text, needle, from, true)
    if not s then
      return nil
    end
    if not claimed[s] then
      claimed[s] = true
      local srow, scol = pos_of(starts, s)
      local erow, ecol = pos_of(starts, e + 1)
      return srow, scol, erow, ecol, s
    end
    from = s + 1
  end
end

local function place(item)
  local signs = (hn().config or {}).signs
  local marker = item.old == nil and "written" or "edited"
  local group = item.stale and "HerdrNvimStale" or "HerdrNvimReview"
  local opts = {
    id = item.mark,
    right_gravity = true,
    end_right_gravity = false,
    invalidate = true,
    undo_restore = true,
    priority = 110,
    end_row = item.erow,
    end_col = item.ecol,
    hl_group = group,
    line_hl_group = group,
    virt_text = { { " ~ " .. marker .. " by " .. (item.agent or "agent"), "HerdrNvimReviewVirt" } },
    virt_text_pos = "eol",
    sign_text = signs and "~" or nil,
    sign_hl_group = signs and "HerdrNvimReviewVirt" or nil,
  }
  local ok, mark = pcall(vim.api.nvim_buf_set_extmark, item.buf, ns, item.srow, item.scol, opts)
  if ok then
    item.mark = mark
  end
  return ok
end

--- Place every hunk recorded for the file in `buf` that is not placed yet.
function R.attach(buf)
  local file = vim.api.nvim_buf_get_name(buf)
  local queued = file ~= "" and pending[file]
  if not queued or #queued == 0 then
    return 0
  end
  local claimed = {}
  for _, item in pairs(items) do
    if item.buf == buf and item.origin then
      claimed[item.origin] = true
    end
  end
  local placed = 0
  local kept = {}
  for _, edit in ipairs(queued) do
    -- A whole-file write anchors at the top: there is no "before" to point
    -- at, and highlighting every line would just repaint the buffer.
    local srow, scol, erow, ecol, origin
    if edit.old == nil then
      srow, scol, erow, ecol = 0, 0, 0, 0
    else
      srow, scol, erow, ecol, origin = locate(buf, edit.new, claimed)
    end
    if srow then
      next_id = next_id + 1
      local item = {
        id = next_id,
        buf = buf,
        file = file,
        old = edit.old,
        new = edit.new,
        agent = edit.agent,
        srow = srow,
        scol = scol,
        erow = erow,
        ecol = ecol,
        origin = origin,
        stale = false,
      }
      if place(item) then
        items[item.id] = item
        placed = placed + 1
      end
    else
      -- Not in the buffer right now; it may arrive with the next reload.
      table.insert(kept, edit)
    end
  end
  pending[file] = kept
  if placed > 0 then
    emit()
  end
  return placed
end

--- Take a batch of edits from the transcript tail.
function R.record(edits, agent)
  if type(edits) ~= "table" then
    return 0
  end
  local touched = {}
  for _, edit in ipairs(edits) do
    if type(edit) == "table" and type(edit.path) == "string" and edit.path ~= "" then
      pending[edit.path] = pending[edit.path] or {}
      table.insert(pending[edit.path], {
        old = edit.old,
        new = edit.new,
        agent = agent,
      })
      touched[edit.path] = true
    end
  end
  -- Place immediately into any buffer already showing one of these files.
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and touched[vim.api.nvim_buf_get_name(buf)] then
      R.attach(buf)
    end
  end
  return vim.tbl_count(touched)
end

--- Live position of a hunk. Like annotations, a vanished mark is flagged
--- rather than dropped: an undo can bring it back.
local function refresh(item)
  if not (vim.api.nvim_buf_is_valid(item.buf) and vim.api.nvim_buf_is_loaded(item.buf)) then
    item.stale = true
    return item
  end
  local mark = vim.api.nvim_buf_get_extmark_by_id(item.buf, ns, item.mark, { details = true })
  if not mark or #mark == 0 or (mark[3] or {}).invalid then
    item.stale = true
    return item
  end
  local details = mark[3] or {}
  item.srow, item.scol = mark[1], mark[2]
  item.erow = details.end_row or item.srow
  item.ecol = details.end_col or item.scol
  return item
end

--- Hunks in `buf` (or every buffer), in document order.
function R.list(buf)
  local out = {}
  for _, item in pairs(items) do
    if not buf or item.buf == buf then
      table.insert(out, refresh(item))
    end
  end
  table.sort(out, function(a, b)
    if a.buf ~= b.buf then
      return a.buf < b.buf
    end
    if a.srow ~= b.srow then
      return a.srow < b.srow
    end
    return a.id < b.id
  end)
  return out
end

function R.count()
  local n = 0
  for _, item in pairs(items) do
    if not item.stale then
      n = n + 1
    end
  end
  return n
end

--- The hunk containing `row` (0-based) in `buf`.
function R.find_at(buf, row)
  for _, item in ipairs(R.list(buf)) do
    if not item.stale and item.srow <= row and item.erow >= row then
      return item
    end
  end
  return nil
end

local function drop(item)
  pcall(vim.api.nvim_buf_del_extmark, item.buf, ns, item.mark)
  items[item.id] = nil
end

--- Stop marking a hunk. The edit stays; you have simply seen it.
function R.accept(item)
  if not item then
    return false
  end
  drop(item)
  emit()
  return true
end

--- Put the agent's change back the way it was.
---
--- Fails rather than guesses when the text under the mark is no longer what
--- the agent wrote: reverting then would clobber whatever replaced it.
function R.revert(item)
  if not item then
    return false, "no hunk here"
  end
  if item.old == nil then
    return false, "the agent created this file; there is nothing to revert to"
  end
  refresh(item)
  if item.stale then
    return false, "this hunk is no longer in the buffer"
  end
  local current = vim.api.nvim_buf_get_text(item.buf, item.srow, item.scol, item.erow, item.ecol, {})
  if table.concat(current, "\n") ~= item.new then
    return false, "this hunk has changed since the agent wrote it"
  end
  local ok, err = pcall(
    vim.api.nvim_buf_set_text,
    item.buf,
    item.srow,
    item.scol,
    item.erow,
    item.ecol,
    vim.split(item.old, "\n", { plain = true })
  )
  if not ok then
    return false, tostring(err)
  end
  drop(item)
  emit()
  return true
end

--- Clear every hunk in `buf`, or all of them.
function R.clear(buf)
  for _, item in pairs(items) do
    if not buf or item.buf == buf then
      drop(item)
    end
  end
  if not buf then
    pending = {}
  else
    local file = vim.api.nvim_buf_get_name(buf)
    if file ~= "" then
      pending[file] = nil
    end
  end
  emit()
end

--- Move to the next (`step = 1`) or previous (`step = -1`) hunk in the buffer.
function R.jump(step)
  local buf = vim.api.nvim_get_current_buf()
  local list = vim.tbl_filter(function(item)
    return not item.stale
  end, R.list(buf))
  if #list == 0 then
    return nil
  end
  local row = vim.api.nvim_win_get_cursor(0)[1] - 1
  local target
  if step > 0 then
    for _, item in ipairs(list) do
      if item.srow > row then
        target = item
        break
      end
    end
    target = target or list[1]
  else
    for i = #list, 1, -1 do
      if list[i].srow < row then
        target = list[i]
        break
      end
    end
    target = target or list[#list]
  end
  pcall(vim.api.nvim_win_set_cursor, 0, { target.srow + 1, target.scol })
  pcall(vim.cmd, "normal! zz")
  return target
end

--- Buffers going away take their hunks with them.
function R.detach(buf)
  for _, item in pairs(items) do
    if item.buf == buf then
      items[item.id] = nil
    end
  end
end

return R
