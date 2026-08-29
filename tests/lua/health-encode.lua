local bridge = require("herdr-nvim.bridge")

local json = bridge.encode({ code = "caf\xe9 bad", ok = true })
check(type(json) == "string" and json:find("ok"), "encode failed on invalid utf-8")

vim.cmd("silent checkhealth herdr-nvim")
local lines = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n")

-- With no binary anywhere -- a fresh CI checkout has no target/ at all --
-- health correctly reports that instead of naming one.
if have_binary() then
  check(lines:find("binary:"), "health missing binary line:\n" .. lines)
else
  check(lines:find("binary not found"), "health did not report the missing binary:\n" .. lines)
end

-- `:checkhealth` legitimately reports ERROR when the release binary has not
-- been built or the `herdr` CLI is not installed. Only assert a clean report
-- when both prerequisites are actually present.
if have_binary() and have_herdr() then
  check(not lines:find("ERROR"), "health reported an ERROR:\n" .. lines)
else
  local skipped = {}
  if not have_binary() then
    table.insert(skipped, "no target/release/herdr-nvim (run `cargo build --release`)")
  end
  if not have_herdr() then
    table.insert(skipped, "no `herdr` on $PATH")
  end
  io.stdout:write("    (skipped the no-ERROR assertion: " .. table.concat(skipped, ", ") .. ")\n")
end
pass("health + encode")
