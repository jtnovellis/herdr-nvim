# Contributing to herdr-nvim

The project is two halves that ship together: a Rust binary (`src/`) that Herdr
invokes as a plugin action, and a Neovim plugin (`lua/`, `plugin/`) that runs
inside the headless nvim daemon behind the sidebar.

## Setup

```sh
cargo build --release      # the manifest's actions call ./target/release/herdr-nvim
herdr plugin link .        # register this checkout with Herdr
```

Requires Rust >= 1.82 (matches `rust-version` in `Cargo.toml`), Neovim >= 0.11,
and Herdr >= 0.8.2.

Optional but recommended: `cargo install stylua` for Lua formatting.

## The one command

```sh
make test
```

That is exactly what CI runs: `cargo fmt --check`, `cargo clippy --all-targets
-D warnings`, `cargo test`, and the headless Neovim suite. `make help` lists the
rest.

## Tests

**Rust** — unit tests live inline in `#[cfg(test)] mod tests`. Fixtures in
`tests/fixtures/` are pulled in with `include_str!`, so they are compiled into
the test binary. The pure modules (`layout.rs`, `candidates.rs`, `extract.rs`,
`gitscan.rs`'s parsers) are the easy places to add coverage — keep shell-outs
and parsing separate the way those modules already do.

**Lua** — one file per check in `tests/lua/`, each run in its own
`nvim --clean --headless` process with fresh fixtures. See
`tests/lua/README.md` for the globals the prelude provides. Run one file with:

```sh
make lua T=picker         # or: scripts/lua-tests.sh picker
```

The suite needs only `nvim`. Checks that additionally want
`target/release/herdr-nvim` or the `herdr` CLI relax their assertions when those
are missing, so it passes on a bare CI runner.

**End-to-end** — `make e2e` drives a real throwaway Herdr session (`hn-e2e`).
It needs `herdr`, `nvim`, `cargo`, and `python3`, takes a few minutes, and is
**not** run in CI.

> `scripts/e2e.sh` writes to your real plugin state directory
> (`~/.local/state/herdr/plugins/herdr-nvim/`) and briefly replaces your plugin
> `config.env`, restoring it on exit. Interrupting it partway can leave those
> behind. It also closes every workspace in the `hn-e2e` session during cleanup.

`e2e.sh` is the only coverage the sidebar layout maneuver in `sidebar.rs` has,
so run it by hand before changing `sidebar.rs`, `layout.rs`, or `daemon.rs`.

## Conventions

- Rust is `rustfmt` default with `edition = 2021`; Lua is `stylua.toml`
  (2-space indent, double quotes, 120 columns).
- Dependencies are deliberately minimal — four direct crates. Prefer a small
  hand-rolled helper over a new dependency, and say why in the PR if you add one.
- Code adapted from other projects goes in `THIRD_PARTY.md` with the upstream
  license.
