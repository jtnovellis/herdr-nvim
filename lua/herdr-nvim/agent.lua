-- What the agent is doing, and what it said.
--
-- Two channels, both pushed rather than polled. Herdr delivers
-- `pane.agent_status_changed` to the plugin binary, which forwards it here
-- through the daemon's msgpack socket (`on_status`). The words and the edits
-- come from the agent's own transcript: `agent.prompt` answers with lifecycle
-- state and never text, but Herdr reports the transcript file behind every
-- agent pane, and that file is append-only. `ask` records its length at the
-- moment it sends, so everything past that offset is the reply to that
-- message -- no diffing, no guessing where a turn begins.
local Agent = {}
local uv = vim.uv or vim.loop

local bridge = require("herdr-nvim.bridge")

-- How long after the transcript changes before reading it. The file is
-- written in bursts as the agent works; a small debounce collapses a burst
-- into one read without making the reply feel delayed.
local TAIL_DEBOUNCE_MS = 120
-- Watching a transcript costs a file watch and a subprocess per burst, so it
-- stops on its own once the agent has been idle this long with nothing new.
local TAIL_IDLE_MS = 5 * 60 * 1000

local state = {
  -- Last reported agent state, keyed by pane id.
  status = {},
  -- The pane whose reply we are currently following.
  following = nil,
  watcher = nil,
  debounce = nil,
  idle = nil,
  session = nil, -- { path, offset, agent }
}

local function hn()
  return require("herdr-nvim")
end

--- Fire a `User` autocmd carrying its own payload.
---
--- `HerdrNvimAnnotationsChanged` deliberately carries none, so listeners have
--- to ask for the count. These carry theirs: a statusline reading them should
--- not have to make a call to find out what changed.
local function emit(pattern, data)
  pcall(vim.api.nvim_exec_autocmds, "User", {
    pattern = pattern,
    modeline = false,
    data = data,
  })
end

-- ---------------------------------------------------------------- status ---

--- Called from the plugin binary over the daemon socket, with the event as a
--- JSON string. Never throws: it runs inside `nvim_eval` on the daemon's RPC
--- channel, where an error would surface to Herdr as a failed event hook.
function Agent.on_status(raw)
  local ok, event = pcall(vim.json.decode, raw)
  if not ok or type(event) ~= "table" or not event.pane_id then
    return 0
  end
  local previous = state.status[event.pane_id]
  state.status[event.pane_id] = {
    pane_id = event.pane_id,
    agent = event.agent,
    status = event.status,
    at = os.time(),
  }
  if not previous or previous.status ~= event.status then
    emit("HerdrNvimAgentStatus", state.status[event.pane_id])
    pcall(vim.cmd.redrawstatus)
  end
  -- The agent going quiet is the cue that the reply is complete: read once
  -- more so nothing written in the final burst is missed.
  if state.following == event.pane_id and event.status ~= "working" then
    Agent.read_now()
  end
  return 1
end

--- The last reported state for a pane.
---
--- With no pane named, prefer the one whose reply we are following, then the
--- most recently changed: a statusline asks this question with no argument and
--- wants "what is happening", not "nothing, because you did not say where".
function Agent.status(pane_id)
  if pane_id then
    return state.status[pane_id]
  end
  if state.following and state.status[state.following] then
    return state.status[state.following]
  end
  -- Prefer a pane that is actually doing something. A released agent leaves a
  -- final `unknown` behind, and picking the most recent entry outright would
  -- let that stale row mask a pane that really is working. Ties on `at` are
  -- common -- os.time() has second resolution -- so the comparison must not
  -- depend on pairs() order either.
  local latest, fallback
  for _, entry in pairs(state.status) do
    if entry.status ~= "unknown" then
      if not latest or (entry.at or 0) > (latest.at or 0) then
        latest = entry
      end
    elseif not fallback or (entry.at or 0) > (fallback.at or 0) then
      fallback = entry
    end
  end
  return latest or fallback
end

--- Every pane state we have been told about.
function Agent.all()
  return state.status
end

-- ----------------------------------------------------------------- reply ---

local function stop_timer(timer)
  if timer then
    timer:stop()
    if not timer:is_closing() then
      timer:close()
    end
  end
  return nil
end

