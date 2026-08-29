-- herdr-nvim: code annotations you send to the agents running in your
-- Herdr workspace, plus the sidebar-daemon integration.
local M = {}

local A = require("herdr-nvim.annotations")
local bridge = require("herdr-nvim.bridge")

local defaults = {
  keymaps = true, -- true | false | "force" (override existing mappings)
  binary = nil, -- path to the herdr-nvim binary (auto-detected when nil)
  prompt = "Comment: ",
  clear_on_send = true, -- forget annotations after <leader>aS succeeds
  clear_on_paste = false, -- after <leader>as: false marks them delivered instead
  focus_after_send = false, -- focus the agent pane after delivery
  notify = true,
  signs = false, -- use the sign column (default: highlight the line number)
  statusline_icon = "●",
  -- Sidebar daemon features (only active when HERDR_NVIM_DAEMON=1):
  quit_guard = true, -- :q on the last window detaches instead of quitting
  reload = { enabled = true, debounce_ms = 200 }, -- reload files agents edit
  pane_title = true, -- show the current file as the Herdr pane title
}

M.config = vim.deepcopy(defaults)
M._setup_done = false
M._inflight = false

local DESC_PREFIX = "herdr-nvim: "

function M.notify(msg, level)
  level = level or vim.log.levels.INFO
  if M.config.notify or level >= vim.log.levels.WARN then
    vim.notify("herdr-nvim: " .. msg, level)
  end
end
local notify = M.notify

local function define_highlights()
  local hl = vim.api.nvim_set_hl
  hl(0, "HerdrNvimAnnotation", { link = "DiffChange", default = true })
  hl(0, "HerdrNvimSign", { link = "DiagnosticSignInfo", default = true })
  hl(0, "HerdrNvimVirt", { link = "DiagnosticVirtualTextInfo", default = true })
  hl(0, "HerdrNvimListLoc", { link = "Directory", default = true })
  hl(0, "HerdrNvimStale", { link = "Comment", default = true })
  hl(0, "HerdrNvimDelivered", { link = "DiagnosticVirtualTextOk", default = true })
  -- Defined here rather than in picker.open so they are re-applied on
  -- ColorScheme like the rest, and can be overridden before the first open.
  hl(0, "HerdrNvimPickerMatch", { link = "Special", default = true })
  hl(0, "HerdrNvimPickerDiff", { link = "DiffAdd", default = true })
  hl(0, "HerdrNvimPickerNew", { link = "DiagnosticVirtualTextWarn", default = true })
  hl(0, "HerdrNvimPickerAge", { link = "Comment", default = true })
end

local keymaps = {
  {
    "n",
    "<leader>ac",
    function()
      M.annotate()
    end,
    "comment the current line",
  },
  -- `:` from Visual mode inserts the '<,'> range itself; the command takes it.
  { "x", "<leader>ac", ":HerdrAnnotate<CR>", "comment the selection" },
  {
    "n",
    "<leader>al",
    function()
      M.list()
    end,
    "list annotations",
  },
  {
    "n",
    "<leader>as",
    function()
      M.paste()
    end,
    "paste annotations into the agent's input",
  },
  {
    "n",
    "<leader>aS",
    function()
      M.send()
    end,
    "send annotations to the agent",
  },
  {
    "n",
    "<leader>af",
    function()
      M.pick_file()
    end,
    "pick a file the agent touched",
  },
  {
    "n",
    "]a",
    function()
      M.next()
    end,
    "next annotation",
  },
  {
    "n",
    "[a",
    function()
      M.prev()
    end,
    "previous annotation",
  },
}

