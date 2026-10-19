# The bundled Moonlight client

This directory is where Dusk's forked Moonlight client is staged before a
bundle is built. `tauri.conf.json` ships it as a resource, so whatever is here
when `tauri build` runs ends up inside the app and is what `dusk_client()`
finds at `client/` in the resource directory.

Put it here with:

```sh
./scripts/stage-client.sh
```

Everything in this directory is ignored by git except this file, which is
committed so the directory always exists — Tauri fails a build when a
configured resource path does not resolve, and a build should not require a
staged client just to run.

A build with nothing staged is fine. Dusk falls back to whatever Moonlight is
installed on the machine, which streams normally and has no in-stream overlay;
the device drawer says so rather than leaving it to be guessed at.
