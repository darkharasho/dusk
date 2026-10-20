#!/usr/bin/env bash
#
# Put the staged client and host back into a built Dusk AppImage, unaltered.
#
# Both are AppImages themselves: an ELF runtime with a squashfs appended
# after the last section. The AppImage bundler sets an rpath on every ELF
# file it finds under usr/lib, and patchelf makes room for that by adding a
# segment at the end of the file -- on top of the appended squashfs. The
# runtime then segfaults before printing a word, so streaming fails with no
# reason and the host fails its checksum. The .deb is untouched by this; only
# the AppImage needs repairing.
#
# The bundler has no way to exclude a file, so this undoes it afterwards:
# unpack the image, copy the staged originals over whatever the bundler left,
# and repack behind the image's own runtime. It does nothing when every file
# already matches, so a bundler that stops doing this costs no rebuild.
#
# Run by scripts/tauri.mjs after a Linux build, which re-signs the result.
#
# Usage:
#   ./scripts/repair-appimage.sh path/to/Dusk.AppImage
#
# Exits 0 having repaired, 0 with nothing to repair, non-zero on failure.
# Prints "repaired" on stdout only when the image was rewritten.

set -euo pipefail

cd "$(dirname "$0")/.."
image="$(realpath "$1")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Where the squashfs starts. The runtime answers this itself without
# mounting anything, so FUSE is not needed.
offset="$("$image" --appimage-offset)"
unsquashfs -n -no-xattrs -o "$offset" -d "$work/root" "$image" >/dev/null

# Tauri puts resources in usr/lib/<product name>; find that by its client
# directory rather than repeating the product name here.
resources="$(find "$work/root/usr/lib" -mindepth 2 -maxdepth 2 -type d -name client -printf '%h\n' | head -1)"
if [[ -z "$resources" ]]; then
  echo "error: no bundled client directory in $image" >&2
  exit 1
fi

changed=0
for staged_dir in src-tauri/client src-tauri/sunshine; do
  [[ -d "$staged_dir" ]] || continue
  while IFS= read -r -d '' staged; do
    relative="${staged#src-tauri/}"
    bundled="$resources/$relative"
    if [[ ! -f "$bundled" ]]; then
      echo "error: $relative was staged but is not in the image" >&2
      exit 1
    fi
    if ! cmp -s "$staged" "$bundled"; then
      echo "dusk: restoring $relative, altered by the AppImage bundler" >&2
      cp --preserve=mode "$staged" "$bundled"
      changed=1
    fi
  done < <(find "$staged_dir" -type f ! -name README.md -print0)
done

[[ "$changed" == 1 ]] || exit 0

# zstd with 128K blocks is what the bundler itself writes, and what the
# runtime is built to read.
mksquashfs "$work/root" "$work/fs" -root-owned -no-xattrs -noappend -comp zstd -b 131072 \
  -quiet -no-progress >/dev/null

head -c "$offset" "$image" > "$work/image"
cat "$work/fs" >> "$work/image"
chmod 755 "$work/image"
mv "$work/image" "$image"

echo repaired
