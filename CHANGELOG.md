# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-08-31

### Added

- **The agent's answer now comes back to Neovim.** `<leader>ac` opens a float
  that does not take your focus, showing what the agent is doing and then what
  it said; `q` closes it and `:HerdrReplyView` moves into it. The claim this
  replaces — that `agent.prompt` answers with lifecycle state and never text,
  so there was nothing to stream back — was true of that call and wrong about
  the platform. Herdr reports the transcript behind every agent pane, and it
  is append-only, so `ask` now records its length at the moment it sends and
  everything past that offset is the reply to that message. The transcript is
  written a message at a time, so the reply arrives whole rather than
  streaming, and only Claude Code and pi transcripts can be read today; for
  any other agent nothing opens and its own pane stays the place to read.
  New option `reply = { enabled = true }`; `focus_after_ask` now defaults to
  `false`.
- **Live agent status.** The plugin subscribes to Herdr's
  `pane.agent_status_changed` and `pane.agent_detected` and pushes the state
  into the tab's Neovim, so the statusline shows `◑ working`, `⏸ blocked` or
  `✓ done` without polling. New `User` autocommands `HerdrNvimAgentStatus`,
  `HerdrNvimAgentReply` and `HerdrNvimReviewChanged`, all carrying a payload
  (`HerdrNvimAnnotationsChanged` still carries none).
- **Step through what the agent changed.** The same transcript records every
  edit with its before and after, so they are marked where they landed: `]r`
  and `[r` move between them, `<leader>au` puts one back the way it was and
  `<leader>ak` clears the mark. Reverting refuses when the text is no longer
  what the agent wrote, so it cannot clobber an edit of your own, and a file
  the agent created says it has nothing to revert to. New option
  `review = { enabled = true }`, commands `:HerdrNextEdit`, `:HerdrPrevEdit`,
  `:HerdrRevertEdit`, `:HerdrKeepEdit`, `:HerdrKeepEdits`, and highlights
  `HerdrNvimReview`/`HerdrNvimReviewVirt`. `]r`/`[r` rather than `]h`/`[h`,
  which gitsigns takes buffer-locally in most configurations.
- **`herdr plugin action invoke herdr-nvim setup-keys`** binds `prefix+e` and
  `prefix+f`. A manifest cannot declare keybindings, so a fresh install left
  the sidebar reachable only through `plugin action invoke`. It backs up
  `config.toml` first, skips any key already bound to something else, and
  reloads the config so the keys work straight away; `:checkhealth` now names
  it instead of describing the problem. `startup` also seeds `config.env` from
  `config.env.example` the first time, so the documented knobs exist where
  they are read. Neither ever overwrites what is already there.
- `@` in the ask composer attaches another file, through the existing file
  picker rather than a second one. `<leader>at` and `<leader>ag` reach
  `:HerdrAskTarget` and `:HerdrAgents`, which had no mappings.
- `herdr-nvim tail --path P [--agent KIND] [--from BYTES]` prints what an agent
  said and edited past a byte offset in its transcript, as JSON.

- `:checkhealth herdr-nvim` reports whether the reply view and edit review are
  on, what the agent was last seen doing, and that only Claude Code and pi
  transcripts can be read -- so an agent whose answer never appears is a known
  limitation rather than a mystery. It also errors when the binary predates
  `tail`, the shape "updated the Lua, did not rebuild" takes now.
- Updating and uninstalling are documented, including what an uninstall leaves
  behind (your `config.env`, the state directory, and the keys `setup-keys`
  wrote) and that a running sidebar keeps the Lua its daemon started with.

### Changed

- `focus_after_ask` defaults to `false`: the agent's pane was focused because
  there was nowhere else to read the answer, and now there is.
- The bundled Lua is found through Herdr's own `HERDR_PLUGIN_ROOT` before
  falling back to guessing from the binary's path. The guess cannot survive the
  binary being copied or symlinked out of the checkout, which silently left a
  sidebar with no `:HerdrAsk`; a reported root that does not actually carry the
  Lua is still ignored rather than trusted.

### Fixed

- An agent edit never found its buffer when the two spellings of the path
  differed -- `/tmp` against `/private/tmp` on macOS, where they name the same
  file. Both sides now resolve before they are compared.
- A released agent left a final `unknown` state behind that could hide a pane
  that was still working, because the statusline took the most recently updated
  row and `os.time()` ties are common at second resolution.

## [0.1.0] - 2026-08-29

The first published release: per-tab Neovim sidebars backed by headless
daemons, code annotations you can send to any agent in the workspace,
auto-reload of files agents edit, a fuzzy picker over the files an agent
touched, `edit file:line` from any pane, and Ctrl-clickable paths.

Sections below record how the plugin got here; there is no earlier release to
compare against.

