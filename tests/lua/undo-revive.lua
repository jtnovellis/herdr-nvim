edit("hn.txt")
local A = require("herdr-nvim.annotations")

A.add(0, 1, 1, "t")
vim.cmd("normal! 2Gdd")
check(#A.list() == 0 and #A.list({ include_stale = true }) == 1, "expected stale after dd")

vim.cmd("silent normal! u")
local l = A.list()
check(#l == 1 and l[1].row == 1, "undo did not revive the annotation")
pass("undo revive")
