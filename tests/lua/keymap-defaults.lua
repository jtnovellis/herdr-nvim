-- Every default this plugin declares must actually resolve.
--
-- `]a` / `[a` did not, from 0.1.0 to 0.2.3: Neovim ships both as default mappings
-- that walk the argument list, `apply_keymaps` never clobbers a key someone
-- else holds, and nothing reported the loss. The prelude has already sourced
-- plugin/herdr-nvim.lua, so what is asserted first is the state a real user
-- gets -- and under `nvim --clean` the only competition is Neovim's own
-- built-in mappings, so a non-empty `_skipped` means a default collides with
-- one.
local H = require("herdr-nvim")

local lost = {}
for _, m in ipairs(H._skipped) do
  table.insert(lost, ("%s (%s) held by %q"):format(m.lhs, m.mode, m.held_by))
end
check(#lost == 0, "defaults lost to Neovim's own mappings: " .. table.concat(lost, ", "))

for lhs, want in pairs({ ["]n"] = "next annotation", ["[n"] = "previous annotation" }) do
  local desc = vim.fn.maparg(lhs, "n", false, true).desc
  check(desc == "herdr-nvim: " .. want, ("%s is %q, not ours"):format(lhs, tostring(desc)))
end

-- The ledger has to fill when a key really is taken, or checkhealth reports
-- nothing and the next collision hides just as well as this one did.
vim.keymap.set("n", "]r", function() end, { desc = "someone else" })
H.setup({ keymaps = true, notify = false })
local found
for _, m in ipairs(H._skipped) do
  if m.lhs == "]r" then
    found = m
  end
end
check(found ~= nil, "a taken key was not recorded as skipped")
check(found.held_by == "someone else", "the skip record lost what holds the key")
check(found.want == "next agent edit", "the skip record lost what the key was for")
check(vim.fn.maparg("]r", "n", false, true).desc == "someone else", "a user mapping was clobbered")

H.setup({ keymaps = "force", notify = false })
check(vim.fn.maparg("]r", "n", false, true).desc == "herdr-nvim: next agent edit", "force did not override")
check(#H._skipped == 0, "force left a skip recorded")

-- `]a` / `[a` are retired: an in-session upgrade drops the pair we used to set,
-- and leaves alone a mapping that has since claimed the key.
vim.keymap.set("n", "]a", function() end, { desc = "herdr-nvim: next annotation" })
H.setup({ keymaps = true, notify = false })
check(vim.fn.maparg("]a", "n") == "", "a retired default of ours was not removed")

vim.keymap.set("n", "[a", function() end, { desc = "mine" })
H.setup({ keymaps = true, notify = false })
check(vim.fn.maparg("[a", "n", false, true).desc == "mine", "retirement clobbered a user mapping")

pass("keymap defaults resolve, and a lost one is recorded")