### Added

- **Ask an agent about the code you are looking at.** `<leader>ac` (Normal or
  Visual) opens a composer float over the current line or selection; type a
  message, press `<C-s>`, and it goes straight to the agent with `file:line`,
  the code and git context attached — no queue to flush. `<leader>ar`
  (`:HerdrReply`) continues the conversation with no code attached. The
  agent's pane is remembered so follow-ups need no picker, and is forgotten
  and re-resolved automatically when it goes away. New commands
  `:HerdrAsk[!] [message]` (accepts a range; a message on the command line
  skips the composer), `:HerdrReply[!] [message]` and `:HerdrAskTarget[!]`;
  new options `ask_height`, `ask_send_key`, `focus_after_ask`; new highlight
  `HerdrNvimAskRange`. The reply appears in the agent's own Herdr pane —
  `agent.prompt` answers with lifecycle state, never text, so there is nothing
  to stream back into Neovim.
- **`herdr plugin install` is now the whole installation.** The Herdr plugin
  always shipped the Lua half in `lua/`, but the sidebar daemon spawned a bare
  `nvim` that never had it on the runtimepath, so a user who installed only
  through Herdr got a sidebar with no `:HerdrAsk`, no annotations, no reload
  watcher — and a `pick-file` that gathered its candidates and then failed on
  the `luaeval` call into the daemon. The daemon now falls back to the bundled
  copy, guarded by a `pcall(require, "herdr-nvim")` that runs *after* the
  user's config so an install of their own always wins and the two never both
  land on the runtimepath. Installing the Neovim plugin separately is now
  optional (see |herdr-nvim-bundled-lua|).
- `:checkhealth herdr-nvim` reports when the Rust binary predates `:HerdrAsk`,
  which is what "new Lua, old binary" looks like after an update that did not
  re-run the build step.
- Prebuilt release binaries for macOS (arm64, x86_64) and Linux (x86_64,
  aarch64, statically linked against musl so they run on any distribution).
  `herdr plugin install` now downloads one instead of requiring a Rust
  toolchain, falling back to `cargo build --release --locked` when there is no
  matching asset. `HERDR_NVIM_NO_DOWNLOAD=1` forces the source build.
- **The install verifies what it downloads.** `scripts/build.sh` checks the
  asset's SHA-256 against the `SHA256SUMS` published with the same release,
  pins transport to https across redirects, extracts exactly the one expected
  member without restoring its ownership or permission bits, and **aborts on a
  mismatch** instead of quietly falling back to a source build. Release assets
  carry Sigstore build provenance, verifiable with `gh attestation verify`.
- [SECURITY.md](SECURITY.md) — the trust model, a disclosure process, and what
  the daemon socket and agent-scraped paths do and do not guarantee.
- `:help herdr-nvim` — a full vimdoc reference (`doc/herdr-nvim.txt`).
- CI on every push: formatting, clippy, the Rust tests, and the headless
  Neovim suite against Neovim 0.11 and stable, plus an MSRV build, a
  `cargo deny` pass over advisories/licenses/sources, a musl build of the
  release target, and behavioural tests for `scripts/build.sh`'s refusal
  paths. Every action is pinned by commit SHA, every workflow declares a
  least-privilege `permissions` block, and Dependabot keeps both the crates
  and those pins current.
- The release workflow attests build provenance, re-verifies every artifact's
  checksum before publishing, uploads an explicit asset list rather than a
  glob, and can be dry-run from `workflow_dispatch` without cutting a release.
- `make test` runs everything CI does; `make lua T=<name>` runs one Neovim
  check. `CONTRIBUTING.md` documents the workflow and `e2e.sh`'s side effects.
- `:checkhealth herdr-nvim` now detects the two most common first-run
  failures — the plugin not being registered with Herdr, and no key bound to
  `herdr-nvim.toggle` — and verifies Herdr meets the declared minimum version.
- Slow operations announce themselves: a bridge call that takes longer than
  400 ms now says what it is waiting for instead of leaving the editor silent
  for up to ten seconds.

### Changed

- **`<leader>ac` now asks the agent instead of queueing a comment.** Annotating
  moved to `<leader>aa` (Normal and Visual); everything else about the queue —
  `<leader>al`, `<leader>as`, `<leader>aS`, `]a`/`[a`, `:HerdrAnnotate` and
  friends — is unchanged.
- The blocked-agent message no longer names `:HerdrSend!` specifically, since
  `:HerdrAsk!` reaches the same check.
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

[Unreleased]: https://github.com/jtnovellis/herdr-nvim/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jtnovellis/herdr-nvim/releases/tag/v0.2.0
[0.1.0]: https://github.com/jtnovellis/herdr-nvim/releases/tag/v0.1.0
