# Picking this up elsewhere

Written to move work from the Mac it was started on to the Bazzite machine.
`docs/architecture.md` is the plan; this is where the plan actually stands,
what to set up, and what bit hard enough to be worth not rediscovering.

## Where things stand

**v0.2.0 is released**, all three platforms, from `main`.
<https://github.com/darkharasho/dusk/releases/tag/v0.2.0>

| Phase | State |
| --- | --- |
| 0 — bundle the fork, fix the client search path | done, verified in artifacts |
| 1 — rename and identity | done, verified in artifacts |
| 2 — control socket, overlay actions, live stats | **Dusk side done; fork side not started** |
| 3–6 | not started |

Everything is merged to `main` / `master`. Nothing is in flight.

## The map

Two repositories, side by side. Several things assume they are siblings.

```
<parent>/
├── dusk/            main @ 2a2cba4   tags v0.1.0, v0.2.0
└── moonlight-qt/    master @ 199cc93a  tags dusk-v0.1.0, dusk-v0.2.0, dusk-v0.2.1
```

The fork builds and publishes a client; Dusk downloads a pinned one at
release time. That pin is one line — `CLIENT_TAG` in
`.github/workflows/release.yml`, currently `dusk-v0.2.1`. Bumping the client
means tagging `dusk-v*` in the fork, waiting for its release, and editing
that line.

`main` is the only branch left in dusk; the merged ones are gone. The fork
still has one of ours, **`dusk-overlay`**, two commits ahead of `master`:
anchoring the handle inside the video rather than the window, and handing
the cursor to the overlay while the stream holds the mouse. Neither is in
the pinned client. If phase 2's fork half is the next job, that branch is
probably the base you want rather than `master`. Everything else over there
(`SDL3`, `weblate`, `appimage_kms`, dependabot) is inherited from upstream.

`linux-first-run` is **merged** — what it carried is described under
*Flatpak* below, and it matters on this machine.

## Setting up on Bazzite

Bazzite is immutable, so `dnf install` is not the move. Use a distrobox and
keep the toolchain in it:

```sh
distrobox create --name dusk --image fedora:41
distrobox enter dusk

sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel \
  librsvg2-devel libsoup3-devel patchelf openssl-devel \
  gcc gcc-c++ make git

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo dnf install -y nodejs npm      # or nvm, Node 22
```

Those are the Fedora names for the same set `ci.yml` installs under Ubuntu
names — they are not interchangeable, so copying the workflow's list will not
work here.

Then, in the repo:

```sh
npm install
npm run dev
```

### Getting the overlay in a dev build

A debug build looks for the forked client in a **sibling checkout** at
`../moonlight-qt/app/moonlight`. Building the fork on Linux means Qt6, SDL,
FFmpeg and libplacebo from source — hours. Don't. Download the published
client instead and put it where the search already looks:

```sh
gh release download dusk-v0.2.1 --repo darkharasho/moonlight-qt \
  --pattern 'dusk-client-linux-x86_64.tar.gz'
tar xzf dusk-client-linux-x86_64.tar.gz
mkdir -p ../moonlight-qt/app && mv moonlight ../moonlight-qt/app/moonlight
chmod +x ../moonlight-qt/app/moonlight
```

It is an AppImage, so FUSE has to work — on Bazzite it does. Dusk logs which
client it picked at startup; `using Dusk's Moonlight client at ...` is the
line you want, and `using system Moonlight ...; the in-stream overlay will be
unavailable` means it did not find this.

## What this machine makes possible for the first time

Bazzite is a real Sunshine host, and it is already in the grid. Two things
have never been exercised by anyone:

**Flatpak, and why it matters here.** A Flatpak Moonlight — how most
distributions ship it, and the only way to install one on an immutable
system — writes its settings *inside the sandbox*, at
`~/.var/app/com.moonlight_stream.Moonlight/config`, not `~/.config`. That one
file holds both the client identity and the remembered-host list, so looking
only at the native path loses TLS probing and a whole discovery source at
once, silently. Dusk now checks both, and narrows to the store belonging to
the client it will actually launch — a Flatpak and a native client keep
separate identities, and reading the wrong one makes Dusk offer to stream to
a host the client it runs has never paired with. See
`src-tauri/src/moonlight/mod.rs`. If pairing state looks wrong on Bazzite,
start there and check which client `discover` picked.

**The Linux host backend.** `host/platform.rs` drives Sunshine through
`systemctl --user` — Sunshine ships a *user* unit, so every call carries
`--user` and the system manager does not know the unit exists. Probe, start
and stop have been compiled and never run. If Bazzite's Sunshine is a Flatpak
rather than a user unit, `probe` will report `NotInstalled` and that is the
first thing to look at.

**The client path end to end.** Pair, list apps, launch, quit — built,
compile-checked, never tested against a second machine. The Mac can be the
client and Bazzite the host, which is the pair that has been missing.

Both are more valuable than any amount of further building, because they are
the parts the plan assumes work.

## Next work: phase 2's fork half

