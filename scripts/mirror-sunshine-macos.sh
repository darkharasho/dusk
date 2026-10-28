#!/usr/bin/env bash
#
# Mirror a macOS Sunshine build into a release we control.
#
# Why this exists: the macOS host is built from a Sunshine DMG downloaded at a
# pinned tag (SUNSHINE_MACOS_TAG in release.yml). LizardByte publishes the
# builds that carry the macOS-only fixes as *pre-releases* and garbage-collects
# them within days. When the pinned pre-release was removed, the macOS build
# failed with "release not found" — a release that built fine yesterday could
# not build today, through no change of ours.
#
# This copies the two DMGs from an upstream tag into a "sunshine-macos-<tag>"
# release on our own repo, which nothing but us will ever delete. The pipeline
# then pins SUNSHINE_MACOS_REPO at our repo and SUNSHINE_MACOS_TAG at the
# mirror tag; stage-host-macos.sh downloads and digest-checks it exactly as
# before, just from a source that cannot vanish.
#
# Run this once per Sunshine bump, before updating the tag in release.yml:
#   ./scripts/mirror-sunshine-macos.sh v2026.1028.122642
# then set in release.yml:
#   SUNSHINE_MACOS_REPO: <owner>/<repo>
#   SUNSHINE_MACOS_TAG:  sunshine-macos-v2026.1028.122642
#
# The mirror release is marked not-latest, so it never takes the "Latest"
# badge from a Dusk version release.
#
# Usage:
#   ./scripts/mirror-sunshine-macos.sh <upstream sunshine tag> [target repo]
#
# The target repo defaults to the current checkout's origin repo.

set -euo pipefail

upstream_tag="${1:?usage: mirror-sunshine-macos.sh <upstream sunshine tag> [target repo]}"
target_repo="${2:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}"
upstream_repo="${SUNSHINE_UPSTREAM_REPO:-LizardByte/Sunshine}"
mirror_tag="sunshine-macos-${upstream_tag}"
assets=("Sunshine-macOS-arm64.dmg" "Sunshine-macOS-x86_64.dmg")

if gh release view "$mirror_tag" --repo "$target_repo" >/dev/null 2>&1; then
  echo "==> $target_repo already has $mirror_tag; nothing to do." >&2
  echo "    Delete it first to re-mirror: gh release delete $mirror_tag --repo $target_repo --cleanup-tag" >&2
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for asset in "${assets[@]}"; do
  echo "==> Downloading $asset from $upstream_repo@$upstream_tag" >&2
  gh release download "$upstream_tag" --repo "$upstream_repo" --pattern "$asset" --dir "$work" >&2
  [[ -s "$work/$asset" ]] || { echo "error: $asset did not download" >&2; exit 1; }
done

notes="Mirror of macOS Sunshine from [\`${upstream_repo}@${upstream_tag}\`](https://github.com/${upstream_repo}/releases/tag/${upstream_tag}), kept so the Dusk macOS build cannot be broken by upstream removing a pre-release. Not for direct use. See scripts/mirror-sunshine-macos.sh."

echo "==> Creating $mirror_tag on $target_repo" >&2
gh release create "$mirror_tag" \
  --repo "$target_repo" \
  --title "macOS Sunshine mirror ${upstream_tag}" \
  --notes "$notes" \
  --latest=false \
  "$work/${assets[0]}" "$work/${assets[1]}" >&2

echo "==> Verifying GitHub recorded a digest for each asset" >&2
for asset in "${assets[@]}"; do
  # stage-host-macos.sh checks the download against this digest, so it has to
  # be present before the pipeline can use the mirror.
  for attempt in 1 2 3 4 5 6; do
    digest="$(gh release view "$mirror_tag" --repo "$target_repo" --json assets \
      -q ".assets[] | select(.name == \"$asset\") | .digest" 2>/dev/null || true)"
    [[ -n "$digest" ]] && break
    sleep 5
  done
  [[ -n "$digest" ]] || { echo "error: no digest for $asset yet; GitHub may still be computing it" >&2; exit 1; }
  echo "    $asset $digest" >&2
done

echo "$target_repo $mirror_tag"
