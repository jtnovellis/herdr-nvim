-- One message to the agent, sent the moment you finish typing it: the lines you
-- selected, then what you want to say about them.
--
-- The reply lands in the agent's own Herdr pane, not here. That is not a
-- shortcut: `agent.prompt` types a string into the agent's PTY and answers with
-- lifecycle state, never text, so Herdr has no reply channel to stream back.
local Ask = {}

local bridge = require("herdr-nvim.bridge")

local ns = vim.api.nvim_create_namespace("herdr-nvim-ask")
local GROUP = "HerdrNvimAsk"
local MIN_HEIGHT, MAX_HEIGHT = 3, 12

-- Codes that mean "the pane we remembered is gone": forget it and resolve once
-- more rather than making the user rediscover an agent they never chose.
local STALE = {
  agent_not_found = true,
  agent_not_running = true,
  agent_pane_not_found = true,
  unknown_target = true,
}

local state = {}
local last_target = nil -- { pane_id, agent }
local draft = nil -- text kept when the box loses focus, or a send fails

local function hn()
  return require("herdr-nvim")
end

local function notify(msg, level)
  hn().notify(msg, level)
end

function Ask.is_open()
  return state.win ~= nil and vim.api.nvim_win_is_valid(state.win)
end

--- `opts.keep_draft` stashes the text for the next open. A stray <C-w>w must
--- not silently destroy a paragraph you just typed.
function Ask.close(opts)
  if (opts or {}).keep_draft and Ask.is_open() then
    local text = vim.trim(table.concat(vim.api.nvim_buf_get_lines(state.buf, 0, -1, false), "\n"))
    draft = text ~= "" and text or nil
  else
    draft = nil
  end
  pcall(vim.api.nvim_del_augroup_by_name, GROUP)
  if state.mark and state.src_buf and vim.api.nvim_buf_is_valid(state.src_buf) then
    pcall(vim.api.nvim_buf_del_extmark, state.src_buf, ns, state.mark)
  end
  if state.win and vim.api.nvim_win_is_valid(state.win) then
    pcall(vim.api.nvim_win_close, state.win, true)
  end
  state = {}
end

--- Text kept from a box that lost focus or a send that failed, or nil.
function Ask.draft()
  return draft
end

--- The remembered agent, or nil. `{ pane_id, agent }`.
function Ask.target()
  return last_target
end

function Ask.forget_target()
  last_target = nil
end

--- Aim future asks at a pane without going through the picker.
function Ask.set_target(target)
  last_target = target
end

--- "src/send.rs:12-14", relative to the cwd when it lives under it.
function Ask.location(ctx)
  if not ctx then
    return "no code attached"
  end
  local where = vim.fn.fnamemodify(ctx.file, ":~:.") .. ":" .. (ctx.srow + 1)
  if ctx.erow > ctx.srow then
    where = where .. "-" .. (ctx.erow + 1)
  end
  return where
end

--- Elide from the left: the line numbers matter more than the directories.
local function title_for(ctx, width)
  local who = (last_target and last_target.agent) or "agent"
  local title = " ask " .. who .. " · " .. Ask.location(ctx) .. " "
  local max, n = width - 2, vim.fn.strchars(title)
  if n > max then
    title = "…" .. vim.fn.strcharpart(title, n - (max - 1))
  end
  return title
end

--- Mark the lines being asked about, so the float is obviously *about* them.
local function highlight_range(ctx)
  if not (ctx and vim.api.nvim_buf_is_valid(ctx.buf) and vim.api.nvim_buf_is_loaded(ctx.buf)) then
    return
  end
  local last = vim.api.nvim_buf_line_count(ctx.buf) - 1
  local srow = math.max(0, math.min(ctx.srow, last))
  local erow = math.max(srow, math.min(ctx.erow, last))
  local opts = { line_hl_group = "HerdrNvimAskRange", priority = 130 }
  if erow > srow then
    opts.hl_group = "HerdrNvimAskRange"
    opts.hl_eol = true
    opts.end_right_gravity = false
    if erow < last then
      opts.end_row, opts.end_col = erow + 1, 0
    else
      local line = vim.api.nvim_buf_get_lines(ctx.buf, erow, erow + 1, false)[1] or ""
      opts.end_row, opts.end_col = erow, #line
    end
  end
  state.src_buf = ctx.buf
  state.mark = vim.api.nvim_buf_set_extmark(ctx.buf, ns, srow, 0, opts)
