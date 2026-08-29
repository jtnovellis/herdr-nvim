edit("hn.txt")
local A = require("herdr-nvim.annotations")

A.add(0, 1, 2, "t")
vim.fn.writefile({ "a", "b", "c", "d", "e", "f" }, tmp("hn.txt"))
vim.cmd("checktime")
check(vim.api.nvim_buf_line_count(0) == 6, "autoread did not reload")

local l = A.list()
check(#l == 1 and not l[1].stale and l[1].row == 1, "lost or stale after unchanged-range reload")

-- Neovim detects changes by mtime/size: change the size too.
vim.fn.writefile({ "a", "BBBB", "c", "d", "e", "f" }, tmp("hn.txt"))
vim.cmd("checktime")
check(vim.api.nvim_buf_get_lines(0, 1, 2, false)[1] == "BBBB", "second reload did not happen")

local l2 = A.list({ include_stale = true })
check(#l2 == 1 and l2[1].stale, "expected stale after the annotated line changed")
check(A.count() == 0, "stale items must not count as pending")

vim.cmd("e!")
check(#A.list({ include_stale = true }) == 1, "lost on :e!")
pass("reload survival")