--- Stop following a reply. Idempotent: called on a new ask, on idle expiry,
--- and when the reply view closes.
function Agent.unfollow()
  if state.watcher then
    pcall(function()
      state.watcher:stop()
    end)
    if not state.watcher:is_closing() then
      state.watcher:close()
    end
    state.watcher = nil
  end
  state.debounce = stop_timer(state.debounce)
  state.idle = stop_timer(state.idle)
  state.following = nil
  state.session = nil
end

local function touch_idle()
  state.idle = stop_timer(state.idle)
  state.idle = uv.new_timer()
  state.idle:start(TAIL_IDLE_MS, 0, function()
    vim.schedule(function()
      Agent.unfollow()
    end)
  end)
end

--- Read the transcript from the remembered offset and publish what is new.
function Agent.read_now()
  local session = state.session
  if not session or not session.path then
    return
  end
  local args = { "tail", "--path", session.path, "--from", tostring(session.offset or 0) }
  if session.agent and session.agent ~= "" then
    vim.list_extend(args, { "--agent", session.agent })
  end
  bridge.run(hn().config, args, nil, function(ok, res)
    -- A transcript that cannot be read is not worth a message: the agent's
    -- own pane still has the reply, exactly as before this existed.
    if not ok or type(res) ~= "table" or not res.ok then
      return
    end
    -- `state.session` may have been replaced by a newer ask while this call
    -- was in flight; publishing then would attribute an old reply to a new
    -- question.
    if state.session ~= session then
      return
    end
    session.offset = res.offset or session.offset
    local reply = res.reply or {}
    local edits = res.edits or {}
    if #reply == 0 and #edits == 0 then
      return
    end
    touch_idle()
    if #edits > 0 then
      require("herdr-nvim.review").record(edits, session.agent)
    end
    emit("HerdrNvimAgentReply", {
      pane_id = state.following,
      reply = reply,
      edits = edits,
    })
  end, { label = "reading the agent's reply" })
end

local function schedule_read()
  if state.debounce then
    state.debounce:stop()
  else
    state.debounce = uv.new_timer()
  end
  state.debounce:start(TAIL_DEBOUNCE_MS, 0, function()
    vim.schedule(Agent.read_now)
  end)
end

-- A freshly started agent has no transcript until the message we just sent
-- creates one, so the first watch attempt usually fails. Retrying briefly is
-- what makes the *first* ask to an agent behave like every later one; without
-- it the reply only lands when the agent goes quiet.
local WATCH_RETRY_MS = 250
local WATCH_ATTEMPTS = 20

local function start_watch(session, attempt)
  attempt = attempt or 1
  if state.session ~= session then
    return -- superseded by a newer ask
  end
  local watcher = uv.new_fs_event()
  local ok = pcall(watcher.start, watcher, session.path, {}, function(err)
    if not err then
      schedule_read()
    end
  end)
  if ok then
    state.watcher = watcher
    return
  end
  if not watcher:is_closing() then
    watcher:close()
  end
  if attempt >= WATCH_ATTEMPTS then
    return -- the status change that ends the turn still triggers a read
  end
  local timer = uv.new_timer()
  timer:start(WATCH_RETRY_MS, 0, function()
    timer:stop()
    if not timer:is_closing() then
      timer:close()
    end
    vim.schedule(function()
      start_watch(session, attempt + 1)
    end)
  end)
end

--- Follow the reply to a message just sent to `pane_id`.
---
--- `session` is the marker `herdr-nvim ask` returns: the transcript path and
--- its length at send time. A nil marker means Herdr tracks no transcript we
--- can read for this agent (a kind with no parser); following is simply
--- skipped and the agent's own pane stays the place to read.
function Agent.follow(pane_id, session)
  Agent.unfollow()
  if type(session) ~= "table" or not session.path or session.path == "" then
    return false
  end
  state.following = pane_id
  state.session = {
    path = session.path,
    offset = session.offset or 0,
    agent = session.agent,
  }
  -- Watch the file itself rather than its directory: a transcript directory
  -- holds every session for the project and would wake us for all of them.
  start_watch(state.session)
  touch_idle()
  return true
end

--- Whether a reply is currently being followed.
function Agent.following()
  return state.following
end

--- Test seam: the tail state, without exposing the timers.
function Agent.debug()
  return {
    following = state.following,
    watching = state.watcher ~= nil,
    offset = state.session and state.session.offset or nil,
    path = state.session and state.session.path or nil,
  }
end

return Agent
