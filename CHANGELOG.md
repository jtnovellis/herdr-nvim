# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Prebuilt release binaries for macOS (arm64, x86_64) and Linux (x86_64,
  aarch64). `herdr plugin install` now downloads one instead of requiring a
  Rust toolchain, falling back to `cargo build --release` when there is no
  matching asset.
- `:help herdr-nvim` — a full vimdoc reference (`doc/herdr-nvim.txt`).
- CI on every push: formatting, clippy, the Rust tests, and the headless
  Neovim suite against Neovim 0.11 and stable, plus an MSRV build.
- `make test` runs everything CI does; `make lua T=<name>` runs one Neovim
  check. `CONTRIBUTING.md` documents the workflow and `e2e.sh`'s side effects.
- `:checkhealth herdr-nvim` now detects the two most common first-run
  failures — the plugin not being registered with Herdr, and no key bound to
  `herdr-nvim.toggle` — and verifies Herdr meets the declared minimum version.
- Slow operations announce themselves: a bridge call that takes longer than
  400 ms now says what it is waiting for instead of leaving the editor silent
  for up to ten seconds.

### Changed

- Every Herdr call goes over the session socket instead of spawning the
  `herdr` CLI (~8.7 ms → ~1.0 ms per call; a sidebar toggle makes 15–20).
- The daemon is driven over its own msgpack-rpc socket instead of spawning
  `nvim --server --remote-expr` and polling for it (~61 ms → ~0.07 ms per
  call, on the `edit`, pane-title and picker-open paths).
- The file picker renders only the visible window rather than every match, and
  filtering is debounced and narrows the previous result set. On a 20,000-file
  repository the render step went from ~25 ms per keystroke to ~0.01 ms.
- The picker's git scans run concurrently instead of one after another
  (`pick-file` 71 ms → 40 ms here).
- `herdr-nvim agents` makes one subprocess call instead of five: the git
  queries for the prompt header are a single `rev-parse`, and `ps` is asked
  once per scan rather than once per daemon record.
- The annotation list preview is debounced, so holding `j` no longer loads a
  file buffer per row.

### Fixed

- Auto-reload silently stopped working for a directory once any buffer from it
  was wiped: `:bwipeout` fires both `BufDelete` and `BufWipeout`, and the
  watch reference was released twice.
- The picker highlighted the wrong columns on paths containing multibyte
  characters. `matchfuzzypos()` reports character indices that do not count
  combining marks, which needs `byteidx()`, not `vim.str_byteindex()`.
- The picker listed files in a different order every time it opened, because
  dirty and committed paths were iterated straight out of a hash set.
- `HERDR_NVIM_LOCK_TIMEOUT_MS` had no effect when set in `config.env`, the
  only place it was documented: it was read from the process environment,
  which Herdr does not forward to a plugin action.
- A second `:HerdrPreview` with the first split still open raised E95.
- Three libuv timers were stopped but never closed, leaking a handle each; the
  picker leaked one per deferred open.
- A second send could start while the "which agent?" prompt was still open.
- Unprotected calls that could throw out of a keymap or autocommand are now
  guarded: `:drop` in the picker, the split fallback in `]a`/`[a`, and cursor
  placement in the annotation list.
- `:checkhealth` no longer aborts when `$HOME` is unset.

### Removed

- The `herdr` CLI subprocess transport, which had no timeout and could hang a
  plugin action indefinitely.
- `tests/fixtures/agent_output_claude.txt`, which no test referenced, and a
  `.gitignore` entry for a `config.env` no code reads.

## [0.1.0] - 2026-08-28

Initial release: per-tab Neovim sidebars backed by headless daemons, code
annotations you can send to any agent in the workspace, auto-reload of files
agents edit, a fuzzy picker over the files an agent touched, `edit file:line`
from any pane, and Ctrl-clickable paths.

[Unreleased]: https://github.com/jtnovellis/herdr-nvim/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jtnovellis/herdr-nvim/releases/tag/v0.1.0
