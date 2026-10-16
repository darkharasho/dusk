#!/usr/bin/env bash
#
# Build a signed and notarized macOS release.
#
# Credentials come from 1Password at launch via `op run`, so nothing secret is
# written to disk or baked into the repository — scripts/release.env.tpl holds
# only op:// references.
#
# Usage:  ./scripts/build-macos-release.sh [--no-notarize]
#
# Requires: the 1Password desktop app, `op` (brew install --cask 1password-cli),
# and "Integrate with 1Password CLI" enabled in the app's Developer settings.

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v op >/dev/null 2>&1; then
  echo "error: the 1Password CLI (op) is not installed." >&2
  echo "       brew install --cask 1password-cli" >&2
  exit 1
fi

# Notarization is the slow part (it waits on Apple's service) and it is the
# only step that needs network access, so it is worth being able to skip while
# iterating on the bundle itself.
notarize=1
for arg in "$@"; do
  case "$arg" in
    --no-notarize) notarize=0 ;;
    *) echo "error: unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# Tauri decides whether to notarize purely by whether APPLE_ID is present in
# the environment, so dropping those three variables is how a build is kept
# signed-but-not-notarized.
env_file=scripts/release.env.tpl
if [ "$notarize" -eq 0 ]; then
  env_file="$(mktemp -t dusk-release-env)"
  trap 'rm -f "$env_file"' EXIT
  grep -v -E '^APPLE_(ID|PASSWORD|TEAM_ID)=' scripts/release.env.tpl > "$env_file"
  echo "==> Signing only (notarization skipped)"
else
  echo "==> Signing and notarizing"
fi

# `op run` resolves every op:// reference and execs the build with those values
# in its environment. The first run of a session raises an auth prompt.
# Masking is left on so a secret cannot surface in build output.
op run --env-file="$env_file" -- npm run tauri build

# Tauri notarizes and staples the .app, then builds the .dmg *around* the
# stapled app — so the disk image itself is signed but carries no ticket of
# its own. Without this step a downloaded .dmg trips Gatekeeper when opened,
# even though the app inside it is fine. Notarize and staple the image too.
if [ "$notarize" -eq 1 ]; then
  dmg="$(find src-tauri/target/release/bundle/dmg -maxdepth 1 -name '*.dmg' | head -1)"
  if [ -z "$dmg" ]; then
    echo "error: no .dmg found to notarize." >&2
    exit 1
  fi
  echo
  echo "==> Notarizing the disk image"
  op run --env-file="$env_file" -- sh -c '
    xcrun notarytool submit "$1" \
      --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" \
      --wait' -- "$dmg"
  xcrun stapler staple "$dmg"
fi

echo
echo "==> Verifying the signature"
app="src-tauri/target/release/bundle/macos/Dusk.app"
if [ ! -d "$app" ]; then
  echo "error: expected a bundle at $app but none was produced." >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app"
echo
codesign --display --verbose=4 "$app" 2>&1 | grep -E '^(Authority|TeamIdentifier|Identifier|Format)='

echo
echo "==> Gatekeeper assessment"
# Only a notarized build can pass this; a signed-only build is correctly
# rejected here, so the failure is reported rather than treated as fatal.
if spctl --assess --type execute --verbose=4 "$app" 2>&1; then
  echo "accepted by Gatekeeper"
elif [ "$notarize" -eq 0 ]; then
  echo "(expected: a signed-but-not-notarized build does not pass Gatekeeper)"
else
  echo "warning: notarized build was not accepted by Gatekeeper — see above" >&2
fi

# The .dmg is what people actually download, so verify its ticket separately.
# `stapler validate` is the check that catches a missing ticket; spctl alone
# can pass on a stapled app while the image around it is still unticketed.
if [ "$notarize" -eq 1 ]; then
  echo
  echo "==> Disk image"
  xcrun stapler validate "$dmg" 2>&1 | tail -1
  spctl --assess --type install --verbose=4 "$dmg" 2>&1 | tail -2
fi

echo
echo "==> Artifacts"
find src-tauri/target/release/bundle -maxdepth 2 -name '*.dmg' -o -maxdepth 2 -name '*.app' | sed 's/^/    /'