Dusk's side is done and dormant. It binds a socket per session and hands the
client two environment variables:

```
DUSK_SESSION_ENDPOINT   unix socket path (named pipe on Windows)
DUSK_SESSION_TOKEN      32 hex characters, one session only
```

Environment and not argv on purpose: moonlight-qt rejects arguments it does
not recognise, so an argument would break every client that has not learned
the protocol. Unknown environment variables are ignored, which is why Dusk's
half could ship first.

What the fork has to do:

1. On startup, if both variables are set, connect to the endpoint.
2. Send `{"type":"ready","token":"<token>"}` first. Nothing else is acted on
   until that lands, and a wrong token closes the connection.
3. When the overlay menu dispatches an action, send
   `{"type":"overlay_action","action":"disconnect"}` — or `settings`, or
   `quality`.
4. Optionally send `{"type":"stats","fps":59.5,"bitrateKbps":18000}`. Every
   field is optional; Dusk parses and currently drops them.

The hook point already exists. `app/streaming/video/duskoverlay.h` has:

```cpp
enum Action {
    ActionStats, ActionSwitchDisplay, ActionFullScreen,
    ActionReleaseMouse, ActionMinimize, ActionDisconnect, ActionMax
};
```

`ActionDisconnect` is the one to mirror out over the socket first — it is the
whole point of the floating button. The protocol and every message is
specified in `src-tauri/src/session/protocol.rs`, which is the file to read
rather than this summary.

Dusk already answers `overlay_action: settings` by raising its window, and
`disconnect` by asking the client to end cleanly. `ToClient::SetQuality`,
`ShowOverlay` and `HostDisplay` are defined and have no caller yet, by
design, so the fork has something fixed to implement against.

**This is the first piece that cannot be verified from a keyboard alone.**
It needs a real stream against a real host to know the overlay actually
drives Dusk.

## Landmines

Things that cost real time. Most were invisible to a green build.

**Compiling is not shipping.** Three of the four bugs in this stretch passed
every CI job and only surfaced on inspecting the output — the notarization
log, a `bash -x` trace, the published artifact's plist. When a change is
supposed to alter an artifact, read the artifact back.

**Never rename the QSettings triple in the fork.** `app/main.cpp` sets the
organization name, organization domain and application name, and Qt derives
the settings store from them:

```
Linux    ~/.config/Moonlight Game Streaming Project/Moonlight.conf
macOS    ~/Library/Preferences/com.moonlight-stream.Moonlight.plist
Windows  HKCU\Software\Moonlight Game Streaming Project\Moonlight
```

That store holds the client certificate and key every host pairing is bound
to, and Dusk reads the identity from those exact paths. Renaming any of them
unpairs every machine and hides the identity from Dusk. The macOS bundle
identifier is the same string and is pinned for the same reason. Both sites
say so in a comment.

**Tauri's resource copier does not preserve symlinks.** This is why macOS
inserts its client after bundling rather than via `bundle.resources`. A Qt
framework is a versioned bundle whose top-level binary is a symlink into
`Versions/Current`; flattening it doubles the size and breaks the structure
the signature seals, and notarization rejects it. Linux and Windows have no
symlinks to lose, so they still stage before the build. All of it is written
up at the top of `scripts/package-macos.sh`.

**`codesign` reads standard input.** Inside a `while read` loop, the first
call swallows the rest of the file list and the loop ends after one file —
which looks exactly like success.

**A later duplicate key wins in a plist, and `plutil -lint` does not care.**
The phase 1 rename shipped and did nothing because `app/Info.plist` already
had a `CFBundleDisplayName` further down.

**A push filter naming only tags turns the workflow off for branches.**
Adding `tags-ignore` to the fork's `build.yml` silently stopped CI on every
ordinary push until `branches: ['**']` was added beside it.

**`gh` needs a repo.** A job that does not check out has no git remote to
infer one from; set `GH_REPO` rather than adding a pointless checkout.

## Commands

```sh
npm run dev                       # real discovery
DUSK_MOCK=1 npm run dev           # fixtures, no network

npm run typecheck
cd src-tauri && cargo test
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings

./scripts/stage-client.sh         # Linux/Windows: client in before the build
./scripts/package-macos.sh <app>  # macOS: client in after it
```

`scripts/check-windows.sh` cross-compiles the Windows build from a Mac and
has caught a compile error on three separate occasions. There is no Linux
equivalent and there does not need to be — on Bazzite the Linux build is the
native one.

## Cutting a release

1. Bump the version in `package.json`, `src-tauri/tauri.conf.json` and
   `src-tauri/Cargo.toml` — all three.
2. Add a `Version vX.Y.Z — <date>` section at the top of `RELEASE_NOTES.md`.
   The publish job fails rather than shipping empty or stale notes, and it
   reads them *before* creating anything so a missing section leaves no
   half-made release behind.
3. Tag on `main` and push. `workflow_dispatch` runs the same builds without
   publishing, which is the way to exercise signing without cutting a tag.
