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

# Notarizing the image and verifying the result is shared with the release
# workflow, which has no `op` — so it reads credentials from the environment
# and both paths run the same checks.
finalize_args=""
[ "$notarize" -eq 0 ] && finalize_args="--no-notarize"
# shellcheck disable=SC2086
op run --env-file="$env_file" -- ./scripts/finalize-macos-release.sh $finalize_args
