edit("hn.txt")

local fired = 0
vim.api.nvim_create_autocmd("User", {
  pattern = "HerdrNvimAnnotationsChanged",
  callback = function()
    fired = fired + 1
  end,
})

local A = require("herdr-nvim.annotations")
A.add(0, 0, 0, "x")
A.add(0, 2, 2, "y")

vim.wait(200, function()
  return fired > 0
end, 10)
check(fired >= 1, "User autocmd not fired")
check(require("herdr-nvim").statusline() == "● 2", "statusline count")

A.mark_delivered()
check(require("herdr-nvim").statusline() == "", "delivered items still pending")
check(A.total() == 2, "delivered items were dropped")
check(A.reap_stale() == 2 and A.total() == 0, "reap did not remove delivered items")
pass("user autocmd + statusline")
