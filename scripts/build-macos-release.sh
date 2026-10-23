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
# One handler for every temporary file this script makes. A second
# `trap ... EXIT` would replace this rather than add to it, which is how the
# first of them came to be left behind.
scratch=""
cleanup() { [ -n "$scratch" ] && rm -f $scratch; }
trap cleanup EXIT

env_file=scripts/release.env.tpl
if [ "$notarize" -eq 0 ]; then
  env_file="$(mktemp -t dusk-release-env)"
  scratch="$scratch $env_file"
  grep -v -E '^APPLE_(ID|PASSWORD|TEAM_ID)=' scripts/release.env.tpl > "$env_file"
  echo "==> Signing only (notarization skipped)"
else
  echo "==> Signing and notarizing"
fi

# Dusk ships a forked Moonlight client. It is built by its own repository, so
# a local release build needs one to hand — the released artifact, or a local
# build of the fork.
client="${DUSK_CLIENT_APP:-../moonlight-qt/app/Moonlight.app}"
if [ ! -d "$client" ]; then
  echo "error: no Moonlight client at $client" >&2
  echo "       Set DUSK_CLIENT_APP, or download the dusk-v* release of" >&2
  echo "       darkharasho/dusk-client and point at the .app inside it." >&2
  exit 1
fi

# The app only. The image is built by package-macos.sh once the client is
# inside, and APPLE_ID is withheld here so Tauri signs without notarizing —
# a ticket stapled now would be invalidated by the insert.
#
# `op run` resolves every op:// reference and execs the build with those values
# in its environment. The first run of a session raises an auth prompt.
# Masking is left on so a secret cannot surface in build output.
build_env="$(mktemp -t dusk-build-env)"
scratch="$scratch $build_env"
grep -v -E '^APPLE_(ID|PASSWORD|TEAM_ID)=' "$env_file" > "$build_env"
op run --env-file="$build_env" -- npm run tauri build -- \
  --target universal-apple-darwin --bundles app

package_args=""
[ "$notarize" -eq 0 ] && package_args="--no-notarize"
# shellcheck disable=SC2086
op run --env-file="$env_file" -- ./scripts/package-macos.sh "$client" $package_args

# Notarizing the image and verifying the result is shared with the release
# workflow, which has no `op` — so it reads credentials from the environment
# and both paths run the same checks.
finalize_args=""
[ "$notarize" -eq 0 ] && finalize_args="--no-notarize"
# shellcheck disable=SC2086
op run --env-file="$env_file" -- ./scripts/finalize-macos-release.sh $finalize_args
