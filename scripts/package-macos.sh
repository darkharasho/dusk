#!/usr/bin/env bash
#
# Put the Moonlight client inside the built macOS app, then make the image.
#
# # Why this exists rather than Tauri's `bundle.resources`
#
# Tauri's resource copier does not preserve symlinks. A Qt framework is a
# versioned bundle whose top-level binary is a symlink into Versions/Current,
# so copying one that way replaces the symlink with a second copy of the
# binary. That doubles the size and, more to the point, destroys the structure
# the framework's signature seals — notarization rejects the result with "the
# signature of the binary is invalid", naming each framework in turn. That is
# a real failure this repository hit, not a precaution.
#
# So on macOS the client is inserted *after* Tauri has bundled, with `ditto`,
# which preserves symlinks and metadata. It goes in Contents/Helpers, which is
# where Apple expects a nested helper app — Resources is for resources, and
# code there is a notarization complaint of its own.
#
# # Ordering
#
# Every step here depends on the one before it:
#
#   insert         adding anything to a bundle invalidates its seal
#   sign client    inside-out: nested Mach-O, then frameworks, then the bundle
#   re-sign Dusk   so the outer seal covers what is now inside it
#   notarize app   only a signed bundle can be notarized
#   build image    around an app that is already stapled
#
# The image is notarized and stapled separately by finalize-macos-release.sh,
# which is shared with the local build.
#
# Usage:
#   ./scripts/package-macos.sh <client-app> [--no-notarize]
#
# Expects APPLE_SIGNING_IDENTITY, and APPLE_ID / APPLE_PASSWORD /
# APPLE_TEAM_ID unless --no-notarize. CLIENT_KEYCHAIN optionally names the
# keychain holding the identity.

set -euo pipefail

cd "$(dirname "$0")/.."

client_src=""
notarize=1
for arg in "$@"; do
  case "$arg" in
    --no-notarize) notarize=0 ;;
    -*) echo "error: unknown argument: $arg" >&2; exit 1 ;;
    *) client_src="$arg" ;;
  esac
done

[[ -n "$client_src" ]] || { echo "error: pass the client .app to insert" >&2; exit 1; }
[[ -d "$client_src" ]] || { echo "error: no client app at $client_src" >&2; exit 1; }
[[ -n "${APPLE_SIGNING_IDENTITY:-}" ]] || { echo "error: APPLE_SIGNING_IDENTITY is unset" >&2; exit 1; }

# A universal build nests its output under the target triple, so the bundle
# directory is not always where a native build puts it. Same lookup as
# finalize-macos-release.sh.
bundle_dir=src-tauri/target/release/bundle
[[ -d "$bundle_dir" ]] || bundle_dir="$(find src-tauri/target -maxdepth 3 -type d -name bundle | head -1)"
[[ -n "$bundle_dir" && -d "$bundle_dir" ]] || { echo "error: no bundle directory under src-tauri/target" >&2; exit 1; }

app="$(find "$bundle_dir/macos" -maxdepth 1 -name '*.app' 2>/dev/null | head -1)"
[[ -n "$app" ]] || { echo "error: no .app under $bundle_dir/macos — build with --bundles app first" >&2; exit 1; }

# Spelled out rather than assembled in an array: macOS ships bash 3.2, where
# expanding an empty array under `set -u` is an unbound-variable error.
#
# An identity of "-" is ad-hoc, which is how the layout can be checked
# without a certificate — the structural failure this script exists to
# prevent shows up under ad-hoc signing just as well. Apple's timestamp
# service refuses ad-hoc signatures, so that case drops the flag.
# `</dev/null` is load-bearing. These run inside `while read` loops fed by a
# process substitution, and codesign reads standard input — so without it the
# first call swallows the rest of the file list and the loop ends after one
# file. The result is a bundle with a single signed binary that looks fine
# until the notary service enumerates the rest.
sign() {
  local stamp=--timestamp
  [[ "$APPLE_SIGNING_IDENTITY" == "-" ]] && stamp=--timestamp=none

  if [[ -n "${CLIENT_KEYCHAIN:-}" ]]; then
    codesign --force "$stamp" --options runtime \
      --keychain "$CLIENT_KEYCHAIN" --sign "$APPLE_SIGNING_IDENTITY" "$@" </dev/null
  else
    codesign --force "$stamp" --options runtime \
      --sign "$APPLE_SIGNING_IDENTITY" "$@" </dev/null
  fi
}

