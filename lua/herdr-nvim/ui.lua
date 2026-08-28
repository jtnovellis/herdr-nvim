-- Floating list of annotations: hover to preview, ⏎ edit, d delete,
-- o/⇥ go there, s paste, S send, q/⎋ close and restore where you were.
local UI = {}
local A = require("herdr-nvim.annotations")

local state = { win = nil, buf = nil, items = {}, origin = nil, preview = nil, committed = false, target_win = nil, warned = false }
local list_ns = vim.api.nvim_create_namespace("herdr-nvim-list")

local function notify(msg, level)
  require("herdr-nvim").notify(msg, level)
end

local function relname(file)
  if file == "" then
    return "[No Name]"
  end
  return vim.fn.fnamemodify(file, ":~:.")
end

local function is_open()
  return state.win ~= nil and vim.api.nvim_win_is_valid(state.win)
end

local function normal_window(win)
  if not (win and vim.api.nvim_win_is_valid(win)) then
    return false
  end
  if vim.api.nvim_win_get_config(win).relative ~= "" then
    return false
  end
  if vim.wo[win].winfixbuf then
    return false
  end
  local buf = vim.api.nvim_win_get_buf(win)
  return vim.bo[buf].buftype == ""
end

--- Remember what a window showed before the first preview, so closing the
--- list can put it back.
local function remember(win)
  if win and not state.preview and vim.api.nvim_win_is_valid(win) then
    state.preview = {
      win = win,
      buf = vim.api.nvim_win_get_buf(win),
      view = vim.api.nvim_win_call(win, vim.fn.winsaveview),
    }
  end
  return win
end

--- A window that can show a file buffer: the origin, another normal window
--- in this tabpage, or a split created once for previews.
local function pick_target_win()
  if state.origin and normal_window(state.origin.win) then
    return remember(state.origin.win)
  end
  if normal_window(state.target_win) then
    return remember(state.target_win)
  end
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if win ~= state.win and normal_window(win) then
      return remember(win)
    end
  end
  local ok = pcall(function()
    vim.api.nvim_win_call(state.origin and state.origin.win or 0, function()
      vim.cmd("vsplit")
      state.target_win = vim.api.nvim_get_current_win()
    end)
  end)
  if ok and normal_window(state.target_win) then
    return remember(state.target_win)
  end
  return nil
end

