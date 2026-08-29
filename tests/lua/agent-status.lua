-- The status Herdr pushes in must reach a listener with its payload attached,
-- and show up in the statusline. `HerdrNvimAnnotationsChanged` carries no
-- data, so this is the first event where the payload itself is the contract.
local Agent = require("herdr-nvim.agent")

local seen = {}
vim.api.nvim_create_autocmd("User", {
  pattern = "HerdrNvimAgentStatus",
  callback = function(ev)
    table.insert(seen, ev.data)
  end,
})

-- Exactly what the plugin binary sends over the daemon socket: a JSON string.
Agent.on_status('{"pane_id":"wF:pC","agent":"claude","status":"working"}')
vim.wait(200, function()
  return #seen > 0
end, 10)

check(#seen == 1, "no HerdrNvimAgentStatus fired")
check(seen[1].pane_id == "wF:pC", "payload lost pane_id")
check(seen[1].status == "working", "payload lost status")
check(Agent.status("wF:pC").agent == "claude", "status not remembered")

-- The same status again is not news; a listener should not be woken for it.
Agent.on_status('{"pane_id":"wF:pC","agent":"claude","status":"working"}')
vim.wait(50)
check(#seen == 1, "an unchanged status fired an event")

Agent.on_status('{"pane_id":"wF:pC","agent":"claude","status":"idle"}')
vim.wait(200, function()
  return #seen > 1
end, 10)
check(#seen == 2, "a changed status did not fire")

-- Malformed input arrives over an RPC channel where throwing would surface to
-- Herdr as a failed event hook, so it must be swallowed.
check(Agent.on_status("not json") == 0, "malformed payload was not rejected")
check(Agent.on_status('{"no_pane":1}') == 0, "payload without a pane was accepted")

-- Statusline: working shows, idle does not (an idle agent is the resting
-- state and does not deserve permanent furniture).
Agent.on_status('{"pane_id":"wF:pC","agent":"claude","status":"working"}')
local line = require("herdr-nvim").statusline()
check(line:find("claude", 1, true) ~= nil, "statusline does not name the agent: " .. line)
Agent.on_status('{"pane_id":"wF:pC","agent":"claude","status":"idle"}')
check(require("herdr-nvim").statusline() == "", "idle agent still in the statusline")

pass("agent status")
