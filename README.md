# herdr-nvim

Neovim integration for [Herdr](https://herdr.dev).

- **Full-height Neovim sidebar, one key to toggle.** Your panes move into
  the left half and nvim takes the right, full height; toggle it off and
  Herdr restores the original layout. Each tab gets its own Neovim, backed
  by a headless daemon that survives the toggle — buffers, cursor, undo, LSP
  and pending annotations are all still there when you reopen it. Herdr
  stops the daemons of closed tabs automatically (saving unsaved buffers
  first).
- **Fuzzy file picker.** `prefix+f` opens on the files your agent touched
  this session (newest first, with diff stats); type to fuzzy-search the
  whole repo; `⏎` opens the file in the sidebar at the right line.
- **Code annotations for agents.** Comment lines or a selection like a code
  review, then paste or send them all to any agent in the workspace (Claude
  Code, Codex, pi, OpenCode, …) with `file:line`, the code, and git context.
- **Files edited by agents reload in the sidebar** while your pending
  annotations stay attached.

The Herdr side is a single Rust binary that serves every manifest entrypoint;
the Neovim side is a small Lua plugin that shells out to that binary.

## Requirements

- Herdr 0.8.2 or newer, on Linux or macOS
- Neovim 0.11 or newer (`--remote-ui` and `:detach`)
- A Rust toolchain (`cargo`, 1.82+) to build the binary
- `git` for the branch/repo context in prompts (optional)

## Install

### 1. The Herdr plugin

From GitHub (Herdr runs `cargo build --release` during install):

```sh
herdr plugin install jtnovellis/herdr-nvim
```

Or from a local checkout while developing (`plugin link` does not build):

```sh
cargo build --release
herdr plugin link .
```

Bind the toggle in your Herdr `config.toml`. Herdr's built-in
`edit_scrollback` action uses `prefix+e` by default (it opens the pane's
scrollback in `$EDITOR`, which looks like "nvim without my project"), so move
it when you take that key:

```toml
[keys]
edit_scrollback = "prefix+shift+e"

[[keys.command]]
key = "prefix+e"
type = "plugin_action"
command = "herdr-nvim.toggle"
description = "toggle Neovim sidebar"
```

Then `herdr server reload-config`. The sidebar starts like `nvim` with no
arguments (your dashboard); set `HERDR_NVIM_ARGS=.` in `config.env` to start
on the project directory instead (oil/netrw/explorer, depending on your setup).

And the file picker:

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "herdr-nvim.pick-file"
description = "open file from agent output"
```

Actions are also available from the CLI and the pane menu:

```sh
herdr plugin action invoke toggle --plugin herdr-nvim   # show / hide
herdr plugin action invoke open   --plugin herdr-nvim   # open or focus
herdr plugin action invoke close  --plugin herdr-nvim   # hide, keep daemon
herdr plugin action invoke edit   --plugin herdr-nvim   # open the selected file:line
herdr plugin action invoke pick-file --plugin herdr-nvim # file picker
herdr plugin action invoke gc     --plugin herdr-nvim   # stop orphaned daemons
```

### 2. The Neovim plugin

The same repository is the Neovim plugin. With lazy.nvim:

```lua
{
  "jtnovellis/herdr-nvim",
  build = "cargo build --release",
  opts = {},
}
```

Or point at the checkout that Herdr links:

```lua
{ dir = "~/developer/herdr-nvim", build = "cargo build --release", opts = {} }
```

Inside a sidebar the daemon exports `HERDR_NVIM_BIN`, so the Lua plugin finds
the binary Herdr built. Elsewhere it looks for `target/release/herdr-nvim`
next to the plugin, then `herdr-nvim` on `$PATH`, or the `binary` option.
`:checkhealth herdr-nvim` shows what was found.

## The sidebar

Press your toggle key in any tab:

- The first time, herdr-nvim starts `nvim --headless --listen <socket>` for
  that tab (cwd = the focused pane's directory). It then parks the tab's
  other panes in a temporary tab, splits the remaining pane so nvim gets the
  full-height right half (`HERDR_NVIM_SIDE=left` for the left), and rebuilds
  your original arrangement — same splits, same ratios — inside the other
  half. A layout that cannot be rebuilt (rare, non-rectangular nesting)
  falls back to a split beside the focused pane, with a toast.
- Toggling again closes the pane only; Herdr gives your panes the full
  width back with their ratios intact. The daemon, its buffers, jumplist,
  undo history, and LSP clients keep running. If a toggle is interrupted
  half-way (crash, kill), the next herdr-nvim command finishes moving the
  panes back before doing anything else.
- Toggling once more attaches a fresh UI to the same daemon. Everything is
  where you left it.
- Every tab has its own daemon, so sidebars in different tabs are independent.
- Closing a tab or workspace stops its daemon (`tab.closed` /
  `workspace.closed` hooks). Modified buffers are written first (`:wall`);
  set `HERDR_NVIM_SAVE_ON_CLOSE=0` to discard them instead — either way a
  Herdr toast tells you what happened. `startup` reconciles after Herdr
  restarts, and `gc` does the same on demand.

Inside the sidebar, `:q` on the last window detaches the client instead of
quitting the daemon (`:detach` and `:HerdrDetach` do the same); `:qa` quits
the daemon itself, like in any Neovim, and the next toggle starts a new one.

Daemons run in their own session (`setsid`), so they survive Herdr restarts.
Herdr restores the sidebar's slot as a plain shell after a restart; press the
toggle key to attach a sidebar to the still-running daemon again. The
plugin verifies a pane is really its own sidebar (by terminal identity and
the process running in it) before ever closing one.

### The file picker

`prefix+f` (or `:HerdrPickFile` / `<leader>af` inside the sidebar) pops a
fuzzy picker in the sidebar:

- **No query:** the files touched this session, newest first. It mines the
  agent's session log (Claude Code and pi are understood, including Claude
  subagent logs) and adds uncommitted git changes; for agents Herdr does not
  track, it scrapes recent pane output instead. The cursor starts on the
  newest file, so `⏎` opens it with no typing.
- **Typing:** fuzzy matches across the whole repo (`git ls-files`, honouring
  `.gitignore`), ranked best first.

Rows show the path relative to the agent's directory, a `new` badge for files
created this session, `+N -M` diff stats for uncommitted edits, and how long
ago the file was touched. Files outside the repo (a plan file, say) are
listed after the in-repo ones. The agent is the focused pane if it is one,
else the agent sharing the tab, else the workspace's lone agent; with several
candidates `:HerdrPickFile` asks which.

### Ctrl-click a file link

`file://` links agents print (Claude Code does) open in the sidebar on
Ctrl-click instead of the browser. Plain `src/main.rs:42` text is handled by
the same action where Herdr linkifies it; the picker covers the rest.

### Open a file in the sidebar from anywhere

From a shell in any pane of the tab:

```sh
herdr-nvim edit src/main.rs:42        # opens/focuses the sidebar at line 42
herdr-nvim edit src/main.rs:42:7 --no-focus
```

(`herdr-nvim` is `target/release/herdr-nvim` in the plugin directory; add it
to `$PATH` or alias it.) The `edit` action does the same for the text
selected in a pane, or for a Ctrl-clicked `file://` link.

### Configuration

Copy `config.env.example` to the plugin config directory and edit it:

```sh
cp config.env.example "$(herdr plugin config-dir herdr-nvim)/config.env"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `HERDR_NVIM_NVIM` | `nvim` | Neovim executable |
| `HERDR_NVIM_SIDE` | `right` | `right` or `left` (which half the full-height sidebar takes) |
| `HERDR_NVIM_WIDTH` | `0.45` | Sidebar width as a fraction of the tab |
| `HERDR_NVIM_ARGS` | | Extra daemon arguments (e.g. `-u ~/.config/nvim-herdr/init.lua`) |
| `HERDR_NVIM_SAVE_ON_CLOSE` | `1` | Write modified buffers before a closed tab's daemon stops |
| `HERDR_NVIM_GRACE_MS` | `1500` | Wait between `:qall` → `:qall!` → SIGTERM → SIGKILL |
| `HERDR_NVIM_MAX_SNIPPET_LINES` | `80` | Code lines included per annotation |
| `HERDR_NVIM_LOCK_TIMEOUT_MS` | `3000` | How long commands wait for the state lock |
| `HERDR_NVIM_PICKER_SCAN_LINES` | `300` | Pane lines scanned for paths when there is no session log |
| `HERDR_NVIM_PICKER_MAX_FILES` | `20` | Session files shown before you type |

Environment variables with the same names override the file.

## Annotations

| Mapping | Action |
| --- | --- |
| `<leader>ac` | comment the current line / visual selection (on an existing one: edit it) |
| `<leader>al` | list comments (float): hover to preview, `⏎` edit, `d` delete, `o` go there, `s` paste, `S` send, `q` back to where you were |
| `<leader>as` | paste all comments into the agent's input |
| `<leader>aS` | send all comments to the agent (auto-submits) |
| `]a` / `[a` | jump to the next / previous comment |
| `<leader>af` | file picker (files the agent touched, then the whole repo) |

Commands: `:HerdrAnnotate` (accepts a range), `:HerdrAnnotations`,
`:HerdrPaste[!]`, `:HerdrSend[!]`, `:HerdrPreview`, `:HerdrNext`, `:HerdrPrev`,
`:HerdrClear`, `:HerdrAgents`, `:HerdrPickFile`. The `!` forms send even to an agent that is
waiting at an approval prompt.

Sending skips the picker when the target is obvious: the lone agent in the
workspace, or the single agent sharing this tab (the sibling pane). The
picker (`vim.ui.select`) only appears when two or more agents could plausibly
be meant — or when the workspace has no agent at all, in which case agents
from other workspaces are offered.

Comments are ephemeral by design: in-memory only, extmark-tracked (they follow
your edits, survive reloads, and come back with undo), cleared after a
successful send. A comment whose lines were deleted or rewritten shows up
dimmed (`~`) in the list and is not sent; a pasted comment is kept and marked
`✓` until the next send or `:HerdrClear`.

`:HerdrPreview` shows exactly what the agent will receive:

```
Code annotations from Neovim (2 comments) — repo herdr-nvim, branch main @ 3f2a9c1
Root: /Users/me/developer/herdr-nvim

## 1. src/git.rs:12-14
Comment: should this also capture the upstream?
```rust
fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
```

## 2. lua/herdr-nvim/init.lua:3 (buffer has unsaved changes)
Comment: rename module
```lua
local M = {}
```

Please address each annotation above; refer to them by number.
```

Paths are relative to the repo root; when the chosen agent works outside that
repo (another worktree, say) they are sent absolute, with a note.

`<leader>aS` uses `herdr agent prompt`; `<leader>as` inserts the text as one
bracketed-paste block without submitting. Both refuse to type into an agent
that is blocked at an approval prompt (use `!` to force). When Herdr does not
consider the pane a promptable agent, sending falls back to raw input plus
Enter.

### Statusline

```lua
require("herdr-nvim").statusline()  -- "● 3" while comments are pending, else ""
```

The plugin fires `User HerdrNvimAnnotationsChanged` whenever the set changes:

```lua
-- lualine
sections = { lualine_x = { require("herdr-nvim").statusline } },
options = { refresh = { events = { "User HerdrNvimAnnotationsChanged" } } },
-- heirline
{ provider = function() return require("herdr-nvim").statusline() end,
  update = { "User", pattern = "HerdrNvimAnnotationsChanged" } },
```

### Setup options

```lua
require("herdr-nvim").setup({
  keymaps = true,            -- true | false | "force" (override existing mappings)
  binary = nil,              -- explicit path to the herdr-nvim binary
  prompt = "Comment: ",
  clear_on_send = true,      -- forget comments after <leader>aS
  clear_on_paste = false,    -- after <leader>as: keep them, marked delivered
  focus_after_send = false,  -- jump to the agent pane after delivery
  notify = true,
  signs = false,             -- use the sign column instead of the line number
  statusline_icon = "●",
  -- sidebar daemon only:
  quit_guard = true,         -- :q on the last window detaches instead of quitting
  reload = { enabled = true, debounce_ms = 200 },  -- reload files agents edit
  pane_title = true,         -- show the current file as the Herdr pane title
})
```

Default mappings never override an existing mapping on the same keys (use
`keymaps = "force"`). Set `vim.g.herdr_nvim_no_defaults = true` before the
plugin loads to skip the automatic `setup()`. Highlights: `HerdrNvimAnnotation`,
`HerdrNvimSign`, `HerdrNvimVirt`, `HerdrNvimListLoc`, `HerdrNvimStale`,
`HerdrNvimDelivered`.

### Reloading agent edits

In the sidebar daemon the plugin watches the directories of your open files
(libuv `fs_event`) and runs `:checktime` shortly after anything changes, so a
file an agent edited reloads within a fraction of a second. Buffers you have
modified are never clobbered: they are flagged once with a message, and
`:e!` reloads them when you are ready. Reopening the sidebar also reloads.

## How it works

```
herdr action "toggle"                  herdr pane (split)
  herdr-nvim toggle ───spawns──▶ nvim --headless --listen <sock>   (per tab, setsid)
        │                              ▲
        └── plugin pane open ──▶ herdr-nvim sidebar ──▶ nvim --server <sock> --remote-ui
                                       │
                          <leader>aS ──┴──▶ herdr-nvim send ──▶ herdr agent prompt <pane>
```

- State lives in `~/.local/state/herdr/plugins/herdr-nvim/daemons.json` (pid,
  socket, cwd, sidebar pane + terminal id, per tab, scoped by Herdr session),
  guarded by `flock`. `herdr-nvim status` prints it with liveness from any
  Herdr pane.
- Sockets live in `$XDG_RUNTIME_DIR/herdr-nvim` or the system temp directory;
  daemon output goes to `…/herdr-nvim/logs/<session>-<tab>.log`.
- Before signalling a pid the plugin checks with `ps` that it still runs our
  daemon (socket path and start time), so a reused pid is never killed.
- The sidebar is placed with `plugin pane open --placement split` next to
  the lone anchor pane, swapped to the left when configured, and sized
  through `layout.set_split_ratio`; the other panes travel through a parking
  tab (`pane move`) and come back along a guillotine plan computed from
  their rectangles. The plan, the parked panes and the parking tab are
  recorded in `daemons.json` so an interrupted toggle is finished by the
  next command or by `gc`.
- The layout planner, session-log mining, git scanning and path extraction
  are adapted from [ChmaraX/herdr-nvim](https://github.com/ChmaraX/herdr-nvim)
  (MIT) — see `THIRD_PARTY.md`.
- The binary talks to Herdr through `HERDR_BIN_PATH` and, for the few methods
  the CLI does not expose, the session socket. Failures of actions and hooks
  are shown as Herdr toasts.

## Troubleshooting

- `:checkhealth herdr-nvim` — binary, Herdr connection, context, daemon,
  agents, annotations.
- `herdr plugin log list --plugin herdr-nvim` shows every action, hook, and
  their stdout/stderr.
- `herdr-nvim status` shows daemons and whether they are alive.
- "no agent is running in Herdr": `herdr agent list` must show a detected agent.
- The sidebar opens but is blank: check `~/.local/state/herdr/plugins/herdr-nvim/logs/`.

## Development

```sh
cargo build --release && cargo test && cargo clippy --all-targets -- -D warnings
scripts/lua-tests.sh     # headless Neovim checks
scripts/e2e.sh           # full run against a throwaway headless Herdr session
herdr plugin link .
```

## License

MIT