local function render()
  state.items = A.list({ include_stale = true, include_delivered = true })
  local lines, locs = {}, {}
  for _, item in ipairs(state.items) do
    local loc = relname(item.file) .. ":" .. (item.row + 1)
    if item.end_row > item.row then
      loc = loc .. "-" .. (item.end_row + 1)
    end
    local prefix = item.stale and "~ " or (item.delivered and "✓ " or "● ")
    local text = item.text:gsub("\n", " ⏎ ")
    table.insert(lines, prefix .. loc .. "  " .. text)
    table.insert(locs, { #prefix, #prefix + #loc, item })
  end
  if #lines == 0 then
    lines = { "(no annotations)" }
  end
  vim.bo[state.buf].modifiable = true
  vim.api.nvim_buf_set_lines(state.buf, 0, -1, false, lines)
  vim.bo[state.buf].modifiable = false
  vim.api.nvim_buf_clear_namespace(state.buf, list_ns, 0, -1)
  for i, loc in ipairs(locs) do
    local hl = loc[3].stale and "HerdrNvimStale" or (loc[3].delivered and "HerdrNvimDelivered" or "HerdrNvimListLoc")
    vim.api.nvim_buf_set_extmark(state.buf, list_ns, i - 1, loc[1], { end_col = loc[2], hl_group = hl })
    if loc[3].stale or loc[3].delivered then
      vim.api.nvim_buf_set_extmark(state.buf, list_ns, i - 1, 0, { end_col = #lines[i], hl_group = hl })
    end
  end
  return lines
end

local function restore_window(saved)
  if not saved or not vim.api.nvim_win_is_valid(saved.win) then
    return
  end
  pcall(function()
    if vim.api.nvim_buf_is_valid(saved.buf) and vim.api.nvim_win_get_buf(saved.win) ~= saved.buf then
      vim.api.nvim_win_set_buf(saved.win, saved.buf)
    end
    vim.api.nvim_win_call(saved.win, function()
      vim.fn.winrestview(saved.view)
    end)
  end)
end

local function restore_origin()
  restore_window(state.preview)
  restore_window(state.origin)
  state.preview = nil
end

--- Close the list. Restores the origin window unless the jump was committed.
function UI.close(opts)
  opts = opts or {}
  if is_open() then
    pcall(vim.api.nvim_win_close, state.win, true)
  end
  state.win = nil
  if not state.committed and not opts.keep then
    restore_origin()
  end
  if state.origin and vim.api.nvim_win_is_valid(state.origin.win) and not opts.keep then
    pcall(vim.api.nvim_set_current_win, state.committed and (state.target_win or state.origin.win) or state.origin.win)
  end
end

function UI.current()
  if not is_open() then
    return nil
  end
  local row = vim.api.nvim_win_get_cursor(state.win)[1]
  return state.items[row]
end

--- Show the hovered annotation in a normal window (focus stays in the list).
function UI.jump()
  local item = UI.current()
  if not item then
    return
  end
  local win = pick_target_win()
  if not win then
    if not state.warned then
      state.warned = true
      notify("no window can show the annotated file", vim.log.levels.WARN)
    end
    return
  end
  if vim.api.nvim_win_get_buf(win) ~= item.buf then
    local ok = pcall(vim.api.nvim_win_set_buf, win, item.buf)
    if not ok then
      if not state.warned then
        state.warned = true
        notify("cannot show the annotated file in that window", vim.log.levels.WARN)
      end
      return
    end
  end
  local last = vim.api.nvim_buf_line_count(item.buf)
  pcall(vim.api.nvim_win_set_cursor, win, { math.min(item.row + 1, last), 0 })
  pcall(vim.api.nvim_win_call, win, function()
    vim.cmd("normal! zz")
  end)
end

--- Go to the hovered annotation and close the list there.
function UI.goto_item()
  local item = UI.current()
  if not item then
    return
  end
  UI.jump()
  state.committed = true
  if state.origin and vim.api.nvim_win_is_valid(state.origin.win) then
    state.target_win = state.target_win or state.origin.win
  end
  UI.close()
end

function UI.edit()
  local item = UI.current()
  if not item then
    return
  end
  local row = vim.api.nvim_win_get_cursor(state.win)[1]
  UI.close({ keep = true })
  require("herdr-nvim").edit(item.id, function()
    UI.open({ cursor = row, origin = state.origin })
  end)
end

function UI.delete()
  local item = UI.current()
  if not item then
    return
  end
  local row = vim.api.nvim_win_get_cursor(state.win)[1]
  A.remove(item.id)
  if A.total() == 0 then
    UI.close()
    notify("no annotations left")
    return
  end
  local lines = render()
  vim.api.nvim_win_set_cursor(state.win, { math.min(row, #lines), 0 })
  UI.jump()
end

function UI.open(opts)
  opts = opts or {}
  if A.total() == 0 then
    notify("no annotations")
    return
  end
  if is_open() then
    UI.close({ keep = true })
  end
  state.committed = false
  state.warned = false
  state.preview = nil
  if opts.origin and vim.api.nvim_win_is_valid(opts.origin.win) then
    state.origin = opts.origin
  else
    local win = vim.api.nvim_get_current_win()
    state.origin = { win = win, buf = vim.api.nvim_win_get_buf(win), view = vim.fn.winsaveview() }
    pcall(vim.api.nvim_win_call, win, function()
      vim.cmd("normal! m'")
    end)
  end
  state.buf = vim.api.nvim_create_buf(false, true)
  vim.bo[state.buf].bufhidden = "wipe"
  vim.bo[state.buf].buftype = "nofile"
  vim.bo[state.buf].filetype = "herdr-annotations"
  local lines = render()

  local longest = 0
  for _, line in ipairs(lines) do
    longest = math.max(longest, vim.fn.strdisplaywidth(line))
  end
  local width = math.max(1, math.min(math.max(50, longest + 2), vim.o.columns - 4))
  local height = math.max(1, math.min(#lines, math.floor(vim.o.lines * 0.4)))
  state.win = vim.api.nvim_open_win(state.buf, true, {
    relative = "editor",
    width = width,
    height = height,
    row = math.max(0, math.floor((vim.o.lines - height) / 2) - 1),
    col = math.max(0, math.floor((vim.o.columns - width) / 2)),
    style = "minimal",
    border = "rounded",
    title = " Annotations · ⏎ edit  d delete  o go  s paste  S send  q close ",
    title_pos = "center",
  })
  vim.wo[state.win].cursorline = true
  vim.wo[state.win].wrap = true
  vim.wo[state.win].linebreak = true

  local function map(lhs, rhs)
    vim.keymap.set("n", lhs, rhs, { buffer = state.buf, nowait = true, silent = true })
  end
  map("<CR>", UI.edit)
  map("d", UI.delete)
  map("o", UI.goto_item)
  map("<Tab>", UI.goto_item)
  map("q", UI.close)
  map("<Esc>", UI.close)
  map("s", function()
    UI.close()
    require("herdr-nvim").paste()
  end)
  map("S", function()
    UI.close()
    require("herdr-nvim").send()
  end)

  local group = vim.api.nvim_create_augroup("HerdrNvimList", { clear = true })
  vim.api.nvim_create_autocmd("CursorMoved", { group = group, buffer = state.buf, callback = UI.jump })
  vim.api.nvim_create_autocmd("WinLeave", {
    group = group,
    buffer = state.buf,
    callback = function()
      if is_open() then
        UI.close()
      end
    end,
  })
  if opts.cursor then
    pcall(vim.api.nvim_win_set_cursor, state.win, { math.min(opts.cursor, #lines), 0 })
  end
  UI.jump()
end

function UI.is_open()
  return is_open()
end

return UI
