# Security Policy

## Supported versions

Only the latest release. Fixes ship as a new patch release; older tags are not
patched.

| Version | Supported |
|---------|-----------|
| latest  | ✅ |
| older   | ❌ superseded — upgrade |

## Reporting a vulnerability

Report privately via GitHub Security Advisories:
<https://github.com/jtnovellis/herdr-nvim/security/advisories/new>

Please don't open a public issue. Expect an acknowledgement within 72 hours and
a fix or a plan within 14 days. Coordinated disclosure; credit on request.

## Trust model

Read this before installing. It is written to be honest about what is and is
not guaranteed, rather than reassuring.

### Installing

`herdr plugin install` clones this repository and runs `scripts/build.sh` **as
you**. That script downloads a prebuilt binary and checks its SHA-256 against a
`SHA256SUMS` file served from the same GitHub release.

- That detects **corruption or substitution in transit**. It does **not** prove
  authorship: anyone who could publish to that release could supply a matching
  pair.
- Transport is pinned to `https`, including across redirects, so the download
  cannot be silently downgraded to plaintext.
- A checksum mismatch **aborts the install**. It never quietly falls back to a
  source build.
- Exactly one member named `herdr-nvim` is extracted from the archive, without
  restoring its ownership or permission bits.
- To check provenance — which workflow, at which commit, produced a file:

  ```sh
  gh attestation verify --repo jtnovellis/herdr-nvim \
    herdr-nvim-<target>.tar.gz
  ```

  Release binaries carry Sigstore build provenance.
- If no prebuilt binary matches your platform, the script runs
  `cargo build --release --locked`, which executes the build scripts and proc
  macros of the dependency tree on your machine. That is inherent to building
  from source. `HERDR_NVIM_NO_DOWNLOAD=1` forces this path.

### Running

**The sidebar is a real Neovim running your own config.** The daemon spawns
`nvim` the way you would, so your plugins, your `init.lua` and your LSP servers
run with your privileges — exactly as they do in any other terminal. This
plugin does not sandbox them and does not try to.

When your config does not already provide the Neovim half, the daemon falls
back to the copy bundled in `lua/`, loaded after your config so an install of
your own always wins. That fallback is code from this repository, at the
version you installed.

**The daemon socket is the security boundary.** Each tab's Neovim listens on a
unix socket, and anything that can connect to it can drive that Neovim over
msgpack-RPC — which includes evaluating arbitrary Lua as you. The sockets live
in a per-uid directory (`$XDG_RUNTIME_DIR/herdr-nvim`, else
`$TMPDIR/herdr-nvim-<uid>`) that the daemon creates, refuses to use if it is
owned by another user, and chmods to `0700` if it is group- or
world-accessible. On a machine where you are the only user this is a
non-issue; on a shared host, a pre-existing directory you do not own is
refused rather than reused.

**Agent output is untrusted input.** The file picker and the `file-path` /
`file-url` link handlers act on paths scraped from an agent's pane. Those paths
come from a model's output, so treat them as attacker-influenced text:

- A scraped path is only offered if it resolves to a file that already exists.
  The plugin does not create, move or execute anything it finds this way.
- Opening a file is not executing it — but Neovim's `modeline` is on by
  default, so a crafted first or last line of a file can set buffer options.
  `modelineexpr` is off by default, which is what keeps a modeline from
  evaluating expressions; if you have turned it on, opening an untrusted file
  in any Neovim is already an execution vector, this plugin included.
- Paths are passed to Neovim through `fnameescape`, not concatenated into an
  Ex command as-is.

### What crosses the boundary

`<leader>ac`, `<leader>ar` and `<leader>aS` **send your code out of the
editor**. The selected lines, the `file:line`, and the surrounding git context
(branch, repo) are written into the prompt of whichever agent pane you pick.
That agent is usually a hosted model, so treat this exactly like pasting the
same text into that agent yourself: don't do it with a file holding
credentials, and remember that the buffer you are sitting in may contain more
than you meant to send.

Nothing is sent anywhere until you press the send key. The plugin has no
telemetry and makes no network requests of its own — `scripts/build.sh` at
install time is the only one, and it talks only to GitHub releases.

### Not in scope

Anything that requires an attacker who can already run code as you, write to
your Neovim config or your checkout, or connect to your user's runtime
directory. A malicious Neovim plugin or LSP server you have installed yourself
is outside this boundary — it is already inside it.
