-- Fuzzy file picker shown inside the sidebar: the files the agent touched
-- this session first (newest first, with diff stats), then the whole repo
-- once you type. Matching uses Neovim's own matchfuzzypos().
local P = {}

-- `top` is the first match rendered, so the list buffer only ever holds one
-- screenful regardless of how many candidates matched. `last_query` /
-- `last_matches` let a query that extends the previous one narrow the previous
-- result set instead of rescanning every candidate.
local state = {
  list_win = nil,
  list_buf = nil,
  prompt_win = nil,
  prompt_buf = nil,
  spec = nil,
  matches = {},
  cursor = 1,
  top = 1,
  last_query = nil,
  last_matches = nil,
  pending_query = nil,
  debounce = nil,
}
local ns = vim.api.nvim_create_namespace("herdr-nvim-picker")
-- Long enough to coalesce fast typing, short enough to feel immediate.
local REFILTER_DEBOUNCE_MS = 30

local function notify(msg, level)
  require("herdr-nvim").notify(msg, level)
end

local function is_open()
  return state.list_win and vim.api.nvim_win_is_valid(state.list_win)
end

local function age(now, ts)
  local secs = math.max(0, now - ts)
  if secs < 60 then
    return "now"
  elseif secs < 3600 then
    return math.floor(secs / 60) .. "m"
  elseif secs < 86400 then
    return math.floor(secs / 3600) .. "h"
  end
  return math.floor(secs / 86400) .. "d"
end

