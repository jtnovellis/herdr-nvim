-- Reload files that agents (or anything else) change on disk. FocusGained
-- never fires inside a Herdr split, so watch directories with libuv and
-- run :checktime on a debounce; modified buffers are flagged, not clobbered.
local R = {}
local uv = vim.uv or vim.loop

local state = { started = false, watchers = {}, refs = {}, debounce = nil, timer = nil, opts = {} }
local MAX_WATCHERS = 64

local function notify(msg, level)
  require("herdr-nvim").notify(msg, level)
end

local function ui_attached()
  return #vim.api.nvim_list_uis() > 0
end

--- Reload every unmodified file buffer whose file changed on disk.
function R.checktime(force)
  if not force and not ui_attached() and not state.opts.force_without_ui then
    return false
  end
  pcall(vim.cmd, "silent! checktime")
  return true
end

local function schedule_check()
  if state.debounce then
    state.debounce:stop()
  else
    state.debounce = uv.new_timer()
  end
  state.debounce:start(state.opts.debounce_ms or 200, 0, function()
    vim.schedule(function()
      R.checktime(false)
    end)
  end)
end

local function watch_dir(dir)
  if state.watchers[dir] then
    state.refs[dir] = (state.refs[dir] or 0) + 1
    return
  end
  if vim.tbl_count(state.watchers) >= MAX_WATCHERS then
    R.start_timer()
    return
  end
  local handle = uv.new_fs_event()
  if not handle then
    return
  end
  local ok = pcall(handle.start, handle, dir, {}, function(err)
    if err then
      return
    end
    schedule_check()
  end)
  if ok then
    state.watchers[dir] = handle
    state.refs[dir] = 1
  end
end

local function unwatch_dir(dir)
  local refs = (state.refs[dir] or 1) - 1
  state.refs[dir] = refs
  if refs <= 0 and state.watchers[dir] then
    pcall(state.watchers[dir].stop, state.watchers[dir])
    pcall(state.watchers[dir].close, state.watchers[dir])
    state.watchers[dir] = nil
    state.refs[dir] = nil
  end
end

local function buffer_dir(buf)
  local name = vim.api.nvim_buf_get_name(buf)
  if name == "" or vim.bo[buf].buftype ~= "" then
    return nil
  end
  return vim.fn.fnamemodify(name, ":p:h")
end

--- Polling fallback, used only when the watcher cap is reached.
function R.start_timer()
  if state.timer then
    return
  end
  state.timer = uv.new_timer()
  state.timer:start(2000, 2000, function()
    vim.schedule(function()
      if ui_attached() then
        R.checktime(false)
      end
    end)
  end)
end

function R.start(opts)
  if state.started then
    return
  end
  state.started = true
  state.opts = opts or {}
  vim.o.autoread = true

  local group = vim.api.nvim_create_augroup("HerdrNvimReload", { clear = true })
  vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile", "BufFilePost" }, {
    group = group,
    callback = function(ev)
      local dir = buffer_dir(ev.buf)
      if dir and not vim.b[ev.buf].herdr_nvim_watch_dir then
        vim.b[ev.buf].herdr_nvim_watch_dir = dir
        watch_dir(dir)
      end
    end,
  })
  vim.api.nvim_create_autocmd({ "BufDelete", "BufWipeout" }, {
    group = group,
    callback = function(ev)
      local dir = vim.b[ev.buf] and vim.b[ev.buf].herdr_nvim_watch_dir
      if dir then
        unwatch_dir(dir)
      end
    end,
  })
  -- Decide per buffer what a change on disk means: reload clean buffers,
  -- keep modified ones and say so once.
  vim.api.nvim_create_autocmd("FileChangedShell", {
    group = group,
    callback = function(ev)
      local reason = vim.v.fcs_reason
      if reason == "deleted" then
        vim.v.fcs_choice = ""
        return
      end
      if vim.bo[ev.buf].modified and reason ~= "mode" and reason ~= "time" then
        vim.v.fcs_choice = ""
        if not vim.b[ev.buf].herdr_nvim_stale then
          vim.b[ev.buf].herdr_nvim_stale = true
          notify(vim.fn.fnamemodify(ev.file, ":~:.") .. " changed on disk but the buffer is modified; :e! to reload", vim.log.levels.WARN)
        end
        return
      end
      vim.v.fcs_choice = "reload"
    end,
  })
  vim.api.nvim_create_autocmd("BufWritePost", {
    group = group,
    callback = function(ev)
      vim.b[ev.buf].herdr_nvim_stale = nil
    end,
  })
  -- Everything that happened while the sidebar was hidden.
  vim.api.nvim_create_autocmd("UIEnter", {
    group = group,
    callback = function()
      vim.schedule(function()
        R.checktime(true)
      end)
    end,
  })
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) then
      local dir = buffer_dir(buf)
      if dir and not vim.b[buf].herdr_nvim_watch_dir then
        vim.b[buf].herdr_nvim_watch_dir = dir
        watch_dir(dir)
      end
    end
  end
end

function R.stop()
  for dir, handle in pairs(state.watchers) do
    pcall(handle.stop, handle)
    pcall(handle.close, handle)
    state.watchers[dir] = nil
  end
  state.refs = {}
  if state.timer then
    state.timer:stop()
    state.timer:close()
    state.timer = nil
  end
  if state.debounce then
    state.debounce:stop()
    state.debounce:close()
    state.debounce = nil
  end
  pcall(vim.api.nvim_del_augroup_by_name, "HerdrNvimReload")
  state.started = false
end

function R.status()
  return {
    started = state.started,
    watchers = vim.tbl_count(state.watchers),
    polling = state.timer ~= nil,
  }
end

return R
