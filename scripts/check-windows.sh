#!/usr/bin/env bash
#
# Compile and lint the Windows build without a Windows machine.
#
# Two of the three host backends, the registry reader and every elevation
# path only compile under cfg(windows), so on a Mac they are never checked
# at all — the first two CI failures on this project were both exactly that.
# This turns a push-and-wait round trip into about thirty seconds.
#
# What it catches: anything the compiler or clippy sees. What it does not:
# behaviour. These are cross-compiled, not run, so a test that passes here
# is only a test that *builds* here. CI still runs them for real.
#
# One-time setup:
#   brew install llvm lld
#   cargo install cargo-xwin
#   rustup target add x86_64-pc-windows-msvc
#
# cargo-xwin downloads the Windows SDK and CRT headers on first use and
# caches them, so the first run is slow and later ones are not.

set -euo pipefail

TARGET=x86_64-pc-windows-msvc

# A rustup install puts these here but only wires up PATH in an interactive
# shell's profile, which this is not.
[[ -d "$HOME/.cargo/bin" ]] && PATH="$HOME/.cargo/bin:$PATH"

# Homebrew's llvm and lld are keg-only, so they are not on PATH by default.
# clang-cl compiles the C in ring; lld-link is the linker for a real build.
for keg in llvm lld; do
  prefix="$(brew --prefix "$keg" 2>/dev/null || true)"
  if [[ -z "$prefix" || ! -d "$prefix/bin" ]]; then
    echo "error: $keg is missing. Run: brew install llvm lld" >&2
    exit 1
  fi
  PATH="$prefix/bin:$PATH"
done
export PATH

if ! command -v cargo-xwin >/dev/null; then
  echo "error: cargo-xwin is missing. Run: cargo install cargo-xwin" >&2
  exit 1
fi

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "error: the $TARGET target is missing. Run: rustup target add $TARGET" >&2
  exit 1
fi

cd "$(dirname "$0")/../src-tauri"

# --all-targets so the test code is checked too. That is where the third CI
# failure lived: tests that referenced /bin/sleep and could not run on
# Windows. This would not have caught that one — it was a runtime failure,
# not a compile error — but it does catch tests that will not build.
echo "Checking $TARGET..."
cargo xwin clippy --target "$TARGET" --all-targets -- -D warnings
echo "Windows build is clean."