# ---------------------------------------------------------------- insert

helpers="$app/Contents/Helpers"
echo "==> Inserting the client into $helpers"
rm -rf "$helpers"
mkdir -p "$helpers"
# ditto, not cp: it preserves symlinks, permissions and extended attributes,
# which is the entire point of doing this here instead of via resources.
ditto "$client_src" "$helpers/$(basename "$client_src")"

client="$helpers/$(basename "$client_src")"
# Prove the structure survived, because the failure mode this script exists
# for is silent until Apple's notary service reports it minutes later.
probe="$client/Contents/Frameworks"
if [[ -d "$probe" ]] && ! find "$probe" -maxdepth 2 -type l | grep -q .; then
  echo "error: no symlinks under $probe — the copy flattened the frameworks" >&2
  exit 1
fi

# ------------------------------------------------------------------ sign

echo "==> Signing the client, inside out"
# Nested Mach-O first. --deep would be the short way and is explicitly not
# how Apple wants nested code signed for distribution.
#
# Every regular file is asked what it is, rather than filtering on the
# executable bit first: a .dylib is routinely mode 644, and libSDL3.dylib
# was skipped exactly that way, which `codesign --verify` then reported as
# the whole client being "not signed at all".
#
# An explicit `if` rather than `file ... && sign`, because under `set -e` a
# non-Mach-O file makes that list the loop body's failing last command.
#
# Contents/MacOS is excluded: that is the bundle's main executable, and
# signing it on its own makes codesign validate the whole bundle around it,
# which fails on the libraries that have not been reached yet. It gets signed
# last, by signing the bundle.
while IFS= read -r f; do
  if file -b "$f" | grep -q 'Mach-O'; then
    sign "$f"
  fi
done < <(find "$client" -type f -not -path "$client/Contents/MacOS/*")

# Then each framework bundle, deepest first, so an inner one is sealed before
# whatever contains it.
while IFS= read -r fw; do
  sign "$fw"
done < <(find "$client" -depth -type d -name '*.framework')

sign "$client"
codesign --verify --strict --verbose=2 "$client"

echo "==> Re-signing the app around it"
# Tauri already signed this; adding Helpers broke that seal. Carry the
# entitlements forward rather than dropping them, since re-signing without
# them would quietly change what the app is allowed to do.
ents="$(mktemp -t dusk-ents)"
trap 'rm -f "$ents"' EXIT
if codesign -d --entitlements :- "$app" 2>/dev/null > "$ents" && [[ -s "$ents" ]]; then
  echo "    carrying forward existing entitlements"
  sign --entitlements "$ents" "$app"
else
  sign "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"

# ------------------------------------------------------------- notarize

if [[ "$notarize" -eq 1 ]]; then
  for var in APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
    [[ -n "${!var:-}" ]] || { echo "error: $var is not set" >&2; exit 1; }
  done

  echo "==> Notarizing the app"
  zip_path="$(mktemp -d)/$(basename "$app").zip"
  # ditto -c -k --keepParent is the submission format notarytool expects; a
  # plain zip loses the symlinks again and is rejected for the same reason
  # the resource copy was.
  ditto -c -k --keepParent "$app" "$zip_path"
  xcrun notarytool submit "$zip_path" \
    --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
  xcrun stapler staple "$app"
fi

# ----------------------------------------------------------------- image

version="$(python3 -c 'import json;print(json.load(open("src-tauri/tauri.conf.json"))["version"])')"
name="$(basename "$app" .app)"
dmg_dir="$bundle_dir/dmg"
dmg="$dmg_dir/${name}_${version}_universal.dmg"

echo "==> Building $dmg"
mkdir -p "$dmg_dir"
# Every image, not just the one about to be written. The release workflow
# collects artifacts with `find -name '*.dmg'`, so one left over from an
# earlier build in the same tree would be published alongside this one.
rm -f "$dmg_dir"/*.dmg
staging="$(mktemp -d)"
ditto "$app" "$staging/$(basename "$app")"
# The drag-to-install affordance people expect from a .dmg.
ln -s /Applications "$staging/Applications"
hdiutil create -volname "$name" -srcfolder "$staging" -ov -format UDZO "$dmg" >/dev/null
rm -rf "$staging"

sign "$dmg"
echo "==> Done: $dmg"
