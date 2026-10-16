#!/usr/bin/env bash
#
# Notarize the disk image and verify the finished bundles.
#
# Split out of build-macos-release.sh so the local build and the release
# workflow run byte-identical checks. Credentials are read from the
# environment: locally `op run` injects them, in CI they come from repository
# secrets. Nothing here knows or cares which.
#
# Usage:  ./scripts/finalize-macos-release.sh [--no-notarize]
#
# Expects APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID in the environment unless
# --no-notarize is passed.

set -euo pipefail

cd "$(dirname "$0")/.."

notarize=1
for arg in "$@"; do
  case "$arg" in
    --no-notarize) notarize=0 ;;
    *) echo "error: unknown argument: $arg" >&2; exit 1 ;;
  esac
done

bundle_dir=src-tauri/target/release/bundle
# A universal build nests its output under the target triple, so the bundle
# directory is not always where a native build puts it.
if [ ! -d "$bundle_dir" ]; then
  bundle_dir="$(find src-tauri/target -maxdepth 3 -type d -name bundle | head -1)"
fi
if [ -z "$bundle_dir" ] || [ ! -d "$bundle_dir" ]; then
  echo "error: no bundle directory found under src-tauri/target." >&2
  exit 1
fi

app="$(find "$bundle_dir/macos" -maxdepth 1 -name '*.app' 2>/dev/null | head -1)"
if [ -z "$app" ]; then
  echo "error: no .app found under $bundle_dir/macos." >&2
  exit 1
fi

# Tauri notarizes and staples the .app, then builds the .dmg *around* the
# stapled app — so the disk image itself is signed but carries no ticket of
# its own. Without this step a downloaded .dmg trips Gatekeeper when opened,
# even though the app inside it is fine. Notarize and staple the image too.
dmg=""
if [ "$notarize" -eq 1 ]; then
  dmg="$(find "$bundle_dir/dmg" -maxdepth 1 -name '*.dmg' 2>/dev/null | head -1)"
  if [ -z "$dmg" ]; then
    echo "error: no .dmg found to notarize." >&2
    exit 1
  fi

  for var in APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
    eval "val=\${$var:-}"
    if [ -z "$val" ]; then
      echo "error: $var is not set; cannot notarize the disk image." >&2
      exit 1
    fi
  done

  echo
  echo "==> Notarizing the disk image"
  xcrun notarytool submit "$dmg" \
    --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" \
    --wait
  xcrun stapler staple "$dmg"
fi

echo
echo "==> Verifying the signature"
codesign --verify --deep --strict --verbose=2 "$app"
echo
codesign --display --verbose=4 "$app" 2>&1 | grep -E '^(Authority|TeamIdentifier|Identifier|Format)='

echo
echo "==> Gatekeeper assessment"
# Only a notarized build can pass this; a signed-only build is correctly
# rejected here, so that case is reported rather than treated as fatal.
if spctl --assess --type execute --verbose=4 "$app" 2>&1; then
  echo "accepted by Gatekeeper"
elif [ "$notarize" -eq 0 ]; then
  echo "(expected: a signed-but-not-notarized build does not pass Gatekeeper)"
else
  echo "error: notarized app was not accepted by Gatekeeper — see above" >&2
  exit 1
fi

# The .dmg is what people actually download, so verify its ticket separately.
# `stapler validate` is the check that catches a missing ticket; spctl on the
# app alone passes happily while the image around it is still unticketed.
if [ "$notarize" -eq 1 ]; then
  echo
  echo "==> Disk image"
  if ! xcrun stapler validate "$dmg" 2>&1 | tail -1; then
    echo "error: the disk image has no notarization ticket stapled." >&2
    exit 1
  fi
  spctl --assess --type install --verbose=4 "$dmg" 2>&1 | tail -2
fi

echo
echo "==> Artifacts"
find "$bundle_dir" -maxdepth 2 \( -name '*.dmg' -o -name '*.app' \) | sed 's/^/    /'