local function display_path(path, cwd, home)
  if cwd and cwd ~= "" and path:sub(1, #cwd + 1) == cwd .. "/" then
    return path:sub(#cwd + 2)
  end
  if home and home ~= "" and path:sub(1, #home + 1) == home .. "/" then
    return "~/" .. path:sub(#home + 2)
  end
  return path
end

--- Rank candidates for `query`: empty query → session files only (capped),
--- otherwise fuzzy over everything. Returns { {cand=, positions=} }.
function P.rank(cands, query, max_files)
  local out = {}
  if query == "" then
    for _, c in ipairs(cands) do
      if c.session then
        table.insert(out, { cand = c, positions = {} })
        if #out >= (max_files or 20) then
          break
        end
      end
    end
    return out
  end
  local items, positions = unpack(vim.fn.matchfuzzypos(cands, query, { key = "display" }))
  for i, c in ipairs(items) do
    table.insert(out, { cand = c, positions = positions[i] or {} })
  end
  return out
end

--- Byte offset of the 0-based character index `idx` in `str`.
---
--- Must be byteidx(), not vim.str_byteindex(): matchfuzzypos() counts
--- characters the way Vim does, which does not count combining marks
--- separately, while str_byteindex() counts UTF-32 codepoints, which does.
--- The two disagree on decomposed text such as "e" + U+0301.
--- byteidx() returns -1 past the end, so clamp.
local function char_to_byte(str, idx)
  local byte = vim.fn.byteidx(str, idx)
  if byte < 0 then
    return #str
  end
  return byte
end

local function render()
  if not is_open() then
    return
  end
  -- The floats use bufhidden=wipe, so a buffer can be gone while its window
  -- still exists; this runs from a TextChanged autocmd, where a throw escapes.
  if not (state.list_buf and vim.api.nvim_buf_is_valid(state.list_buf)) then
    return
  end
  local spec = state.spec
  local now = os.time()
  local width = vim.api.nvim_win_get_width(state.list_win)
  local height = math.max(1, vim.api.nvim_win_get_height(state.list_win))

  -- Only the visible slice is built. Rendering every match meant one line plus
  -- one extmark per matched character for the whole result set on every
  -- keystroke, which is what made the picker crawl in a large repo.
  local total = #state.matches
  if state.cursor < state.top then
    state.top = state.cursor
  elseif state.cursor > state.top + height - 1 then
    state.top = state.cursor - height + 1
  end
  state.top = math.max(1, math.min(state.top, math.max(1, total - height + 1)))
  local last = math.min(total, state.top + height - 1)

  local lines, meta = {}, {}
  for idx = state.top, last do
    local m = state.matches[idx]
    local c = m.cand
    local right = ""
    if c.diff_stat then
      right = string.format("+%d -%d", c.diff_stat[1], c.diff_stat[2])
    elseif c.newly_created then
      right = "new"
    end
    if c.touched_unix then
      right = right .. (right ~= "" and "  " or "") .. age(now, c.touched_unix)
    end
    local left = c.display or display_path(c.path, spec.cwd, spec.home)
    c.display = left
    local pad = math.max(1, width - vim.fn.strdisplaywidth(left) - vim.fn.strdisplaywidth(right) - 3)
    table.insert(lines, "  " .. left .. string.rep(" ", pad) .. right)
    table.insert(meta, {
      positions = m.positions,
      display = left,
      right = right,
      diff = c.diff_stat ~= nil,
      new = c.newly_created,
    })
  end
  if #lines == 0 then
    lines = { "  (no matches)" }
  end
  vim.bo[state.list_buf].modifiable = true
  vim.api.nvim_buf_set_lines(state.list_buf, 0, -1, false, lines)
  vim.bo[state.list_buf].modifiable = false
  vim.api.nvim_buf_clear_namespace(state.list_buf, ns, 0, -1)
  for i, m in ipairs(meta) do
    for _, pos in ipairs(m.positions) do
      -- matchfuzzypos() reports *character* indices, but extmark columns are
      -- byte offsets, so a multibyte path would otherwise highlight the wrong
      -- column (or land mid-codepoint and throw). The line has 2 leading spaces.
      local from = char_to_byte(m.display, pos) + 2
      local to = char_to_byte(m.display, pos + 1) + 2
      pcall(
        vim.api.nvim_buf_set_extmark,
        state.list_buf,
        ns,
        i - 1,
        from,
        { end_col = to, hl_group = "HerdrNvimPickerMatch" }
      )
    end
    if m.right ~= "" then
      local col = #lines[i] - #m.right
      local hl = m.diff and "HerdrNvimPickerDiff" or (m.new and "HerdrNvimPickerNew" or "HerdrNvimPickerAge")
      pcall(vim.api.nvim_buf_set_extmark, state.list_buf, ns, i - 1, col, { end_col = #lines[i], hl_group = hl })
    end
  end
  state.cursor = math.max(1, math.min(state.cursor, math.max(1, total)))
  if total > 0 then
    pcall(vim.api.nvim_win_set_cursor, state.list_win, { state.cursor - state.top + 1, 0 })
  end
  local count = spec.query == "" and (total .. " files") or (total .. " matches")
  pcall(
    vim.api.nvim_win_set_config,
    state.list_win,
    { title = " " .. (spec.title or "open file") .. " · " .. count .. " ", title_pos = "center" }
  )
end

local function apply_query(query)
  state.spec.query = query
  state.pending_query = query

  -- A query that extends the previous one can only match a subset of the
  -- previous results, so narrow those instead of rescanning every candidate.
  local pool = state.spec.candidates
  if
    query ~= ""
    and state.last_query
    and state.last_query ~= ""
    and #state.last_query < #query
    and query:sub(1, #state.last_query) == state.last_query
    and state.last_matches
  then
    pool = {}
    for _, m in ipairs(state.last_matches) do
      table.insert(pool, m.cand)
    end
  end

  state.matches = P.rank(pool, query, state.spec.max_files)
  state.last_query, state.last_matches = query, state.matches
  state.cursor = 1
  state.top = 1
  render()
end

local function refilter()
  if not (state.prompt_buf and vim.api.nvim_buf_is_valid(state.prompt_buf)) then
    return
  end
  local query = vim.trim(vim.api.nvim_buf_get_lines(state.prompt_buf, 0, 1, false)[1] or "")
  query = query:gsub("^› ?", "")
  -- Compare against what is *scheduled*, not what is applied: typing a
  -- character and deleting it again inside the debounce window would otherwise
  -- return early here and leave the pending timer to apply the character that
  -- is no longer in the prompt.
  if query == state.pending_query then
    return
  end
  state.pending_query = query
  -- Coalesce bursts of typing: one filter per quiet moment, not per keystroke.
  if state.debounce then
    state.debounce:stop()
  else
    state.debounce = (vim.uv or vim.loop).new_timer()
  end
  state.debounce:start(
    REFILTER_DEBOUNCE_MS,
    0,
    vim.schedule_wrap(function()
      if is_open() then
        apply_query(query)
      end
    end)
  )
end

function P.close()
  if state.debounce then
    state.debounce:stop()
    state.debounce:close()
    state.debounce = nil
  end
  state.last_query, state.last_matches, state.pending_query = nil, nil, nil
  for _, key in ipairs({ "list_win", "prompt_win" }) do
    if state[key] and vim.api.nvim_win_is_valid(state[key]) then
      pcall(vim.api.nvim_win_close, state[key], true)
    end
    state[key] = nil
  end
  vim.cmd("stopinsert")
end

local function choose()
  local m = state.matches[state.cursor]
  local on_choose = state.spec and state.spec.on_choose
  P.close()
  if not m then
    return
  end
  local c = m.cand
  -- A caller that wants the file itself rather than a window showing it --
  -- the ask composer attaching a path, say -- says so here.
  if on_choose then
    on_choose(c)
    return
  end
  if not pcall(vim.cmd, "drop " .. vim.fn.fnameescape(c.path)) then
    notify("could not open " .. vim.fn.fnamemodify(c.path, ":~:."), vim.log.levels.ERROR)
    return
  end
  if c.line then
    local last = vim.api.nvim_buf_line_count(0)
    pcall(vim.api.nvim_win_set_cursor, 0, { math.min(c.line, last), 0 })
    vim.cmd("normal! zz")
  end
end

local function move(delta)
  if #state.matches == 0 then
    return
  end
  state.cursor = ((state.cursor - 1 + delta) % #state.matches) + 1
  -- render() scrolls the window to keep the cursor visible.
  render()
end

--- Open the picker. `spec = { candidates = {...}, cwd = "...", max_files = 20, title = "...",
--- on_choose = function(candidate) end }` where each candidate has `path` and
--- optionally `line`, `session`, `newly_created`, `touched_unix`,
--- `diff_stat = {added, removed}`. Without `on_choose` the file is opened.
function P.open(spec, opts)
  opts = opts or {}
  P.close()
  if not spec or not spec.candidates or #spec.candidates == 0 then
    notify("no files to pick from", vim.log.levels.INFO)
    return
  end
  -- A float opened while no UI is attached is sized for the headless 80x24
  -- default and never repositioned: wait for the sidebar client instead.
  if #vim.api.nvim_list_uis() == 0 and not opts.force_without_ui then
    local group = vim.api.nvim_create_augroup("HerdrNvimPickerDeferred", { clear = true })
    local expiry = (vim.uv or vim.loop).new_timer()
    -- One handle per deferred open, so release it on both exits and drop the
    -- augroup with it: `once` removes the autocmd but leaves the group behind.
    local function release()
      if expiry then
        expiry:stop()
        expiry:close()
        expiry = nil
      end
      pcall(vim.api.nvim_del_augroup_by_name, "HerdrNvimPickerDeferred")
    end
    expiry:start(10000, 0, function()
      vim.schedule(release)
    end)
    vim.api.nvim_create_autocmd("UIEnter", {
      group = group,
      once = true,
      callback = function()
        release()
        vim.schedule(function()
          P.open(spec, { force_without_ui = true })
        end)
      end,
    })
    return
  end
  -- One pass, not two: matchfuzzypos() matches on `display`, so it has to be
  -- populated for every candidate before ranking, but normalising the JSON
  -- nulls alongside it halves the work on a large repo.
  spec.home = vim.env.HOME
  for _, c in ipairs(spec.candidates) do
    if c.line == vim.NIL then
      c.line = nil
    end
    if c.diff_stat == vim.NIL then
      c.diff_stat = nil
    end
    if c.touched_unix == vim.NIL then
      c.touched_unix = nil
    end
    c.display = display_path(c.path, spec.cwd, spec.home)
  end
  spec.query = ""
  state.spec = spec
  state.cursor, state.top = 1, 1

  local width = math.max(40, math.min(math.floor(vim.o.columns * 0.9), 100))
  local height = math.max(5, math.min(math.floor(vim.o.lines * 0.5), 20))
  local row = math.max(1, math.floor((vim.o.lines - height) / 2) - 2)
  local col = math.floor((vim.o.columns - width) / 2)

  state.list_buf = vim.api.nvim_create_buf(false, true)
  vim.bo[state.list_buf].bufhidden = "wipe"
  vim.bo[state.list_buf].buftype = "nofile"
  state.list_win = vim.api.nvim_open_win(state.list_buf, false, {
    relative = "editor",
    width = width,
    height = height,
    row = row + 2,
    col = col,
    style = "minimal",
    border = "rounded",
    title = " open file ",
    title_pos = "center",
  })
  vim.wo[state.list_win].cursorline = true

  state.prompt_buf = vim.api.nvim_create_buf(false, true)
  vim.bo[state.prompt_buf].bufhidden = "wipe"
  vim.bo[state.prompt_buf].buftype = "nofile"
  state.prompt_win = vim.api.nvim_open_win(state.prompt_buf, true, {
    relative = "editor",
    width = width,
    height = 1,
    row = row,
    col = col,
    style = "minimal",
    border = "rounded",
    title = " ↑↓ move · ⏎ open · esc close ",
    title_pos = "center",
  })
  vim.api.nvim_buf_set_lines(state.prompt_buf, 0, -1, false, { "› " })

  local function map(mode, lhs, fn)
    vim.keymap.set(mode, lhs, fn, { buffer = state.prompt_buf, nowait = true, silent = true })
  end
  map({ "i", "n" }, "<CR>", choose)
  map({ "i", "n" }, "<Esc>", P.close)
  map({ "i", "n" }, "<C-c>", P.close)
  map({ "i", "n" }, "<Down>", function()
    move(1)
  end)
  map({ "i", "n" }, "<Up>", function()
    move(-1)
  end)
  map({ "i", "n" }, "<C-n>", function()
    move(1)
  end)
  map({ "i", "n" }, "<C-p>", function()
    move(-1)
  end)
  map({ "i", "n" }, "<C-u>", function()
    vim.api.nvim_buf_set_lines(state.prompt_buf, 0, -1, false, { "› " })
    refilter()
  end)
  map("n", "q", P.close)

  local group = vim.api.nvim_create_augroup("HerdrNvimPicker", { clear = true })
  vim.api.nvim_create_autocmd(
    { "TextChangedI", "TextChanged" },
    { group = group, buffer = state.prompt_buf, callback = refilter }
  )
  vim.api.nvim_create_autocmd("WinLeave", {
    group = group,
    buffer = state.prompt_buf,
    callback = function()
      vim.schedule(function()
        if is_open() and vim.api.nvim_get_current_win() ~= state.prompt_win then
          P.close()
        end
      end)
    end,
  })

  -- Populate synchronously; the debounce exists for typing, not for opening.
  apply_query("")
  vim.api.nvim_win_set_cursor(state.prompt_win, { 1, 2 })
  vim.cmd("startinsert!")
end

--- Open the picker from a handoff JSON file written by `herdr-nvim pick`.
--- The file is deleted after reading.
function P.open_file(path, opts)
  local ok, raw = pcall(vim.fn.readfile, path)
  pcall(os.remove, path)
  if not ok then
    notify("cannot read picker handoff " .. tostring(path), vim.log.levels.ERROR)
    return false
  end
  local decoded, spec = pcall(vim.json.decode, table.concat(raw, "\n"), { luanil = { object = true, array = true } })
  if not decoded or type(spec) ~= "table" then
    notify("invalid picker handoff", vim.log.levels.ERROR)
    return false
  end
  P.open(spec, opts)
  return true
end

function P.is_open()
  return is_open() == true
end

return P
