-- Cancelling sends nothing and cleans up; losing focus keeps what you typed.
vim.g.herdr_nvim_test_uis = 1
local hn = require("herdr-nvim")
local ask = require("herdr-nvim.ask")
local bridge = require("herdr-nvim.bridge")
vim.notify = function() end -- an empty submit warns; keep the suite output clean

local sent = 0
bridge.run = function()
  sent = sent + 1
end

edit("hn.txt")
local src = vim.api.nvim_get_current_buf()
local ns = vim.api.nvim_create_namespace("herdr-nvim-ask")

hn.ask()
check(ask.is_open(), "composer did not open")
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "half a thought" })
ask.close() -- what <Esc> / q / <C-c> do

check(not ask.is_open(), "composer stayed open")
check(sent == 0, "cancelling sent something")
check(#vim.api.nvim_buf_get_extmarks(src, ns, 0, -1, {}) == 0, "the range mark outlived the composer")
check(ask.draft() == nil, "an explicit cancel kept the draft")
check(hn._inflight ~= true, "_inflight was left held")

-- Focus wandering off is not a cancel: the text comes back next time.
hn.ask()
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "worth keeping" })
ask.close({ keep_draft = true })
check(ask.draft() == "worth keeping", "draft lost: " .. tostring(ask.draft()))
hn.ask()
check(table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n") == "worth keeping", "draft was not restored")
ask.close()

-- An empty message is refused and leaves the box open to fix.
hn.ask()
ask.submit()
check(ask.is_open(), "an empty submit closed the composer")
check(sent == 0, "an empty message was sent")
ask.close()
-- A send already in flight leaves the box open with the text intact.
hn.ask()
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "do not eat this" })
hn._inflight = true
ask.submit()
check(ask.is_open(), "a busy send closed the composer")
check(table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n") == "do not eat this", "the message was lost")
check(sent == 0, "sent while another send was in flight")
hn._inflight = false
ask.close()
pass("ask cancel")
