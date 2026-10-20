#!/usr/bin/env bash
#
# Stage the Sunshine installer Dusk ships with.
#
# Dusk installs Sunshine for the person rather than sending them to its
# website. Shipping the installer inside the release, rather than fetching
# "latest" at first run, means first run needs no network and installs the
# exact Sunshine the settings screen was tested against.
#
# Sunshine is GPL-3.0, and shipping its installer is conveying it. That is
# deliberate (docs/architecture.md, "Target shape"): src-tauri/licenses/
# carries the licence text and the source offer that conveying requires.
#
# Linux and Windows. macOS carries Sunshine itself rather than an installer,
# signed inside Dusk: see stage-host-macos.sh.
#
# Usage:
#   ./scripts/stage-sunshine.sh <sunshine tag>     e.g. v2026.914.233613

set -euo pipefail

cd "$(dirname "$0")/.."
tag="${1:?usage: stage-sunshine.sh <sunshine tag>}"
repo="LizardByte/Sunshine"
dest="src-tauri/sunshine"

# The same asset install/release.rs would pick at run time, so a bundled
# install and a downloaded one are the same file.
case "$(uname -s)" in
  Linux)                pattern="Sunshine_*_x86_64.AppImage" ;;
  MINGW*|MSYS*|CYGWIN*) pattern="Sunshine-Windows-AMD64-installer.msi" ;;
  Darwin)
    echo "error: macOS embeds Sunshine instead; use stage-host-macos.sh" >&2
    exit 1 ;;
  *) echo "error: unrecognised platform $(uname -s)" >&2; exit 1 ;;
esac

find "$dest" -mindepth 1 ! -name README.md -exec rm -rf {} + 2>/dev/null || true

gh release download "$tag" --repo "$repo" --pattern "$pattern" --dir "$dest"
asset="$(cd "$dest" && ls $pattern)"

# GitHub's digest proves the bytes arrived intact, not who built them —
# Sunshine publishes no signature to check against. Recorded so Dusk can
# re-check the file it is about to run with elevated rights.
want="$(gh release view "$tag" --repo "$repo" --json assets \
  -q ".assets[] | select(.name == \"$asset\") | .digest" | sed 's/^sha256://')"
[[ -n "$want" ]] || { echo "error: GitHub reports no digest for $asset" >&2; exit 1; }

if command -v sha256sum >/dev/null; then
  got="$(sha256sum "$dest/$asset" | cut -d' ' -f1)"
else
  got="$(shasum -a 256 "$dest/$asset" | cut -d' ' -f1)"
fi
[[ "$got" == "$want" ]] || { echo "error: $asset does not match its digest" >&2; exit 1; }

printf '{"version":"%s","asset":"%s","sha256":"%s"}\n' \
  "${tag#v}" "$asset" "$got" > "$dest/bundled.json"

echo "Staged Sunshine ${tag#v} ($(du -h "$dest/$asset" | cut -f1)) at $dest/$asset"
