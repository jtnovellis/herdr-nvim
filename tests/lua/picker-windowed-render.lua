-- The list buffer must only ever hold one screenful. Rendering every match
-- meant a line plus an extmark per matched character for the whole result set
-- on every keystroke, which is what made the picker crawl in a large repo.
local P = require("herdr-nvim.picker")

local cands = {}
for i = 1, 2000 do
  cands[i] = { path = ("/repo/src/mod%d/file_%d.lua"):format(i % 20, i), session = true }
end

P.open({ candidates = cands, cwd = "/repo", max_files = 5000 }, { force_without_ui = true })
check(P.is_open(), "picker did not open")

local list_win, list_buf
for _, w in ipairs(vim.api.nvim_list_wins()) do
  local cfg = vim.api.nvim_win_get_config(w)
  if cfg.relative == "editor" and cfg.height ~= 1 then
    list_win, list_buf = w, vim.api.nvim_win_get_buf(w)
  end
end
check(list_win, "could not locate the list float")

local height = vim.api.nvim_win_get_height(list_win)
local rendered = vim.api.nvim_buf_line_count(list_buf)
check(
  rendered <= height,
  ("rendered %d lines into a %d-row window: the render is not windowed"):format(rendered, height)
)

-- The title still reports the full match count, not the rendered slice.
local title = vim.api.nvim_win_get_config(list_win).title
local text = type(title) == "table" and title[1][1] or tostring(title)
check(text:find("2000"), "window title lost the full count: " .. text)

-- Paging past the bottom scrolls rather than growing the buffer.
local first_before = vim.api.nvim_buf_get_lines(list_buf, 0, 1, false)[1]
for _ = 1, height + 3 do
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("<C-n>", true, false, true), "x", false)
end
check(
  vim.api.nvim_buf_line_count(list_buf) <= height,
  "buffer grew past the window while scrolling: " .. vim.api.nvim_buf_line_count(list_buf)
)
check(vim.api.nvim_buf_get_lines(list_buf, 0, 1, false)[1] ~= first_before, "the list did not scroll")
P.close()
pass("picker renders only the visible window")
