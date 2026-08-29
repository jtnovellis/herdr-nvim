-- The mapping set, including the regression guard for the <C-u> range wipe.
local hn = require("herdr-nvim")

local function desc(lhs, mode)
  local m = vim.fn.maparg(lhs, mode, false, true)
  return type(m) == "table" and m.desc or nil
end
local function rhs(lhs, mode)
  local m = vim.fn.maparg(lhs, mode, false, true)
  return type(m) == "table" and m.rhs or nil
end

check((desc("<leader>ac", "n") or ""):find("ask"), "n <leader>ac: " .. tostring(desc("<leader>ac", "n")))
check((desc("<leader>ar", "n") or ""):find("follow up"), "n <leader>ar: " .. tostring(desc("<leader>ar", "n")))
check((desc("<leader>aa", "n") or ""):find("comment"), "n <leader>aa: " .. tostring(desc("<leader>aa", "n")))

-- ":HerdrAsk<CR>", never ":<C-u>HerdrAsk<CR>": a <C-u> wipes the '<,'> range.
check(rhs("<leader>ac", "x") == ":HerdrAsk<CR>", "x <leader>ac: " .. tostring(rhs("<leader>ac", "x")))
check(rhs("<leader>aa", "x") == ":HerdrAnnotate<CR>", "x <leader>aa: " .. tostring(rhs("<leader>aa", "x")))

for _, name in ipairs({ "HerdrAsk", "HerdrReply", "HerdrAskTarget" }) do
  check(vim.fn.exists(":" .. name) == 2, name .. " is not defined")
end

-- keymaps = false removes ours and leaves a user's alone.
vim.keymap.set("n", "<leader>ac", "<Nop>", { desc = "mine" })
hn.setup({ keymaps = false })
check(desc("<leader>ac", "n") == "mine", "a user mapping was deleted: " .. tostring(desc("<leader>ac", "n")))
check(rhs("<leader>ac", "x") == nil, "x <leader>ac survived keymaps = false")
check(desc("<leader>ar", "n") == nil, "<leader>ar survived keymaps = false")
check(desc("<leader>aa", "n") == nil, "<leader>aa survived keymaps = false")
pass("ask keymaps")
