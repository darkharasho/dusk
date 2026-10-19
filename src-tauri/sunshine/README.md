# The bundled Sunshine installer

Where one pinned Sunshine installer is staged before a bundle is built.
`tauri.conf.json` ships this directory as a resource, and
`src-tauri/src/install/bundled.rs` installs from it instead of downloading.

Put it here with:

```sh
./scripts/stage-sunshine.sh v2026.914.233613
```

That downloads the official installer for the platform it runs on, checks it
against the digest GitHub reports, and writes `bundled.json` beside it with
the version, file name and checksum. Dusk checks the file against that
checksum again before running it.

Everything here is ignored by git except this file, which keeps the
directory in existence: Tauri fails a build on a resource path that does not
resolve. A build with nothing staged is fine, and downloads Sunshine at first
run the way Dusk always did.
