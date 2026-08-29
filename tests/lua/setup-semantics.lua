vim.keymap.set("n", "<leader>al", function() end, { desc = "mine" })

local H = require("herdr-nvim")
H.setup({ prompt = "X: " })
H.setup({ notify = false })
check(H.config.prompt == "X: " and H.config.notify == false, "second setup reset options")
check(vim.fn.maparg("<leader>al", "n", false, true).desc == "mine", "user mapping clobbered")
check(vim.fn.maparg("<leader>ac", "n", false, true).desc ~= nil, "our mapping missing")

H.setup({ keymaps = false })
check(vim.fn.maparg("<leader>al", "n", false, true).desc == "mine", "keymaps=false removed the user mapping")
check(vim.fn.maparg("<leader>ac", "n") == "", "keymaps=false left our mapping")

check(H.quit_guard_should_intercept({ uis = 1, windows = 1 }), "guard should intercept")
check(not H.quit_guard_should_intercept({ uis = 0, windows = 1 }), "no UI: no intercept")
check(not H.quit_guard_should_intercept({ uis = 1, windows = 2 }), "two windows: no intercept")
pass("setup semantics + quit guard helper")
