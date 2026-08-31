#!/usr/bin/env bash
# Behavioural tests for scripts/build.sh.
#
# scripts/build.sh runs on a user's machine during `herdr plugin install`, so
# its refusal paths are the ones worth testing: a failed download must fall
# back to a source build, and a tampered asset must abort rather than fall
# back. Every test stubs `curl` and `cargo`, so nothing here touches the
# network or compiles anything.
#
# Run by CI and by `make scripts`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Mirror build.sh's own platform detection so the asset names line up.
case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) TRIPLE=aarch64-apple-darwin ;;
  Darwin:x86_64)               TRIPLE=x86_64-apple-darwin ;;
  Linux:x86_64)                TRIPLE=x86_64-unknown-linux-musl ;;
  Linux:aarch64|Linux:arm64)   TRIPLE=aarch64-unknown-linux-musl ;;
  *) echo "build-tests: no prebuilt triple for this platform; nothing to test" >&2; exit 0 ;;
esac

# `shasum` on macOS, `sha256sum` on Linux; build.sh accepts either.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'
  fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
passed=0
fail() { echo "  ✗ $1" >&2; exit 1; }
ok() { passed=$((passed + 1)); echo "  ✓"; }

# A stub PATH with no cargo and no curl but the coreutils build.sh needs.
stub_bin() { # dir
  mkdir -p "$1"
  printf '#!/bin/sh\necho "stub cargo $*"\n' > "$1/cargo"
  chmod +x "$1/cargo"
}

# A stub curl that serves a fake release out of $REL.
stub_curl() { # path
  cat > "$1" <<'SH'
#!/bin/sh
for a in "$@"; do case "$prev" in -o) dest=$a;; esac; prev=$a; done
case "$prev" in
  *SHA256SUMS) cp "$REL/SHA256SUMS" "$dest" ;;
  # A release older than v0.2.1 has no COMMIT: 404, like the real thing.
  *COMMIT)     [ -f "$REL/COMMIT" ] || exit 22; cp "$REL/COMMIT" "$dest" ;;
  *)           cp "$REL/asset.tar.gz" "$dest" ;;
esac
SH
  chmod +x "$1"
}

# A throwaway repo root holding just what build.sh reads.
fake_root() { # dir
  mkdir -p "$1/scripts"
  cp "$ROOT/scripts/build.sh" "$1/scripts/"
  cp "$ROOT/herdr-plugin.toml" "$1/"
}

echo "== a failed download falls back to a source build"
d="$WORK/1"; mkdir -p "$d"; stub_bin "$d/bin"
printf '#!/bin/sh\nexit 22\n' > "$d/bin/curl"; chmod +x "$d/bin/curl"
# HOME must not point at a real ~/.cargo/env: sourcing it would put the real
# cargo ahead of the stub and the assertions below would be testing nothing.
HOME=/nonexistent PATH="$d/bin:/usr/bin:/bin" sh scripts/build.sh > "$d/out" 2>&1 || true
grep -q 'building from source instead' "$d/out" || fail "no fallback message"
grep -q 'stub cargo build --release --locked' "$d/out" || fail "cargo was not invoked"
ok

echo "== an unset HOME is survivable"
d="$WORK/2"; mkdir -p "$d"; stub_bin "$d/bin"
printf '#!/bin/sh\nexit 22\n' > "$d/bin/curl"; chmod +x "$d/bin/curl"
env -u HOME PATH="$d/bin:/usr/bin:/bin" sh scripts/build.sh > "$d/out" 2>&1 || true
grep -q 'stub cargo build --release --locked' "$d/out" || { cat "$d/out"; fail "cargo was not invoked"; }
ok

echo "== HERDR_NVIM_NO_DOWNLOAD=1 never reaches the network"
d="$WORK/3"; mkdir -p "$d"; stub_bin "$d/bin"
printf '#!/bin/sh\necho NETWORK >&2\nexit 1\n' > "$d/bin/curl"; chmod +x "$d/bin/curl"
HERDR_NVIM_NO_DOWNLOAD=1 HOME=/nonexistent PATH="$d/bin:/usr/bin:/bin" \
  sh scripts/build.sh > "$d/out" 2>&1 || true
grep -q 'stub cargo build --release --locked' "$d/out" || { cat "$d/out"; fail "cargo was not invoked"; }
! grep -q NETWORK "$d/out" || fail "the download was attempted anyway"
ok

echo "== a non-https origin is refused before anything is downloaded"
d="$WORK/4"; mkdir -p "$d"; stub_bin "$d/bin"
printf '#!/bin/sh\necho NETWORK >&2\nexit 1\n' > "$d/bin/curl"; chmod +x "$d/bin/curl"
HERDR_NVIM_BASE_URL=http://example.invalid HOME=/nonexistent \
  PATH="$d/bin:/usr/bin:/bin" sh scripts/build.sh > "$d/out" 2>&1 || true
grep -q 'non-https download origin' "$d/out" || { cat "$d/out"; fail "the origin was not refused"; }
! grep -q NETWORK "$d/out" || fail "the download was attempted anyway"
ok

