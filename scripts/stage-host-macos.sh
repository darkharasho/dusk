#!/usr/bin/env bash
#
# Build the hosting helper a macOS Dusk carries inside itself.
#
# On Linux and Windows Dusk ships Sunshine's installer and runs it
# (stage-sunshine.sh). On macOS it ships Sunshine itself: the app goes in
# Dusk.app/Contents/Helpers, signed and notarized with Dusk, and "installing"
# hosting is only registering a launch agent that points at it. Nothing is
# downloaded, nothing lands in /Applications, and no administrator prompt
# appears.
#
# Sunshine publishes one image per CPU while Dusk is universal, so this
# merges the two apps into one with lipo. Their trees are checked to be
# identical first; a release whose two builds differ in layout fails here
# rather than producing a helper that is missing a library on one CPU.
#
# The result is renamed Dusk Host, with its own bundle identifier. macOS
# names an app by its bundle in Screen Recording, Microphone and the menu
# bar, so this is what people see there instead of "Sunshine". The
# executable keeps its name; Dusk reads it from Info.plist.
#
# Sunshine is GPL-3.0 and shipping it is conveying it; src-tauri/licenses/
# carries the licence text and source offer, as on the other platforms.
#
# Usage:
#   ./scripts/stage-host-macos.sh <sunshine tag> <out dir>
#
# Writes "<out dir>/Dusk Host.app" and prints its path last.

set -euo pipefail

tag="${1:?usage: stage-host-macos.sh <sunshine tag> <out dir>}"
out="${2:?usage: stage-host-macos.sh <sunshine tag> <out dir>}"
# Where the Sunshine-macOS-*.dmg assets come from. Defaults to upstream, but
# the release pipeline points this at our own mirror: LizardByte publishes
# the builds carrying the macOS fixes as pre-releases and garbage-collects
# them within days, which left a pinned tag "release not found" and broke the
# macOS build. scripts/mirror-sunshine-macos.sh copies a build into a release
# we control so the pin cannot vanish. See SUNSHINE_MACOS_REPO in release.yml.
repo="${SUNSHINE_MACOS_REPO:-LizardByte/Sunshine}"
[[ "$(uname -s)" == Darwin ]] || { echo "error: this needs macOS (hdiutil, lipo)" >&2; exit 1; }

work="$(mktemp -d)"
mounts=()
cleanup() {
  for m in ${mounts[@]+"${mounts[@]}"}; do hdiutil detach "$m" -quiet || true; done
  rm -rf "$work"
}
trap cleanup EXIT

fetch() {
  local arch="$1" asset="Sunshine-macOS-$1.dmg"
  gh release download "$tag" --repo "$repo" --pattern "$asset" --dir "$work" >&2
  # GitHub's digest proves the bytes arrived intact, not who built them;
  # Sunshine publishes no signature to check against.
  local want got
  want="$(gh release view "$tag" --repo "$repo" --json assets \
    -q ".assets[] | select(.name == \"$asset\") | .digest" | sed 's/^sha256://')"
  [[ -n "$want" ]] || { echo "error: GitHub reports no digest for $asset" >&2; exit 1; }
  got="$(shasum -a 256 "$work/$asset" | cut -d' ' -f1)"
  [[ "$got" == "$want" ]] || { echo "error: $asset does not match its digest" >&2; exit 1; }

  local mount="$work/mnt-$arch"
  mkdir -p "$mount"
  hdiutil attach "$work/$asset" -nobrowse -noautoopen -readonly -mountpoint "$mount" -quiet
  mounts+=("$mount")
  # ditto, not cp: the Qt frameworks are symlinked bundles.
  ditto "$mount/Sunshine.app" "$work/$arch.app"
}

fetch arm64
fetch x86_64

arm="$work/arm64.app"
x86="$work/x86_64.app"
if ! diff <(cd "$arm" && find . | sort) <(cd "$x86" && find . | sort) >/dev/null; then
  echo "error: the arm64 and x86_64 apps differ in layout; cannot merge them" >&2
  exit 1
fi

echo "==> Merging into one universal app" >&2
merged="$work/merged.app"
ditto "$arm" "$merged"
# Regular files only: a symlink inside a framework points at a file that is
# itself merged.
while IFS= read -r rel; do
  a="$arm/$rel"
  file -b "$a" | grep -q 'Mach-O' || continue
  archs="$(lipo -archs "$a")"
  if [[ " $archs " == *" x86_64 "* ]]; then
    continue  # already universal upstream
  fi
  lipo -create "$a" "$x86/$rel" -output "$merged/$rel"
done < <(cd "$arm" && find . -type f | sed 's|^\./||')

exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$merged/Contents/Info.plist")"
lipo -archs "$merged/Contents/MacOS/$exe" | grep -q x86_64 \
  || { echo "error: the merged executable is not universal" >&2; exit 1; }

echo "==> Renaming it Dusk Host" >&2
plist="$merged/Contents/Info.plist"
set_key() {
  /usr/libexec/PlistBuddy -c "Set :$1 $2" "$plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :$1 string $2" "$plist"
}
set_key CFBundleIdentifier dev.dusk.app.host
set_key CFBundleName "Dusk Host"
set_key CFBundleDisplayName "Dusk Host"
set_key NSMicrophoneUsageDescription "Dusk Host uses the microphone to stream audio."
set_key NSScreenCaptureUsageDescription "Dusk Host records the screen to stream it to your other machines."
# No dock icon. Upstream already sets this; asserted rather than assumed.
/usr/libexec/PlistBuddy -c "Delete :LSUIElement" "$plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$plist"

# Upstream's signature is now broken by both edits. Its entitlements are kept
# beside the app so package-macos.sh re-signs with the same rights; the
# microphone, under the hardened runtime, needs one.
codesign -d --entitlements :- "$arm" > "$work/entitlements.plist" 2>/dev/null || true
rm -rf "$merged/Contents/_CodeSignature"

mkdir -p "$out"
rm -rf "$out/Dusk Host.app" "$out/host-entitlements.plist"
ditto "$merged" "$out/Dusk Host.app"
if [[ -s "$work/entitlements.plist" ]]; then
  cp "$work/entitlements.plist" "$out/host-entitlements.plist"
fi

echo "Staged Dusk Host (Sunshine ${tag#v}, universal) at:" >&2
echo "$out/Dusk Host.app"
