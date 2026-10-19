#!/usr/bin/env bash
#
# Stage Dusk's forked Moonlight client for bundling.
#
# Dusk ships its own Moonlight because the in-stream overlay lives in the
# fork, not upstream. Until this ran, a release build carried no client at
# all and fell through to whatever Moonlight was installed on the machine —
# which streams perfectly well and has no overlay, so the feature looked
# broken rather than absent.
#
# The fork is a separate GPL-3.0 program built in its own repository. This
# only copies the artifact; it does not build it, because building Qt is the
# fork's business and doing it here would put a second Qt toolchain in this
# repo to maintain.
#
# Usage:
#   ./scripts/stage-client.sh                 # from the sibling checkout
#   ./scripts/stage-client.sh /path/to/app    # from somewhere else
#
# On macOS, set APPLE_SIGNING_IDENTITY to sign the nested bundle. That has to
# happen here rather than after bundling: a signature lives inside the bundle
# it covers, so it survives the copy Tauri does, and signing the outer app
# afterwards does not reach inside a nested one.

set -euo pipefail

cd "$(dirname "$0")/.."
dest="src-tauri/client"

case "$(uname -s)" in
  Darwin) built="app/Moonlight.app"; staged="$dest/Moonlight.app" ;;
  Linux)  built="app/moonlight";     staged="$dest/moonlight" ;;
  *)      echo "error: run this on macOS or Linux; Windows staging is a .ps1" >&2; exit 1 ;;
esac

source_app="${1:-../moonlight-qt/$built}"

if [[ ! -e "$source_app" ]]; then
  cat >&2 <<EOF
error: no built client at $source_app

Build the fork first, or pass its path:
  git clone https://github.com/darkharasho/moonlight-qt ../moonlight-qt
  cd ../moonlight-qt && <its own build steps>
EOF
  exit 1
fi

# Clear the old one rather than copying over it: a stale file from a previous
# build that the new one no longer ships would otherwise be bundled forever.
find "$dest" -mindepth 1 ! -name README.md -exec rm -rf {} + 2>/dev/null || true

# -R rather than -a: the fork's macOS bundle has no symlinks in it (macdeployqt
# flattens them), and -R preserves the executable bits, which is the part that
# matters.
cp -R "$source_app" "$staged"

if [[ "$(uname -s)" == "Darwin" && -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "Signing the nested client..."
  # --options runtime because the outer app is notarized, and notarization
  # rejects a bundle containing code without the hardened runtime. Inside-out
  # is codesign's requirement, which --deep satisfies for a leaf bundle.
  codesign --force --deep --timestamp --options runtime \
    --sign "$APPLE_SIGNING_IDENTITY" "$staged"
  codesign --verify --strict --verbose=2 "$staged"
elif [[ "$(uname -s)" == "Darwin" ]]; then
  echo "note: APPLE_SIGNING_IDENTITY is unset, so the client is unsigned." >&2
  echo "      Fine for a local build; notarization will reject it." >&2
fi

echo "Staged $(du -sh "$staged" | cut -f1) at $staged"
