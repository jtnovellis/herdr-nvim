if vim.g.loaded_herdr_nvim then
  return
end
vim.g.loaded_herdr_nvim = true

if vim.fn.has("nvim-0.11") == 0 then
  vim.notify("herdr-nvim requires Neovim 0.11 or newer", vim.log.levels.ERROR)
  return
end

local function hn()
  return require("herdr-nvim")
end

local cmd = vim.api.nvim_create_user_command
cmd("HerdrAsk", function(o)
  hn().ask({
    line1 = o.range > 0 and o.line1 or nil,
    line2 = o.range > 0 and o.line2 or nil,
    message = o.args ~= "" and o.args or nil,
    force = o.bang,
  })
end, {
  range = true,
  bang = true,
  nargs = "*",
  desc = "herdr-nvim: ask the agent about this line or range (! forces a blocked agent)",
})
cmd("HerdrReply", function(o)
  hn().reply({ message = o.args ~= "" and o.args or nil, force = o.bang })
end, {
  bang = true,
  nargs = "*",
  desc = "herdr-nvim: follow up with the agent you last asked",
})
cmd("HerdrAskTarget", function(o)
  hn().ask_target({ clear = o.bang })
end, { bang = true, desc = "herdr-nvim: choose the agent :HerdrAsk talks to (! forgets it)" })
cmd("HerdrAnnotate", function(o)
  hn().annotate(o.range > 0 and { line1 = o.line1, line2 = o.line2 } or nil)
end, { range = true, desc = "herdr-nvim: annotate the current line or range" })
cmd("HerdrAnnotations", function()
  hn().list()
end, { desc = "herdr-nvim: list annotations" })
cmd("HerdrPaste", function(o)
  hn().paste({ force = o.bang })
end, { bang = true, desc = "herdr-nvim: paste annotations into the agent's input (! forces a blocked agent)" })
cmd("HerdrSend", function(o)
  hn().send({ force = o.bang })
end, { bang = true, desc = "herdr-nvim: send annotations to the agent (! forces a blocked agent)" })
cmd("HerdrPreview", function()
  hn().preview()
end, { desc = "herdr-nvim: preview the prompt that would be sent" })
cmd("HerdrClear", function()
  hn().clear()
end, { desc = "herdr-nvim: forget all annotations" })
cmd("HerdrAgents", function()
  hn().agents()
end, { desc = "herdr-nvim: list agents visible from here" })
cmd("HerdrPickFile", function()
  hn().pick_file()
end, { desc = "herdr-nvim: pick a file the agent touched" })
cmd("HerdrNext", function()
  hn().next()
end, { desc = "herdr-nvim: next annotation" })
cmd("HerdrNextEdit", function()
  hn().next_hunk()
end, { desc = "herdr-nvim: jump to the next edit the agent made" })
cmd("HerdrPrevEdit", function()
  hn().prev_hunk()
end, { desc = "herdr-nvim: jump to the previous edit the agent made" })
cmd("HerdrRevertEdit", function()
  hn().revert_hunk()
end, { desc = "herdr-nvim: undo the agent's edit under the cursor" })
cmd("HerdrKeepEdit", function()
  hn().keep_hunk()
end, { desc = "herdr-nvim: keep the agent's edit under the cursor" })
cmd("HerdrKeepEdits", function()
  hn().keep_all_hunks()
end, { desc = "herdr-nvim: clear every agent-edit mark in this buffer" })
cmd("HerdrReplyView", function()
  require("herdr-nvim.reply").focus()
end, { desc = "herdr-nvim: move into the agent's reply window" })
cmd("HerdrPrev", function()
  hn().prev()
end, { desc = "herdr-nvim: previous annotation" })

-- Sidebar daemons: `:detach` leaves the sidebar without stopping the daemon.
if vim.env.HERDR_NVIM_DAEMON == "1" and vim.fn.exists(":detach") == 2 then
  cmd("HerdrDetach", function()
    vim.cmd("detach")
  end, { desc = "herdr-nvim: detach this sidebar client" })
end

if not vim.g.herdr_nvim_no_defaults and not hn()._setup_done then
  hn().setup()
end
