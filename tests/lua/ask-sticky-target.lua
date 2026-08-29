-- A remembered pane that has gone is forgotten and resolved again, once.
vim.g.herdr_nvim_test_uis = 1
local hn = require("herdr-nvim")
local ask = require("herdr-nvim.ask")
local bridge = require("herdr-nvim.bridge")
vim.notify = function() end -- errors are expected here; keep the suite output clean

local calls = {}
local answers = {}
bridge.run = function(_, args, stdin, cb)
  table.insert(calls, { args = table.concat(args, " "), payload = vim.json.decode(stdin) })
  local answer = table.remove(answers, 1)
  if answer then
    cb(true, answer)
  end
end

edit("hn.txt")

-- First ask: nothing remembered yet, so no --target.
answers = { { ok = true, via = "agent.prompt", target = { pane_id = "w1:p2", agent = "claude" } } }
hn.ask({ message = "first?" })
check(#calls == 1, "expected 1 call, got " .. #calls)
check(not calls[1].args:find("--target"), "first ask sent a target: " .. calls[1].args)
check(ask.target().pane_id == "w1:p2", "target not remembered")

-- Second ask: goes straight to the remembered pane.
answers = { { ok = true, via = "agent.prompt", target = { pane_id = "w1:p2", agent = "claude" } } }
hn.ask({ message = "second?" })
check(calls[2].args:find("--target w1:p2", 1, true) ~= nil, "second ask argv: " .. calls[2].args)

-- Third: the pane is gone. Forget it, retry once without a target, and stop.
answers = {
  { ok = false, code = "agent_not_found", error = "gone" },
  { ok = true, via = "agent.prompt", target = { pane_id = "w1:p9", agent = "codex" } },
}
hn.ask({ message = "third?" })
check(#calls == 4, "expected 4 calls total, got " .. #calls)
check(calls[3].args:find("--target w1:p2", 1, true) ~= nil, "call 3 argv: " .. calls[3].args)
check(not calls[4].args:find("--target"), "the retry kept the dead target: " .. calls[4].args)
check(ask.target().pane_id == "w1:p9", "target not updated to the new pane")

-- A second failure must not loop: the retry guard stops after one attempt.
answers =
  { { ok = false, code = "agent_not_found", error = "gone" }, { ok = false, code = "agent_not_found", error = "gone" } }
hn.ask({ message = "fourth?" })
check(#calls == 6, "retry looped: " .. #calls .. " calls")
check(ask.target() == nil, "a dead target was kept")

-- agent_blocked is the right agent at an approval prompt: keep it.
ask.set_target({ pane_id = "w1:p2", agent = "claude" })
answers = { { ok = false, code = "agent_blocked", error = "blocked" } }
hn.ask({ message = "fifth?" })
check(ask.target() ~= nil and ask.target().pane_id == "w1:p2", "agent_blocked forgot the target")
pass("ask sticky target")
