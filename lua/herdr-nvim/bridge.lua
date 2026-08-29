-- Locates and runs the herdr-nvim Rust binary.
local M = {}

local function plugin_root()
  local src = debug.getinfo(1, "S").source
  if src:sub(1, 1) == "@" then
    src = src:sub(2)
  end
  return vim.fn.fnamemodify(src, ":h:h:h")
end

M.root = plugin_root()

--- Resolve the binary and say where it came from.
function M.resolve(config)
  local candidates = {}
  if config and config.binary and config.binary ~= "" then
    table.insert(candidates, { vim.fn.expand(config.binary), "config.binary" })
  end
  if vim.env.HERDR_NVIM_BIN and vim.env.HERDR_NVIM_BIN ~= "" then
    table.insert(candidates, { vim.fn.expand(vim.env.HERDR_NVIM_BIN), "$HERDR_NVIM_BIN" })
  end
  table.insert(candidates, { M.root .. "/target/release/herdr-nvim", "target/release" })
  table.insert(candidates, { M.root .. "/target/debug/herdr-nvim", "target/debug" })
  for _, candidate in ipairs(candidates) do
    if vim.fn.executable(candidate[1]) == 1 then
      return candidate[1], candidate[2]
    end
  end
  if vim.fn.executable("herdr-nvim") == 1 then
    return "herdr-nvim", "$PATH"
  end
  return nil, nil
end

function M.binary(config)
  return (M.resolve(config))
end

local function decode(out)
  if out == "" then
    return nil
  end
  local ok, value = pcall(vim.json.decode, out)
  if not ok or value == vim.NIL or type(value) ~= "table" then
    return nil
  end
  return value
end

-- Only announce a call that is actually slow: most return in a few
-- milliseconds and a message for those would be pure noise. Below this, the
-- user sees nothing; above it, they learn what is being waited on instead of
-- staring at a frozen editor for the full 10s timeout.
local PENDING_NOTICE_MS = 400

--- Say what we are waiting for, once the wait becomes noticeable.
--- Returns a function that cancels the pending notice.
local function announce_slow(label)
  local timer = vim.uv.new_timer()
  local shown = false
  timer:start(
    PENDING_NOTICE_MS,
    0,
    vim.schedule_wrap(function()
      shown = true
      require("herdr-nvim").notify(label .. "\u{2026}")
    end)
  )
  return function()
    timer:stop()
    timer:close()
    return shown
  end
end

--- Run `herdr-nvim <args>` asynchronously. `callback(ok, result_or_error)`
--- receives the decoded JSON printed by the binary.
---
--- `opts.label` names the operation for the slow-call notice; pass nil to stay
--- silent (the pane-title updates do, since they fire on every buffer switch).
function M.run(config, args, stdin, callback, opts)
  opts = opts or {}
  local bin = M.binary(config)
  if not bin then
    callback(false, "herdr-nvim binary not found; run `cargo build --release` in " .. M.root)
    return
  end
  local cmd = { bin }
  vim.list_extend(cmd, args)
  local sys_opts = { text = true, timeout = opts.timeout or 10000 }
  if stdin then
    sys_opts.stdin = stdin
  end
  local cancel_notice = opts.label and announce_slow(opts.label) or function() end
  local ok, err = pcall(vim.system, cmd, sys_opts, function(res)
    vim.schedule(function()
      cancel_notice()
      local out = vim.trim(res.stdout or "")
      local decoded = decode(out)
      if decoded == nil then
        local stderr = vim.trim(res.stderr or "")
        if res.signal == 15 or res.code == 124 then
          callback(false, "herdr-nvim timed out (is Herdr responding?)")
        else
          callback(false, stderr ~= "" and stderr or ("herdr-nvim exited with code " .. tostring(res.code)))
        end
        return
      end
      callback(true, decoded)
    end)
  end)
  if not ok then
    cancel_notice()
    callback(false, tostring(err))
  end
end

--- JSON-encode, replacing bytes that are not valid UTF-8 so the payload
--- never fails on the Rust side.
function M.encode(data)
  local ok, json = pcall(vim.json.encode, data)
  if ok then
    return json
  end
  local function scrub(value)
    if type(value) == "string" then
      local fixed = value:gsub("[\128-\255]", function(byte)
        return vim.str_utf_start ~= nil and byte or "?"
      end)
      local ok2 = pcall(vim.json.encode, fixed)
      return ok2 and fixed or (value:gsub("[\128-\255]", "?"))
    elseif type(value) == "table" then
      local out = {}
      for k, v in pairs(value) do
        out[k] = scrub(v)
      end
      return out
    end
    return value
  end
  return vim.json.encode(scrub(data))
end

return M
