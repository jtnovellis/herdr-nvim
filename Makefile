# herdr-nvim — see CONTRIBUTING.md
#
#   make            build the release binary
#   make test       everything CI runs (fmt, clippy, cargo test, Lua tests)
#   make e2e        the full end-to-end suite (needs herdr + nvim + python3)

CARGO ?= cargo
NVIM  ?= nvim
BIN   := target/release/herdr-nvim

.DEFAULT_GOAL := build
.PHONY: build test fmt fmt-check lint unit lua e2e clean install help

build: $(BIN)

$(BIN): $(wildcard src/*.rs) Cargo.toml Cargo.lock
	$(CARGO) build --release

## Everything CI runs.
test: fmt-check lint unit lua

fmt:
	$(CARGO) fmt
	@command -v stylua >/dev/null && stylua lua plugin tests/lua || \
		echo "  (stylua not installed: cargo install stylua)"

fmt-check:
	$(CARGO) fmt --check
	@command -v stylua >/dev/null && stylua --check lua plugin tests/lua || \
		echo "  (stylua not installed, skipping Lua format check)"

lint:
	$(CARGO) clippy --all-targets -- -D warnings

unit:
	$(CARGO) test

## Headless Neovim checks. `make lua T=picker` runs a single file.
lua: $(BIN)
	scripts/lua-tests.sh $(T)

## Drives a real throwaway Herdr session. Mutates ~/.local/state; not run in CI.
e2e:
	scripts/e2e.sh

install: $(BIN)
	$(CARGO) install --path . --locked

clean:
	$(CARGO) clean

help:
	@echo 'herdr-nvim make targets:'
	@echo '  build       compile the release binary (default)'
	@echo '  test        everything CI runs: fmt-check, lint, unit, lua'
	@echo '  fmt         format Rust and Lua in place'
	@echo '  fmt-check   check formatting without writing'
	@echo '  lint        cargo clippy --all-targets -D warnings'
	@echo '  unit        cargo test'
	@echo '  lua         headless Neovim checks; make lua T=picker for one file'
	@echo '  e2e         full end-to-end suite against a throwaway Herdr session'
	@echo '  install     cargo install --path .'
	@echo '  clean       cargo clean'
