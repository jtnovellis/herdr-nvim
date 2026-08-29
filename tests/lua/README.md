Lua tests for the Neovim side. Each file is run by `scripts/lua-tests.sh` in its
own `nvim --clean --headless` process, after `plugin/herdr-nvim.lua` has been
sourced. Fixtures are recreated fresh before every file, so tests are
order-independent.

Globals provided by the prelude:

  check(cond, msg)   fail the test (exit 1) when `cond` is falsy
  pass(name)         print "ok  <name>"
  TMP                the per-run temp directory
  tmp(name)          TMP .. "/" .. name
  edit(name)         :edit the fixture `name` in TMP
  have_binary()      true when target/release/herdr-nvim exists
  have_herdr()       true when the `herdr` CLI is on $PATH

Run one file: `scripts/lua-tests.sh picker`
