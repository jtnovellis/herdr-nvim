-- What actually goes over the wire: argv, the message, and the code range.
vim.g.herdr_nvim_test_uis = 1
local ask = require("herdr-nvim.ask")
local bridge = require("herdr-nvim.bridge")
require("herdr-nvim").config.notify = false -- keep the suite output to ok/FAIL

local calls = {}
bridge.run = function(_, args, stdin, cb)
  table.insert(calls, { args = args, payload = stdin and vim.json.decode(stdin) or nil, cb = cb })
end

edit("hn.txt")
vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("Vj ac", true, false, true), "x", false)
check(ask.is_open(), "composer did not open")
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "why two?", "", "and here?" })
ask.submit()

check(#calls == 1, "expected 1 call, got " .. #calls)
check(calls[1].args[1] == "ask", "argv[1] = " .. tostring(calls[1].args[1]))
local p = calls[1].payload
check(p.message == "why two?\n\nand here?", "message: " .. vim.inspect(p.message))
check(p.selection.line == 1 and p.selection.end_line == 2, "range " .. p.selection.line .. "-" .. p.selection.end_line)
check(p.selection.code == "a\nb", "code: " .. vim.inspect(p.selection.code))
check(p.selection.file:match("hn%.txt$") ~= nil, "file: " .. p.selection.file)
check(p.selection.modified == false, "modified: " .. tostring(p.selection.modified))
check(p.cwd ~= nil and p.cwd ~= "", "cwd missing")

-- A successful send remembers the agent and drops the draft.
calls[1].cb(true, { ok = true, via = "agent.prompt", target = { pane_id = "w1:p2", agent = "claude" } })
check(ask.target() ~= nil and ask.target().pane_id == "w1:p2", "target not remembered")
check(ask.draft() == nil, "draft survived a successful send")

-- A reply carries no code and goes to the remembered pane.
require("herdr-nvim").reply({ message = "and the fallback?" })
check(#calls == 2, "expected 2 calls, got " .. #calls)
check(calls[2].payload.selection == nil, "reply attached a selection")
check(calls[2].payload.message == "and the fallback?", "reply message: " .. tostring(calls[2].payload.message))
local joined = table.concat(calls[2].args, " ")
check(joined:find("--target w1:p2", 1, true) ~= nil, "reply argv: " .. joined)
pass("ask payload")
