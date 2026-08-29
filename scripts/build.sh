#!/usr/bin/env sh
# Put a herdr-nvim binary at target/release/herdr-nvim, which is where the
# manifest's actions look for it.
#
# Tries a prebuilt release asset first so a user does not need a Rust
# toolchain, and falls back to `cargo build --release`. Run by `[[build]]` in
# herdr-plugin.toml during `herdr plugin install`.
#
#   HERDR_NVIM_NO_DOWNLOAD=1   always build from source
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/target/release/herdr-nvim"
REPO="jtnovellis/herdr-nvim"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/herdr-plugin.toml" | head -1)"

build_from_source() {
  # A plugin action does not get an interactive shell's PATH, so rustup's
  # default location often is not on it.
  if ! command -v cargo >/dev/null 2>&1; then
    for candidate in "${CARGO_HOME:-$HOME/.cargo}/bin" /usr/local/cargo/bin; do
      if [ -x "$candidate/cargo" ]; then
        PATH="$candidate:$PATH"
        export PATH
        break
      fi
    done
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    echo "herdr-nvim: no prebuilt binary for this platform and cargo is not installed." >&2
    echo "herdr-nvim: install Rust (https://rustup.rs) and re-run, or download a" >&2
    echo "herdr-nvim: release from https://github.com/$REPO/releases" >&2
    exit 1
  fi
  echo "herdr-nvim: building from source"
  cd "$ROOT" && exec cargo build --release
}

[ "${HERDR_NVIM_NO_DOWNLOAD:-0}" = "1" ] && build_from_source

case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux)  os="unknown-linux-gnu" ;;
  *)      build_from_source ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64)  arch="x86_64" ;;
  *)             build_from_source ;;
esac

target="$arch-$os"
url="https://github.com/$REPO/releases/download/v$VERSION/herdr-nvim-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "herdr-nvim: trying $url"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL --retry 2 "$url" -o "$tmp/dist.tar.gz" 2>/dev/null || {
    echo "herdr-nvim: no release asset for $target (v$VERSION)"
    build_from_source
  }
elif command -v wget >/dev/null 2>&1; then
  wget -q "$url" -O "$tmp/dist.tar.gz" 2>/dev/null || {
    echo "herdr-nvim: no release asset for $target (v$VERSION)"
    build_from_source
  }
else
  build_from_source
fi

tar -xzf "$tmp/dist.tar.gz" -C "$tmp" || build_from_source
[ -f "$tmp/herdr-nvim" ] || build_from_source

mkdir -p "$(dirname "$OUT")"
# Replace via a temporary name so a running action never sees a partial file.
cp "$tmp/herdr-nvim" "$OUT.new"
chmod +x "$OUT.new"
# Refuse a download that cannot run here (wrong arch, missing libc).
if ! "$OUT.new" version >/dev/null 2>&1; then
  rm -f "$OUT.new"
  echo "herdr-nvim: the downloaded binary does not run here" >&2
  build_from_source
fi
mv "$OUT.new" "$OUT"
echo "herdr-nvim: installed prebuilt $target binary"
