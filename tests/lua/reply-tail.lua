-- Reading the agent's answer back into Neovim: the tail turns appended
-- transcript lines into reply text and edits, and the reply view shows them.
local Agent = require("herdr-nvim.agent")
local Reply = require("herdr-nvim.reply")

vim.g.herdr_nvim_test_uis = 1 -- the view refuses to open with no UI

-- An agent kind Herdr tracks no transcript for must not be followed, and must
-- not be an error: that agent's own pane is still the place to read.
check(Agent.follow("wF:pC", nil) == false, "followed a nil session")
check(Agent.follow("wF:pC", { path = "" }) == false, "followed an empty path")
check(Agent.following() == nil, "a refused follow left state behind")

-- The view renders what the tail publishes, without stealing focus.
local before = vim.api.nvim_get_current_win()
check(Reply.open({ agent = "claude", pane_id = "wF:pC" }), "reply view did not open")
check(vim.api.nvim_get_current_win() == before, "the reply view stole focus")

vim.api.nvim_exec_autocmds("User", {
  pattern = "HerdrNvimAgentReply",
  modeline = false,
  data = { pane_id = "wF:pC", reply = { "The error is swallowed by the closure." }, edits = {} },
})
vim.wait(200, function()
  return Reply.debug().turns > 0
end, 10)
check(Reply.debug().turns == 1, "reply turn not recorded")

-- A reply from a different pane belongs to a different question.
vim.api.nvim_exec_autocmds("User", {
  pattern = "HerdrNvimAgentReply",
  modeline = false,
  data = { pane_id = "wF:pOTHER", reply = { "not yours" }, edits = {} },
})
vim.wait(50)
check(Reply.debug().turns == 1, "a reply from another pane was shown")

Reply.close()
check(not Reply.debug().open, "reply view did not close")

-- End to end through the real binary: append to a transcript and check that
-- what comes back is the prose and the edit, not the tool-call noise.
if have_binary() then
  local path = tmp("session.jsonl")
  local first = '{"message":{"role":"assistant","content":[{"type":"text","text":"first turn"}]},'
    .. '"type":"assistant","timestamp":"2026-08-29T09:00:00.000Z"}\n'
  local f = assert(io.open(path, "w"))
  f:write(first)
  f:close()
  local offset = #first

  f = assert(io.open(path, "a"))
  f:write(
    '{"message":{"role":"assistant","content":[{"type":"text","text":"second turn"}]},'
      .. '"type":"assistant","timestamp":"2026-08-29T09:00:10.000Z"}\n'
  )
  f:write(
    '{"message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit",'
      .. '"input":{"file_path":"/repo/a.rs","old_string":"a","new_string":"b"}}]},'
      .. '"type":"assistant","timestamp":"2026-08-29T09:00:20.000Z"}\n'
  )
  f:close()

  local done, result = false, nil
  require("herdr-nvim.bridge").run(
    require("herdr-nvim").config,
    { "tail", "--path", path, "--agent", "claude", "--from", tostring(offset) },
    nil,
    function(ok, res)
      done, result = true, ok and res or nil
    end
  )
  vim.wait(5000, function()
    return done
  end, 20)
  check(result ~= nil and result.ok, "tail call failed")
  check(#result.reply == 1, "expected only the appended turn, got " .. #result.reply)
  check(result.reply[1] == "second turn", "wrong turn: " .. tostring(result.reply[1]))
  check(#result.edits == 1, "edit not returned")
  check(result.edits[1].old == "a" and result.edits[1].new == "b", "edit payload lost")
  check(result.offset > offset, "offset did not advance")
end

pass("reply tail")
