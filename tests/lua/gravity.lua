edit("hn.txt")
local A = require("herdr-nvim.annotations")

A.add(0, 1, 1, "t")
vim.api.nvim_buf_set_lines(0, 1, 1, false, { "inserted above" })

local l = A.list()
check(l[1].row == 2 and l[1].end_row == 2, "annotation swallowed inserted line: " .. l[1].row .. "-" .. l[1].end_row)
pass("gravity")