echo "== a tampered asset aborts the install"
d="$WORK/5"; mkdir -p "$d/rel"; stub_bin "$d/bin"; stub_curl "$d/bin/curl"; fake_root "$d/root"
printf '#!/bin/sh\necho real\n' > "$d/rel/herdr-nvim"
tar -czf "$d/rel/asset.tar.gz" -C "$d/rel" herdr-nvim
printf '%s  herdr-nvim-%s.tar.gz\n' \
  0000000000000000000000000000000000000000000000000000000000000000 "$TRIPLE" > "$d/rel/SHA256SUMS"
set +e
REL="$d/rel" HOME=/nonexistent PATH="$d/bin:/usr/bin:/bin" \
  sh "$d/root/scripts/build.sh" > "$d/out" 2>&1
code=$?
set -e
sed 's/^/    /' "$d/out"
[ "$code" -eq 1 ] || fail "exit was $code, expected 1"
grep -q 'REFUSING TO INSTALL' "$d/out" || fail "the install was not refused"
! grep -q 'stub cargo' "$d/out" || fail "fell back to a source build after tampering"
[ ! -e "$d/root/target/release/herdr-nvim" ] || fail "a tampered binary was installed"
ok

echo "== a matching checksum installs the prebuilt binary"
d="$WORK/6"; mkdir -p "$d/rel"; stub_bin "$d/bin"; stub_curl "$d/bin/curl"; fake_root "$d/root"
# Must answer `version`: build.sh refuses a download it cannot run.
printf '#!/bin/sh\necho 0.0.0-stub\n' > "$d/rel/herdr-nvim"; chmod +x "$d/rel/herdr-nvim"
tar -czf "$d/rel/asset.tar.gz" -C "$d/rel" herdr-nvim
printf '%s  herdr-nvim-%s.tar.gz\n' "$(sha256_of "$d/rel/asset.tar.gz")" "$TRIPLE" > "$d/rel/SHA256SUMS"
REL="$d/rel" HOME=/nonexistent PATH="$d/bin:/usr/bin:/bin" \
  sh "$d/root/scripts/build.sh" > "$d/out" 2>&1
sed 's/^/    /' "$d/out"
[ -x "$d/root/target/release/herdr-nvim" ] || fail "the binary was not installed"
! grep -q 'stub cargo' "$d/out" || fail "unexpectedly built from source"
ok

echo "== a release built from another commit is not installed over this checkout"
# `plugin install` checks out the default branch, which can be ahead of the
# tag. Downloading by manifest version alone would pair this newer source with
# an older binary; the COMMIT file is how the release says which one it is.
d="$WORK/7"; mkdir -p "$d/rel"; stub_bin "$d/bin"; stub_curl "$d/bin/curl"; fake_root "$d/root"
printf '#!/bin/sh\necho 0.0.0-stub\n' > "$d/rel/herdr-nvim"; chmod +x "$d/rel/herdr-nvim"
tar -czf "$d/rel/asset.tar.gz" -C "$d/rel" herdr-nvim
printf '%s  herdr-nvim-%s.tar.gz\n' "$(sha256_of "$d/rel/asset.tar.gz")" "$TRIPLE" > "$d/rel/SHA256SUMS"
printf '%s\n' '0000000000000000000000000000000000000000' > "$d/rel/COMMIT"
( cd "$d/root" && git init -q && git -c user.email=t@x -c user.name=t commit -q --allow-empty -m x )
REL="$d/rel" HOME=/nonexistent PATH="$d/bin:/usr/bin:/bin" \
  sh "$d/root/scripts/build.sh" > "$d/out" 2>&1 || true
sed 's/^/    /' "$d/out"
grep -q 'not the commit' "$d/out" || fail "the commit mismatch was not detected"
grep -q 'building from source instead' "$d/out" || fail "it did not fall back to source"
ok

echo "== a release built from THIS commit still installs"
d="$WORK/8"; mkdir -p "$d/rel"; stub_bin "$d/bin"; stub_curl "$d/bin/curl"; fake_root "$d/root"
printf '#!/bin/sh\necho 0.0.0-stub\n' > "$d/rel/herdr-nvim"; chmod +x "$d/rel/herdr-nvim"
tar -czf "$d/rel/asset.tar.gz" -C "$d/rel" herdr-nvim
printf '%s  herdr-nvim-%s.tar.gz\n' "$(sha256_of "$d/rel/asset.tar.gz")" "$TRIPLE" > "$d/rel/SHA256SUMS"
( cd "$d/root" && git init -q && git -c user.email=t@x -c user.name=t commit -q --allow-empty -m x )
( cd "$d/root" && git rev-parse HEAD ) > "$d/rel/COMMIT"
REL="$d/rel" HOME=/nonexistent PATH="$d/bin:/usr/bin:/bin" \
  sh "$d/root/scripts/build.sh" > "$d/out" 2>&1
sed 's/^/    /' "$d/out"
[ -x "$d/root/target/release/herdr-nvim" ] || fail "the matching binary was not installed"
! grep -q 'stub cargo' "$d/out" || fail "a matching commit still built from source"
ok

echo
echo "all $passed build.sh checks passed ($TRIPLE)"
