vim.ui.input = function(o, cb)
  cb(o.default ~= "" and "edited" or "first")
end
edit("hn.txt")

local A = require("herdr-nvim.annotations")
local H = require("herdr-nvim")

vim.api.nvim_win_set_cursor(0, { 1, 0 })
H.annotate({ line1 = 1, line2 = 3 })
H.annotate({ line1 = 2, line2 = 4 })

local l = A.list()
check(#l == 1, "overlapping range duplicated: " .. #l)
check(l[1].text == "edited", "overlap should edit: " .. l[1].text)
pass("overlap edits")
