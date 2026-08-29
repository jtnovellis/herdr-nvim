edit("hn.txt")
local A = require("herdr-nvim.annotations")
local UI = require("herdr-nvim.ui")

local file_win = vim.api.nvim_get_current_win()
local file_buf = vim.api.nvim_get_current_buf()

edit("other.txt")
local other_buf = vim.api.nvim_get_current_buf()
A.add(0, 0, 0, "in other")

vim.api.nvim_win_set_buf(file_win, file_buf)
vim.api.nvim_win_set_cursor(file_win, { 4, 0 })

vim.cmd("vsplit")
local tree_win = vim.api.nvim_get_current_win()
local scratch = vim.api.nvim_create_buf(false, true)
vim.api.nvim_win_set_buf(tree_win, scratch)
vim.wo[tree_win].winfixbuf = true

UI.open()
check(UI.is_open(), "float did not open")
check(vim.api.nvim_win_get_buf(tree_win) == scratch, "winfixbuf window was changed")
check(vim.api.nvim_win_get_buf(file_win) == other_buf, "preview did not use the normal window")

UI.close()
check(vim.api.nvim_win_get_buf(file_win) == file_buf, "origin window not restored (from a non-normal origin)")

vim.api.nvim_set_current_win(file_win)
UI.open()
check(vim.api.nvim_win_get_buf(file_win) == other_buf, "hover did not preview")

UI.close()
check(
  vim.api.nvim_win_get_buf(file_win) == file_buf and vim.api.nvim_win_get_cursor(file_win)[1] == 4,
  "origin buffer/cursor not restored"
)

vim.api.nvim_set_current_win(file_win)
UI.open()
UI.goto_item()
check(vim.api.nvim_win_get_buf(file_win) == other_buf, "goto did not commit the jump")
pass("winfixbuf jump + restore")
