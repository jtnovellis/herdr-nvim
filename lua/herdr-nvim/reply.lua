-- Where the agent's answer shows up.
--
-- A float that does not take focus: you asked a question in the middle of
-- doing something, and the answer arriving should not move your cursor out of
-- the buffer you are working in. It reports what the agent is doing until
-- there are words to show, then shows them.
local Reply = {}

local GROUP = "HerdrNvimReply"
local MAX_HEIGHT = 20
local MIN_HEIGHT = 3

local state = {}

local function hn()
  return require("herdr-nvim")
end

-- The same vocabulary the statusline uses, spelled out here because a footer
-- has room for words.
local STATUS = {
  working = "◑ working",
  blocked = "⏸ waiting for you in its pane",
  done = "✓ done",
  idle = "✓ done",
  unknown = "…",
}

function Reply.is_open()
  return state.win ~= nil and vim.api.nvim_win_is_valid(state.win)
end

--- Close the reply window. `opts.keep_follow` leaves the transcript tail
--- alone, for the caller that is about to start a new one.
---
--- Dismissing the window really does mean stop reading: nobody is looking.
--- But `Reply.open` also passes through here to replace an old window, and
--- unfollowing there would cancel the follow its caller had just started --
--- the window would open and then never fill.
function Reply.close(opts)
  pcall(vim.api.nvim_del_augroup_by_name, GROUP)
  if Reply.is_open() then
    pcall(vim.api.nvim_win_close, state.win, true)
  end
  state = {}
  if not (opts or {}).keep_follow then
    pcall(function()
      require("herdr-nvim.agent").unfollow()
    end)
  end
end

local function footer()
  local agent = require("herdr-nvim.agent").status(state.pane_id)
  local label = agent and (STATUS[agent.status] or agent.status) or "◑ working"
  local hunks = require("herdr-nvim.review").count()
  if hunks > 0 then
    label = label .. " · " .. hunks .. " edit" .. (hunks == 1 and "" or "s") .. " to review"
  end
  return " " .. label .. " · q close "
end

local function render()
  if not Reply.is_open() then
    return
  end
  local lines = {}
  if #state.turns == 0 then
    table.insert(lines, "…")
  else
    for i, turn in ipairs(state.turns) do
      if i > 1 then
        table.insert(lines, "")
      end
      vim.list_extend(lines, vim.split(turn, "\n", { plain = true }))
    end
  end
  vim.bo[state.buf].modifiable = true
  vim.api.nvim_buf_set_lines(state.buf, 0, -1, false, lines)
  vim.bo[state.buf].modifiable = false

  local want = math.max(MIN_HEIGHT, math.min(#lines, MAX_HEIGHT))
  pcall(vim.api.nvim_win_set_height, state.win, want)
  pcall(vim.api.nvim_win_set_config, state.win, { footer = footer(), footer_pos = "center" })
  -- Keep the newest text in view without moving the user's own cursor: the
  -- window is not focused, so scrolling it means asking it to scroll itself.
  pcall(vim.api.nvim_win_call, state.win, function()
    vim.cmd("normal! G")
  end)
end

--- Show the answer to a question just asked of `opts.agent` in `opts.pane_id`.
function Reply.open(opts)
  local uis = vim.g.herdr_nvim_test_uis or #vim.api.nvim_list_uis()
  if uis == 0 then
    return false
  end
  -- `Agent.follow` already retires any previous tail, so this must not.
  Reply.close({ keep_follow = true })
  state = {
    pane_id = opts.pane_id,
    agent = opts.agent,
    turns = {},
  }
  local width = math.max(50, math.min(math.floor(vim.o.columns * 0.7), 90))
  state.buf = vim.api.nvim_create_buf(false, true)
  vim.bo[state.buf].bufhidden = "wipe"
  vim.bo[state.buf].buftype = "nofile"
  vim.bo[state.buf].filetype = "markdown"
  -- `false`: the answer arrives while you carry on working.
  state.win = vim.api.nvim_open_win(state.buf, false, {
    relative = "editor",
    width = width,
    height = MIN_HEIGHT,
    row = math.max(1, vim.o.lines - MIN_HEIGHT - 4),
    col = math.floor((vim.o.columns - width) / 2),
    style = "minimal",
    border = "rounded",
    title = " " .. (opts.agent or "agent") .. " ",
    title_pos = "center",
    footer = footer(),
    footer_pos = "center",
    focusable = true,
    noautocmd = true,
  })
  vim.wo[state.win].wrap = true
  vim.wo[state.win].linebreak = true

  for _, lhs in ipairs({ "q", "<Esc>" }) do
    vim.keymap.set("n", lhs, function()
      Reply.close()
    end, { buffer = state.buf, nowait = true, silent = true })
  end

  local group = vim.api.nvim_create_augroup(GROUP, { clear = true })
  vim.api.nvim_create_autocmd("User", {
    group = group,
    pattern = "HerdrNvimAgentReply",
    callback = function(ev)
      local data = ev.data or {}
      if state.pane_id and data.pane_id and data.pane_id ~= state.pane_id then
        return
      end
      vim.list_extend(state.turns, data.reply or {})
      render()
    end,
  })
  vim.api.nvim_create_autocmd("User", {
    group = group,
    pattern = { "HerdrNvimAgentStatus", "HerdrNvimReviewChanged" },
    callback = render,
  })
  render()
  return true
end

--- Move into the reply window to scroll or copy from it.
function Reply.focus()
  if not Reply.is_open() then
    hn().notify("no reply open")
    return false
  end
  vim.api.nvim_set_current_win(state.win)
  return true
end

--- Test seam.
function Reply.debug()
  return { open = Reply.is_open(), turns = state.turns and #state.turns or 0 }
end

return Reply
