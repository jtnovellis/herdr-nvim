edit("hn.txt")
local A = require("herdr-nvim.annotations")

A.add(0, 0, 0, "first line")
require("herdr-nvim.reload").start({ debounce_ms = 50, force_without_ui = true })

vim.fn.writefile({ "a", "b", "c", "d", "e", "appended" }, tmp("hn.txt"))
local ok = vim.wait(2000, function()
  return vim.api.nvim_buf_line_count(0) == 6
end, 50)
check(ok, "watcher did not reload the buffer")
check(#A.list() == 1 and not A.list()[1].stale, "annotation lost on watcher reload")

vim.api.nvim_buf_set_lines(0, 5, 6, false, { "local edit" })
vim.fn.writefile({ "a", "b", "c", "d", "e", "disk" }, tmp("hn.txt"))
vim.wait(500, function()
  return false
end, 50)
check(vim.api.nvim_buf_get_lines(0, 5, 6, false)[1] == "local edit", "modified buffer was clobbered")
check(vim.b.herdr_nvim_stale == true, "modified buffer not flagged stale")
pass("reload watcher")
