#!/usr/bin/env bash
# Headless Neovim checks for the Lua side.
#
#   scripts/lua-tests.sh              run every test in tests/lua/
#   scripts/lua-tests.sh picker       run only tests whose name contains "picker"
#
# Each test file runs in its own `nvim --clean --headless` process with fresh
# fixtures, so tests are order-independent. `nvim` is the only hard requirement:
# tests that need `target/release/herdr-nvim` or the `herdr` CLI relax their
# assertions when those are missing (see tests/lua/README.md).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
FILTER="${1:-}"

PRELUDE="$TMP/prelude.lua"
cat > "$PRELUDE" <<'LUA'
_G.check = function(cond, msg)
  if not cond then
    io.stderr:write("FAIL: " .. msg .. "\n")
    vim.cmd("cquit 1")
  end
end
_G.pass = function(name)
  io.stdout:write("ok  " .. name .. "\n")
end
_G.TMP = assert(vim.env.HERDR_NVIM_TEST_TMP, "HERDR_NVIM_TEST_TMP is not set")
_G.tmp = function(name)
  return _G.TMP .. "/" .. name
end
_G.edit = function(name)
  vim.cmd("edit " .. vim.fn.fnameescape(_G.tmp(name)))
end
_G.have_binary = function()
  -- Ask the resolver rather than guessing: it also falls back to
  -- target/debug and $PATH, so a path check disagrees with what the plugin
  -- (and :checkhealth) actually sees.
  local ok, bridge = pcall(require, "herdr-nvim.bridge")
  if not ok then
    return false
  end
  return bridge.resolve(require("herdr-nvim").config) ~= nil
end
_G.have_herdr = function()
  return vim.fn.executable("herdr") == 1
end
vim.g.mapleader = " "
-- Keep the suite's output to just ok/FAIL lines.
vim.opt.report = 9999
vim.opt.shortmess:append("F")
LUA

export HERDR_NVIM_TEST_TMP="$TMP"
export HERDR_NVIM_TEST_ROOT="$ROOT"

# Recreated before every test so files mutated by one test cannot leak into the next.
make_fixtures() {
  printf 'a\nb\nc\nd\ne\n' > "$TMP/hn.txt"
  printf 'x1\nx2\nx3\n' > "$TMP/other.txt"
  rm -f "$TMP/handoff.json"
}

ran=0
for test_file in "$ROOT"/tests/lua/*.lua; do
  name="$(basename "$test_file" .lua)"
  if [ -n "$FILTER" ] && [[ "$name" != *"$FILTER"* ]]; then
    continue
  fi
  make_fixtures
  ran=$((ran + 1))
  nvim --clean --headless \
    --cmd "set rtp+=$ROOT" \
    --cmd "luafile $PRELUDE" \
    -c "runtime plugin/herdr-nvim.lua" \
    -c "luafile $test_file" \
    -c "qa!" \
    || { echo "FAILED: $name"; exit 1; }
done

if [ "$ran" -eq 0 ]; then
  echo "no Lua tests matched '$FILTER'" >&2
  exit 1
fi
echo "all $ran Lua checks passed"