local function is_ours(mapping)
  return type(mapping) == "table"
    and type(mapping.desc) == "string"
    and mapping.desc:sub(1, #DESC_PREFIX) == DESC_PREFIX
end

local function apply_keymaps(mode)
  for _, map in ipairs(keymaps) do
    local existing = vim.fn.maparg(map[2], map[1], false, true)
    local taken = type(existing) == "table" and next(existing) ~= nil
    if mode then
      if not taken or is_ours(existing) or mode == "force" then
        vim.keymap.set(map[1], map[2], map[3], { desc = DESC_PREFIX .. map[4], silent = true })
      end
    elseif taken and is_ours(existing) then
      pcall(vim.keymap.del, map[1], map[2])
    end
  end
end

local function validate(opts)
  vim.validate("opts", opts, "table", true)
  if not opts then
    return
  end
  for key in pairs(opts) do
    if defaults[key] == nil and key ~= "binary" then
      notify("unknown option `" .. tostring(key) .. "`", vim.log.levels.WARN)
    end
  end
end

--- Pure helper for the :q guard: intercept only when a UI is attached and
--- this is the last non-floating window.
function M.quit_guard_should_intercept(info)
  return (info.uis or 0) > 0 and (info.windows or 0) <= 1
end

local function non_floating_windows()
  local n = 0
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    if vim.api.nvim_win_get_config(win).relative == "" then
      n = n + 1
    end
  end
  return n
end

local title_timer, last_title = nil, nil

--- Release the debounce handle. libuv timers must be closed, not just stopped.
local function stop_title_timer()
  if title_timer then
    title_timer:stop()
    title_timer:close()
    title_timer = nil
  end
end

local function report_title()
  if title_timer then
    title_timer:stop()
  else
    title_timer = (vim.uv or vim.loop).new_timer()
  end
  title_timer:start(300, 0, function()
    vim.schedule(function()
      if #vim.api.nvim_list_uis() == 0 then
        return
      end
      local buf = vim.api.nvim_get_current_buf()
      local name = vim.api.nvim_buf_get_name(buf)
      local title = name == "" and "" or vim.fn.fnamemodify(name, ":~:.")
      if vim.bo[buf].modified and title ~= "" then
        title = title .. " +"
      end
      if title == last_title then
        return
      end
      last_title = title
      bridge.run(M.config, { "title", title }, nil, function() end)
    end)
  end)
end

local function setup_daemon_features()
  if vim.env.HERDR_NVIM_DAEMON ~= "1" then
    return
  end
  local group = vim.api.nvim_create_augroup("HerdrNvimDaemon", { clear = true })
  if M.config.quit_guard then
    vim.api.nvim_create_autocmd("QuitPre", {
      group = group,
      callback = function()
        local uis = vim.g.herdr_nvim_test_uis or #vim.api.nvim_list_uis()
        if not M.quit_guard_should_intercept({ uis = uis, windows = non_floating_windows() }) then
          return
        end
        -- :q closes the window we open here instead of the daemon; then detach.
        vim.cmd("split")
        vim.schedule(function()
          if vim.fn.exists(":detach") == 2 and #vim.api.nvim_list_uis() > 0 then
            notify("sidebar detached; the daemon keeps running (:qa quits it)")
            vim.cmd("detach")
          end
        end)
      end,
    })
  end
  if M.config.reload and M.config.reload.enabled ~= false then
    require("herdr-nvim.reload").start(M.config.reload)
  end
  if M.config.pane_title then
    vim.api.nvim_create_autocmd({ "BufEnter", "BufWritePost", "BufModifiedSet", "UIEnter" }, {
      group = group,
      callback = report_title,
    })
    report_title()
  end
  -- libuv handles outlive the Lua values that hold them, so hand them back
  -- explicitly rather than relying on process teardown.
  vim.api.nvim_create_autocmd("VimLeavePre", {
    group = group,
    callback = function()
      stop_title_timer()
      pcall(function()
        require("herdr-nvim.reload").stop()
      end)
    end,
  })
end

function M.setup(opts)
  validate(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})
  M._setup_done = true
  A.config.signs = M.config.signs == true
  define_highlights()
  vim.api.nvim_create_autocmd("ColorScheme", {
    group = vim.api.nvim_create_augroup("HerdrNvimHighlights", { clear = true }),
    callback = define_highlights,
  })
  if M.config.keymaps then
    apply_keymaps(M.config.keymaps == "force" and "force" or true)
  else
    apply_keymaps(false)
  end
  setup_daemon_features()
end

local function input(prompt, default, callback)
  vim.ui.input({ prompt = prompt, default = default }, function(text)
    if text == nil then
      return
    end
    text = vim.trim(text)
    if text == "" then
      return
    end
    callback(text)
  end)
end

local function current_range(opts)
  if opts and opts.line1 then
    return opts.line1 - 1, (opts.line2 or opts.line1) - 1
  end
  local mode = vim.fn.mode()
  if mode == "v" or mode == "V" or mode == "\22" then
    local s = vim.fn.getpos("v")[2]
    local e = vim.fn.getpos(".")[2]
    -- "x" executes the key now, so a following vim.ui.input() does not read it.
    vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("<Esc>", true, false, true), "nx", false)
    return math.min(s, e) - 1, math.max(s, e) - 1
  end
  local row = vim.api.nvim_win_get_cursor(0)[1] - 1
  return row, row
end

