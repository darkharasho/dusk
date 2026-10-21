#!/usr/bin/env bash
# Writes the updater manifest (latest.json) from the release builds.
#
#   scripts/write-latest-json.sh <artifacts-dir> <out-file>
#
# Every installed Dusk reads this from the latest release to learn whether
# there is a newer version, where each platform's update is, and the
# signature to check it against. The URLs point at the release assets, which
# the publish job uploads under the same file names.
#
# macOS has one universal archive, listed under both CPUs because the
# updater asks for its own. Windows lists the MSI and the NSIS setup under
# their own keys, so each install updates through the installer it came
# from: a per-machine MSI install updated by the per-user setup would end up
# as two copies of Dusk. The plain windows-x86_64 key is the setup, for an
# updater that asks without saying how it was installed.
#
# The .deb sits *beside* platforms, not in it, and that placement is the
# whole trick: the updater plugin reads `platforms` and would try to install
# a .deb as though it were an AppImage, writing over files dpkg owns. Dusk
# installs that one itself, through the package manager, and reads it from
# this key. linux-x86_64 therefore stays the AppImage.
set -euo pipefail

dir="${1:?artifacts directory}"
out="${2:?output file}"

version="$(node -p 'require("./src-tauri/tauri.conf.json").version')"
tag="v$version"
# A dry run has no tag, but the manifest is still worth building to check it.
if [[ "${GITHUB_REF:-}" == refs/tags/* && "${GITHUB_REF#refs/tags/}" != "$tag" ]]; then
  echo "::error::tag ${GITHUB_REF#refs/tags/} does not match version $version in tauri.conf.json" >&2
  exit 1
fi
base="https://github.com/${GITHUB_REPOSITORY:-darkharasho/dusk}/releases/download/$tag"

# The one file matching a pattern, and its signature beside it.
find_one() {
  local found
  found="$(find "$dir" -type f -name "$1" ! -name '*.sig')"
  [[ -n "$found" && "$(wc -l <<<"$found")" -eq 1 ]] \
    || { echo "::error::expected exactly one artifact matching $1, found: ${found:-none}" >&2; exit 1; }
  [[ -f "$found.sig" ]] || { echo "::error::no signature beside $found" >&2; exit 1; }
  printf '%s' "$found"
}

entry() {
  jq -n --arg url "$base/$(basename "$1")" --rawfile sig "$1.sig" '{url: $url, signature: ($sig | sub("\\s+$"; ""))}'
}

mac="$(find_one '*.app.tar.gz')"
nsis="$(find_one '*-setup.exe')"
msi="$(find_one '*.msi')"
appimage="$(find_one '*.AppImage')"
deb="$(find_one '*.deb')"

jq -n \
  --arg version "$version" \
  --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg notes "See https://github.com/${GITHUB_REPOSITORY:-darkharasho/dusk}/releases/tag/$tag" \
  --argjson mac "$(entry "$mac")" \
  --argjson nsis "$(entry "$nsis")" \
  --argjson msi "$(entry "$msi")" \
  --argjson appimage "$(entry "$appimage")" \
  --argjson deb "$(entry "$deb")" \
  '{
    version: $version,
    notes: $notes,
    pub_date: $date,
    platforms: {
      "darwin-aarch64": $mac,
      "darwin-x86_64": $mac,
      "windows-x86_64": $nsis,
      "windows-x86_64-nsis": $nsis,
      "windows-x86_64-msi": $msi,
      "linux-x86_64": $appimage,
      "linux-x86_64-appimage": $appimage
    },
    deb: $deb
  }' >"$out"

cat "$out"