end

--- Open the composer. `opts = { ctx = { buf, srow, erow, file } | nil, force }`.
--- The range must already be captured: `herdr-nvim.current_range` is what
--- leaves visual mode, and that has to happen before this window takes focus.
function Ask.open(opts)
  opts = opts or {}
  local seed = draft -- Ask.close() below clears it
  Ask.close()
  -- The picker defers to UIEnter because Herdr can invoke it while the daemon
  -- is still headless. Ask is only ever triggered by a human inside Neovim, so
  -- no UI means there is nothing to type into.
  local uis = vim.g.herdr_nvim_test_uis or #vim.api.nvim_list_uis()
  if uis == 0 then
    notify("open the sidebar first: there is no UI to type into", vim.log.levels.WARN)
    return
  end

  local config = hn().config
  local send_key = config.ask_send_key or "<C-s>"
  local width = math.max(50, math.min(math.floor(vim.o.columns * 0.7), 90))
  local height = math.max(MIN_HEIGHT, math.min(config.ask_height or 5, MAX_HEIGHT))
  local row = math.max(1, math.floor((vim.o.lines - height) / 2) - 2)
  local col = math.floor((vim.o.columns - width) / 2)

  state.ctx = opts.ctx
  state.force = opts.force
  state.height = height
  highlight_range(opts.ctx)

  state.buf = vim.api.nvim_create_buf(false, true)
  vim.bo[state.buf].bufhidden = "wipe"
  vim.bo[state.buf].buftype = "nofile"
  vim.bo[state.buf].filetype = "markdown"
  state.win = vim.api.nvim_open_win(state.buf, true, {
    relative = "editor",
    width = width,
    height = height,
    row = row,
    col = col,
    style = "minimal",
    border = "rounded",
    title = title_for(opts.ctx, width),
    title_pos = "center",
    footer = " " .. send_key .. "/⏎ send · <Esc> cancel ",
    footer_pos = "center",
  })
  vim.wo[state.win].wrap = true
  vim.wo[state.win].linebreak = true
  if seed then
    vim.api.nvim_buf_set_lines(state.buf, 0, -1, false, vim.split(seed, "\n", { plain = true }))
    vim.api.nvim_win_set_cursor(state.win, { vim.api.nvim_buf_line_count(state.buf), 0 })
  end

  local function map(mode, lhs, fn)
    vim.keymap.set(mode, lhs, fn, { buffer = state.buf, nowait = true, silent = true })
  end
  map({ "i", "n" }, send_key, Ask.submit)
  map("n", "<CR>", Ask.submit)
  -- <Esc> is left alone in insert mode so it still means "leave insert": from
  -- there a second <Esc> cancels. <C-s> can be eaten by tty flow control
  -- (stty -ixon), which is why <CR> in normal mode always works too.
  map("n", "<Esc>", Ask.close)
  map({ "i", "n" }, "<C-c>", Ask.close)
  map("n", "q", Ask.close)

  local group = vim.api.nvim_create_augroup(GROUP, { clear = true })
  vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI" }, {
    group = group,
    buffer = state.buf,
    callback = function()
      if not Ask.is_open() then
        return
      end
      local want = math.max(state.height, math.min(vim.api.nvim_buf_line_count(state.buf), MAX_HEIGHT))
      if want ~= vim.api.nvim_win_get_height(state.win) then
        pcall(vim.api.nvim_win_set_height, state.win, want)
      end
    end,
  })
  vim.api.nvim_create_autocmd("WinLeave", {
    group = group,
    buffer = state.buf,
    callback = function()
      vim.schedule(function()
        if Ask.is_open() and vim.api.nvim_get_current_win() ~= state.win then
          Ask.close({ keep_draft = true })
        end
      end)
    end,
  })
  vim.cmd("startinsert")
end

