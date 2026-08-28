local M = {}

function M.check()
  local health = vim.health
  local hn = require("herdr-nvim")
  local bridge = require("herdr-nvim.bridge")
  local A = require("herdr-nvim.annotations")

  health.start("herdr-nvim: Neovim")
  if vim.fn.has("nvim-0.11") == 1 then
    health.ok("Neovim " .. tostring(vim.version()) .. " (>= 0.11)")
  else
    health.error("Neovim 0.11 or newer is required (--remote-ui and :detach)")
  end
  if vim.fn.exists(":detach") == 2 then
    health.ok(":detach is available")
  else
    health.warn(":detach is not available; leave the sidebar with the Herdr toggle key")
  end
  if vim.ui.input ~= nil then
    local info = debug.getinfo(vim.ui.input, "S")
    local src = info and info.short_src or "?"
    if src:find("vim/ui%.lua") or src:find("runtime") then
      health.ok("vim.ui.input is the default implementation")
    else
      health.info("vim.ui.input is overridden by " .. src)
    end
  end

  health.start("herdr-nvim: binary")
  local bin, source = bridge.resolve(hn.config)
  if bin then
    health.ok(("binary: %s (from %s)"):format(bin, source))
    local res = vim.system({ bin, "version" }, { text = true, timeout = 3000 }):wait()
    if res.code == 0 then
      health.ok(vim.trim(res.stdout))
    else
      health.warn("could not run the binary: " .. vim.trim(res.stderr or ""))
    end
  else
    health.error("herdr-nvim binary not found; run `cargo build --release` in " .. bridge.root)
  end

  health.start("herdr-nvim: Herdr")
  local herdr = vim.env.HERDR_BIN_PATH or "herdr"
  if vim.fn.executable(herdr) == 1 then
    health.ok("herdr CLI: " .. herdr)
  else
    health.error("herdr CLI not found (set HERDR_BIN_PATH or add herdr to $PATH)")
  end
  if vim.env.HERDR_SOCKET_PATH then
    if vim.uv.fs_stat(vim.env.HERDR_SOCKET_PATH) then
      health.ok("HERDR_SOCKET_PATH: " .. vim.env.HERDR_SOCKET_PATH)
    else
      health.warn("HERDR_SOCKET_PATH is set but does not exist: " .. vim.env.HERDR_SOCKET_PATH)
    end
  else
    health.info("HERDR_SOCKET_PATH not set; the default session socket will be used")
  end
  local ctx = {}
  for _, key in ipairs({ "HERDR_WORKSPACE_ID", "HERDR_TAB_ID", "HERDR_PANE_ID" }) do
    if vim.env[key] then
      table.insert(ctx, key .. "=" .. vim.env[key])
    end
  end
  if #ctx > 0 then
    health.ok("Herdr context: " .. table.concat(ctx, " "))
  else
    health.warn("not inside a Herdr pane: sends will consider every agent in the session")
  end

  health.start("herdr-nvim: sidebar daemon")
  if vim.env.HERDR_NVIM_DAEMON == "1" then
    health.ok("running as a sidebar daemon for tab " .. tostring(vim.env.HERDR_NVIM_TAB_ID))
    health.info("server: " .. tostring(vim.v.servername) .. ", attached UIs: " .. #vim.api.nvim_list_uis())
    local rs = require("herdr-nvim.reload").status()
    if rs.started then
      health.ok(("reload watcher active (%d director%s%s)"):format(rs.watchers, rs.watchers == 1 and "y" or "ies", rs.polling and ", polling fallback" or ""))
    else
      health.info("reload watcher disabled")
    end
    health.info("autoread=" .. tostring(vim.o.autoread) .. ", quit guard=" .. tostring(hn.config.quit_guard))
  else
    health.info("not a sidebar daemon (annotations still work; open the sidebar with the Herdr toggle action)")
  end

  health.start("herdr-nvim: agents")
  if bin then
    local done, result = false, nil
    hn.agents(function(list)
      done, result = true, list
    end, true)
    vim.wait(3000, function()
      return done
    end)
    if not done then
      health.warn("agent list did not answer within 3 s (is Herdr running?)")
    elseif type(result) ~= "table" then
      health.warn("could not list agents: " .. tostring(result))
    elseif #result == 0 then
      health.info("no agents running")
    else
      local here = vim.tbl_filter(function(c)
        return c.same_workspace
      end, result)
      health.ok(("%d agent(s) visible, %d in this workspace"):format(#result, #here))
      for _, c in ipairs(result) do
        health.info(c.label)
      end
    end
  end

  health.start("herdr-nvim: file picker")
  local claude_root = (vim.env.CLAUDE_CONFIG_DIR and vim.env.CLAUDE_CONFIG_DIR ~= "") and vim.env.CLAUDE_CONFIG_DIR or (vim.env.HOME .. "/.claude")
  if vim.uv.fs_stat(claude_root .. "/projects") then
    health.ok("Claude Code session logs: " .. claude_root .. "/projects")
  else
    health.info("no Claude Code session logs at " .. claude_root .. "/projects (the picker falls back to git + pane output)")
  end
  if vim.fn.executable("git") == 1 then
    health.ok("git found (dirty files, diff stats, repo-wide search)")
  else
    health.warn("git not found: the picker only lists session and scraped files")
  end

  health.start("herdr-nvim: annotations")
  local pending = A.count()
  local total = A.total()
  health.info(("%d pending, %d total (stale/delivered included)"):format(pending, total))
end

return M