--- Annotate the current line, the visual selection, or an explicit range
--- (`opts = { line1 = n, line2 = m }`). Overlapping an existing annotation edits it.
function M.annotate(opts)
  local buf = vim.api.nvim_get_current_buf()
  if vim.bo[buf].buftype ~= "" then
    notify("annotations need a file buffer", vim.log.levels.WARN)
    return
  end
  if vim.api.nvim_buf_get_name(buf) == "" then
    notify("save the file first: unnamed buffers cannot be sent to an agent", vim.log.levels.WARN)
    return
  end
  local srow, erow = current_range(opts)
  local existing = A.find_overlapping(buf, srow, erow)
  if existing then
    M.edit(existing.id)
    return
  end
  input(M.config.prompt, "", function(text)
    A.add(buf, srow, erow, text)
  end)
end

function M.edit(id, after)
  local item = A.get(id)
  if not item then
    return
  end
  input(M.config.prompt, item.text, function(text)
    A.update(id, text)
    if after then
      after()
    end
  end)
end

function M.list()
  require("herdr-nvim.ui").open()
end

function M.clear()
  local n = A.clear()
  notify(("cleared %d annotation%s"):format(n, n == 1 and "" or "s"))
end

function M.count()
  return A.count()
end

--- Statusline component: "● 3" while annotations are pending, else "".
function M.statusline()
  local n = A.count()
  if n == 0 then
    return ""
  end
  return M.config.statusline_icon .. " " .. n
end

local function goto_item(item)
  -- Reached from the ]a / [a keymaps, so nothing here may throw: A.list keeps
  -- items whose buffer has since been wiped.
  if not (item and item.buf and vim.api.nvim_buf_is_valid(item.buf)) then
    notify("that annotation's buffer is gone", vim.log.levels.WARN)
    return
  end
  local win = vim.api.nvim_get_current_win()
  if vim.api.nvim_win_get_buf(win) ~= item.buf then
    -- 'winfixbuf' and friends can refuse the swap; fall back to a split.
    if not pcall(vim.api.nvim_win_set_buf, win, item.buf) then
      if not pcall(vim.cmd, "vsplit") or not pcall(vim.api.nvim_win_set_buf, 0, item.buf) then
        notify("could not open that annotation's buffer here", vim.log.levels.WARN)
        return
      end
    end
  end
  local last = vim.api.nvim_buf_line_count(item.buf)
  pcall(vim.api.nvim_win_set_cursor, 0, { math.min(item.row + 1, last), 0 })
end

local function cycle(direction)
  local items = A.list()
  if #items == 0 then
    notify("no pending annotations")
    return
  end
  local buf = vim.api.nvim_get_current_buf()
  local row = vim.api.nvim_win_get_cursor(0)[1] - 1
  local index
  if direction > 0 then
    for i, item in ipairs(items) do
      if item.buf == buf and item.row > row then
        index = i
        break
      end
    end
    if not index then
      for i, item in ipairs(items) do
        if item.buf ~= buf and (index == nil) then
          index = i
        end
      end
    end
    index = index or 1
  else
    for i = #items, 1, -1 do
      local item = items[i]
      if item.buf == buf and item.row < row then
        index = i
        break
      end
    end
    index = index or #items
  end
  goto_item(items[index])
end

function M.next()
  cycle(1)
end

function M.prev()
  cycle(-1)
end

local function payload()
  local comments = {}
  for _, item in ipairs(A.list()) do
    if item.file ~= "" then
      local lines = vim.api.nvim_buf_get_lines(item.buf, item.row, item.end_row + 1, false)
      table.insert(comments, {
        file = item.file,
        line = item.row + 1,
        end_line = item.end_row + 1,
        text = item.text,
        code = table.concat(lines, "\n"),
        filetype = vim.bo[item.buf].filetype,
        modified = vim.bo[item.buf].modified,
      })
    end
  end
  return { cwd = vim.fn.getcwd(-1, -1), comments = comments }
end

local function deliver(mode, target, force)
  if M._inflight then
    notify("a send is already in progress", vim.log.levels.WARN)
    return
  end
  local data = payload()
  if #data.comments == 0 then
    if A.total() > 0 then
      notify(
        "nothing pending: remaining annotations are stale or already delivered (:HerdrClear to forget them)",
        vim.log.levels.WARN
      )
    else
      notify("no annotations to send", vim.log.levels.WARN)
    end
    return
  end
  local args = { "send", mode == "submit" and "--submit" or "--paste" }
  if target then
    vim.list_extend(args, { "--target", target })
  end
  if force then
    table.insert(args, "--force")
  end
  if M.config.focus_after_send then
    table.insert(args, "--focus")
  end
  M._inflight = true
  bridge.run(M.config, args, bridge.encode(data), function(ok, res)
    -- Stays held while the agent picker below is open: clearing it here let a
    -- second <leader>aS start another send behind the prompt.
    -- `res` is an error string when `ok` is false; indexing it yields nil.
    if not (ok and res.needs_pick) then
      M._inflight = false
    end
    if not ok then
      notify(tostring(res), vim.log.levels.ERROR)
      return
    end
    if res.needs_pick then
      local prompt = res.reason == "no_agent_in_workspace" and "No agent in this workspace — send annotations to:"
        or "Send annotations to:"
      vim.ui.select(res.candidates or {}, {
        prompt = prompt,
        format_item = function(c)
          return c.label or c.pane_id or "?"
        end,
      }, function(choice)
        M._inflight = false
        if not choice then
          notify("send cancelled")
          return
        end
        deliver(mode, choice.pane_id, force)
      end)
      return
    end
    if not res.ok then
      local level = res.code == "agent_blocked" and vim.log.levels.WARN or vim.log.levels.ERROR
      notify(res.error or "send failed", level)
      return
    end
    local t = res.target or {}
    local count = res.count or #data.comments
    if mode == "submit" then
      if M.config.clear_on_send then
        A.clear()
      else
        A.mark_delivered()
      end
    else
      if M.config.clear_on_paste then
        A.clear()
      else
        A.mark_delivered()
      end
    end
    local verb = mode == "submit" and "sent" or "pasted"
    local via = (mode == "submit" and res.via and res.via ~= "agent.prompt") and " (raw input)" or ""
    notify(
      ("%s %d annotation%s to %s (%s)%s"):format(
        verb,
        count,
        count == 1 and "" or "s",
        t.agent or "agent",
        t.pane_id or "?",
        via
      )
    )
  end, { label = "sending annotations" })
end

--- Paste all annotations into the agent's input without submitting.
function M.paste(opts)
  deliver("paste", nil, opts and opts.force)
end

--- Send all annotations to the agent and submit.
function M.send(opts)
  deliver("submit", nil, opts and opts.force)
end

--- Show the exact prompt that would be sent, in a scratch split.
function M.preview()
  local data = payload()
  if #data.comments == 0 then
    notify("no pending annotations to preview", vim.log.levels.WARN)
    return
  end
  bridge.run(M.config, { "send", "--dry-run" }, bridge.encode(data), function(ok, res)
    if not ok or not res.ok then
      notify(tostring(ok and res.error or res), vim.log.levels.ERROR)
      return
    end
    vim.cmd("botright new")
    local buf = vim.api.nvim_get_current_buf()
    vim.bo[buf].buftype = "nofile"
    vim.bo[buf].bufhidden = "wipe"
    vim.bo[buf].swapfile = false
    vim.bo[buf].filetype = "markdown"
    -- A previous preview split left open still owns this name; E95 otherwise.
    pcall(vim.api.nvim_buf_set_name, buf, "herdr-nvim://preview")
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, vim.split(res.prompt or "", "\n", { plain = true }))
    vim.bo[buf].modifiable = false
    vim.keymap.set("n", "q", "<Cmd>close<CR>", { buffer = buf, nowait = true })
  end, { label = "building the preview" })