function Ask.submit()
  if not Ask.is_open() then
    return
  end
  -- Checked before closing: a box that cannot send yet should stay open with
  -- what you typed still in it.
  if hn()._inflight then
    notify("a send is already in progress", vim.log.levels.WARN)
    return
  end
  local message = vim.trim(table.concat(vim.api.nvim_buf_get_lines(state.buf, 0, -1, false), "\n"))
  if message == "" then
    notify("nothing to ask: the message is empty", vim.log.levels.WARN)
    return
  end
  local ctx, force = state.ctx, state.force
  Ask.close()
  Ask.send(message, ctx, { force = force })
end

local function selection_of(ctx)
  if not (ctx and vim.api.nvim_buf_is_valid(ctx.buf)) then
    return nil
  end
  local lines = vim.api.nvim_buf_get_lines(ctx.buf, ctx.srow, ctx.erow + 1, false)
  return {
    file = ctx.file,
    line = ctx.srow + 1,
    end_line = ctx.erow + 1,
    code = table.concat(lines, "\n"),
    filetype = vim.bo[ctx.buf].filetype,
    modified = vim.bo[ctx.buf].modified,
  }
end

--- Deliver one message. `opts = { force, target, retried }`.
function Ask.send(message, ctx, opts)
  opts = opts or {}
  local M = hn()
  if M._inflight then
    -- Reachable from `:HerdrAsk <message>`, which never opened a box.
    draft = message
    notify("a send is already in progress", vim.log.levels.WARN)
    return
  end
  local data = { cwd = vim.fn.getcwd(-1, -1), message = message, selection = selection_of(ctx) }
  local remembered = opts.target == nil and last_target ~= nil
  local target = opts.target or (last_target and last_target.pane_id)

  local args = { "ask" }
  if target then
    vim.list_extend(args, { "--target", target })
  end
  if opts.force then
    table.insert(args, "--force")
  end
  if M.config.focus_after_ask then
    table.insert(args, "--focus")
  end

  M._inflight = true
  bridge.run(M.config, args, bridge.encode(data), function(ok, res)
    -- Stays held while the picker below is open, so a second ask cannot start
    -- behind the prompt. `res` is an error string when `ok` is false.
    if not (ok and res.needs_pick) then
      M._inflight = false
    end
    if not ok then
      draft = message
      local text = tostring(res)
      if text:find("unknown command `ask`", 1, true) then
        text = "this herdr-nvim binary predates :HerdrAsk; rebuild it with `cargo build --release`"
      end
      notify(text, vim.log.levels.ERROR)
      return
    end
    if res.needs_pick then
      local prompt = res.reason == "no_agent_in_workspace" and "No agent in this workspace — ask:"
        or "Ask which agent?"
      vim.ui.select(res.candidates or {}, {
        prompt = prompt,
        format_item = function(c)
          return c.label or c.pane_id or "?"
        end,
      }, function(choice)
        M._inflight = false
        if not choice then
          draft = message
          notify("ask cancelled")
          return
        end
        Ask.send(message, ctx, { force = opts.force, target = choice.pane_id })
      end)
      return
    end
    if not res.ok then
      if remembered and not opts.retried and STALE[res.code] then
        last_target = nil
        Ask.send(message, ctx, { force = opts.force, retried = true })
        return
      end
      draft = message
      notify(res.error or "ask failed", res.code == "agent_blocked" and vim.log.levels.WARN or vim.log.levels.ERROR)
      return
    end
    local t = res.target or {}
    draft = nil
    last_target = { pane_id = t.pane_id, agent = t.agent }
    local via = (res.via and res.via ~= "agent.prompt") and " (raw input)" or ""
    notify(("asked %s (%s)%s"):format(t.agent or "agent", t.pane_id or "?", via))
  end, { label = "asking the agent" })
end

--- Choose the agent future asks go to, instead of inheriting the last one.
function Ask.retarget()
  hn().agents(function(list)
    if type(list) ~= "table" then
      return -- M.agents already notified
    end
    if #list == 0 then
      notify("no agents running")
      return
    end
    vim.ui.select(list, {
      prompt = "Ask which agent?",
      format_item = function(c)
        return c.label or c.pane_id or "?"
      end,
    }, function(choice)
      if not choice then
        return
      end
      last_target = { pane_id = choice.pane_id, agent = choice.agent }
      notify(("asking %s (%s) from now on"):format(choice.agent or "agent", choice.pane_id or "?"))
    end)
  end)
end

return Ask
