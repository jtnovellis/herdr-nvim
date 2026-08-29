vim.ui.input = function(_, cb)
  cb("note")
end
edit("hn.txt")

vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("Vj aa", true, false, true), "x", false)
local l = require("herdr-nvim.annotations").list()
check(vim.fn.mode() == "n", "still in visual mode: " .. vim.fn.mode())
check(#l == 1, "expected 1 annotation, got " .. #l)
check(l[1].row == 0 and l[1].end_row == 1, "rows " .. tostring(l[1].row) .. "-" .. tostring(l[1].end_row))
check(require("herdr-nvim").statusline() == "● 1", "statusline: " .. require("herdr-nvim").statusline())
pass("visual annotate")
