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

The optional Diffview integration test needs a Diffview checkout and the built
Rust binary. It opens real commit, working-tree, and index comparisons in a
temporary repository, captures the annotation payload, and formats it with
`send --dry-run`. It never sends a message to an agent.

```sh
cargo build --release
HERDR_NVIM_TEST_DIFFVIEW=/path/to/diffview.nvim scripts/lua-tests.sh diffview
```
