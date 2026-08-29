-- The composer opens on the selection, names it, and marks the lines.
vim.g.herdr_nvim_test_uis = 1
local ask = require("herdr-nvim.ask")
edit("hn.txt")
local src = vim.api.nvim_get_current_buf()

vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("Vj ac", true, false, true), "x", false)

check(ask.is_open(), "composer did not open")
-- `startinsert` only takes effect once control returns to the top level, so
-- the mode is not assertable here; check the scratch buffer instead.
local box = vim.api.nvim_get_current_buf()
check(vim.bo[box].buftype == "nofile", "composer buftype: " .. vim.bo[box].buftype)
check(box ~= src, "composer reused the source buffer")

local win = vim.api.nvim_get_current_win()
local title = ""
for _, chunk in ipairs(vim.api.nvim_win_get_config(win).title or {}) do
  title = title .. chunk[1]
end
check(title:find("hn%.txt:1%-2"), "title without the range: " .. title)

local marks =
  vim.api.nvim_buf_get_extmarks(src, vim.api.nvim_create_namespace("herdr-nvim-ask"), 0, -1, { details = true })
check(#marks == 1, "expected 1 range mark, got " .. #marks)
check(marks[1][2] == 0, "mark starts on row " .. marks[1][2])
check(marks[1][4].end_row == 2, "mark ends on row " .. tostring(marks[1][4].end_row))

-- A single line loses the range suffix.
ask.close()
vim.api.nvim_set_current_win(vim.fn.win_findbuf(src)[1])
vim.api.nvim_win_set_cursor(0, { 3, 0 })
require("herdr-nvim").ask()
title = ""
for _, chunk in ipairs(vim.api.nvim_win_get_config(0).title or {}) do
  title = title .. chunk[1]
end
check(title:find("hn%.txt:3 "), "single-line title: " .. title)
ask.close()
pass("ask float")