end

--- Fuzzy-pick a file the agent touched this session (or any repo file).
function M.pick_file(target)
  local args = { "pick-file", "--json" }
  if target then
    vim.list_extend(args, { "--target", target })
  end
  bridge.run(M.config, args, nil, function(ok, res)
    if not ok then
      notify(tostring(res), vim.log.levels.ERROR)
      return
    end
    if res.needs_pick then
      vim.ui.select(res.candidates or {}, {
        prompt = "Pick files from which agent?",
        format_item = function(c)
          return c.label or c.pane_id or "?"
        end,
      }, function(choice)
        if choice then
          M.pick_file(choice.pane_id)
        end
      end)
      return
    end
    if not res.ok then
      notify(res.error or "pick-file failed", vim.log.levels.WARN)
      return
    end
    require("herdr-nvim.picker").open(res.handoff, { force_without_ui = true })
  end, { label = "gathering files" })
end

--- List agent candidates: `callback(list)`; notifies when no callback.
--- `quiet` suppresses error notifications (used by :checkhealth).
function M.agents(callback, quiet)
  bridge.run(M.config, { "agents" }, nil, function(ok, res)
    if not ok or not res.ok then
      local err = ok and (res.error or "could not list agents") or tostring(res)
      if callback then
        callback(err)
      end
      if not quiet then
        notify(err, vim.log.levels.ERROR)
      end
      return
    end
    if callback then
      callback(res.candidates or {})
      return
    end
    if not res.candidates or #res.candidates == 0 then
      notify("no agents running")
      return
    end
    local lines = {}
    for _, c in ipairs(res.candidates) do
      table.insert(lines, c.label)
    end
    vim.notify(table.concat(lines, "\n"), vim.log.levels.INFO)
  end, { label = quiet and nil or "listing agents" })
end

return M
