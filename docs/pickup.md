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
| 2 — control socket, overlay actions, live stats | **done and live-tested; client tagged `dusk-v0.2.2`; no Dusk release carries it yet; quality not started** |
| 3 — bundle Sunshine, provision credentials | built, not live-tested |
| 4 — installer, service registration | built, not live-tested; no display driver |
| 5 — zero-click pairing | built, not live-tested |
| 6 — window choreography | built, not live-tested |

The fork's half of phase 2 is merged to its `master` and published as
`dusk-v0.2.2`, which `CLIENT_TAG` now pins. The next Dusk release is the
first to ship it, and phases 3–6 with it. See *Phase 2* and *Phases 3–6*
below.

## The map

Two repositories, side by side. Several things assume they are siblings.

```
<parent>/
├── dusk/            main @ 2a2cba4   tags v0.1.0, v0.2.0
└── moonlight-qt/    master @ 4670364e  tags dusk-v0.1.0 … dusk-v0.2.2
```

The fork builds and publishes a client; Dusk downloads a pinned one at
release time. That pin is one line — `CLIENT_TAG` in
`.github/workflows/release.yml`, currently `dusk-v0.2.2`. Bumping the client
means tagging `dusk-v*` in the fork, waiting for its release, and editing
that line.

`main` is the only branch left in dusk. In the fork, `dusk-overlay` and
`dusk-session-channel` are both merged into `master`, so new fork work
starts from `master`. Everything else over there (`SDL3`, `weblate`,
`appimage_kms`, dependabot) is inherited from upstream.

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

On the Bazzite machine this was written for, none of that turned out to be
needed: Node (nvm) and Rust run on the host, and `npm run dev` works there.
`npm run dev` quietly swaps in `/usr/bin/pkg-config` when a linuxbrew one
shadows it (`scripts/tauri.mjs`); running `cargo` directly does not, so use
`PKG_CONFIG=/usr/bin/pkg-config cargo test`.

### Getting the overlay in a dev build

A debug build looks for the forked client in a **sibling checkout** at
`../moonlight-qt/app/moonlight`. Building the fork on Linux means Qt6, SDL,
FFmpeg and libplacebo from source — hours. Don't. Download the published
client instead and put it where the search already looks:

```sh
gh release download dusk-v0.2.2 --repo darkharasho/moonlight-qt \
  --pattern 'dusk-client-linux-x86_64.tar.gz'
tar xzf dusk-client-linux-x86_64.tar.gz
mkdir -p ../moonlight-qt/app && mv moonlight ../moonlight-qt/app/moonlight
chmod +x ../moonlight-qt/app/moonlight
```

It is an AppImage, so FUSE has to work — on Bazzite it does.

To change the fork you do have to build it, and on Bazzite that is minutes,
not hours: the `fedora44-build` distrobox has the Qt6/SDL2/FFmpeg `-devel`
packages, and the binary it makes runs on the host because the sonames
match. Two catches. The host has no `libSDL2_ttf-2.0.so.0`, so copy it into
`app/` and give the binary an `$ORIGIN` rpath; and every relink drops that
rpath, so re-run `patchelf` after each `make`. Both tools live in the box:

```sh
distrobox enter fedora44-build -- bash -lc '
  cd ~/Documents/GitHub/moonlight-qt && qmake6 moonlight-qt.pro && make -j8 &&
  cp -L /usr/lib64/libSDL2_ttf-2.0.so.0 app/ &&
  patchelf --set-rpath "\$ORIGIN" app/moonlight'
./app/moonlight --version    # "Dusk fork: in-stream overlay menu available"
```

Either way, Dusk logs which client it picked at startup; `using Dusk's Moonlight client at ...` is the
line you want, and `using system Moonlight ...; the in-stream overlay will be
unavailable` means it did not find this.

## What this machine makes possible for the first time

Bazzite is a real Sunshine host, and it is already in the grid. What has
now been checked there, and what still has not:

- Dusk picks the sibling fork client, the "This machine" panel reads
  Sunshine as hosting, and paired hosts come up READY with their app lists,
  so the identity store resolves.
- A stream launched from Dusk, ended from the overlay's Disconnect, works.
- **Not yet run:** Dusk starting and stopping Sunshine. Sunshine here is a
  linuxbrew binary behind a user unit called `sunshine.service`, which
  `LINUX_UNITS` already covers. Flatpak Sunshine is installed too.

The original notes, for context:

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

## Phase 2: the control channel

Dusk binds a socket per session and hands the client two environment
variables:

```
DUSK_SESSION_ENDPOINT   unix socket path (named pipe on Windows)
DUSK_SESSION_TOKEN      32 hex characters, one session only
```

Environment and not argv on purpose: moonlight-qt rejects arguments it does
not recognise, so an argument would break every client that has not learned
the protocol. Unknown environment variables are ignored.

The protocol is specified in `src-tauri/src/session/protocol.rs`; read that
rather than a summary. Where each side stands:

- **Fork, `app/streaming/duskchannel.{h,cpp}`.** Connects once the stream is
  up, sends `ready` first, and says `disconnected` at teardown. The socket
  has its own thread because `Session::exec` holds the main thread and Qt's
  event loop is suspended for the whole stream. Messages from Dusk come
  back as SDL events: `disconnect` quits, `show_overlay` opens the menu,
  `host_display` presses Ctrl+Alt+Shift+F*n* on the host. `set_quality`
  only logs that it is unsupported. Each time the decoder rolls its
  one-second stats window it sends `stats`, measured the way the debug
  overlay measures them.
- **Overlay.** Disconnect sends `overlay_action: disconnect` and then quits
  without waiting on Dusk. An **Open Dusk** row, shown only when Dusk
  launched the stream, sends `settings`. It also releases the mouse and
  minimises a full-screen stream, so the window Dusk raises is not hidden.
- **Dusk** logs the handshake and each overlay action (`dusk: the client
  connected to the session channel`), which is the only way to tell from
  a log that the channel was used: Disconnect ends the stream either way.
  Stats go to the frontend as `dusk://session-stats`, and the device drawer
  shows them under *This stream* while that machine is streaming here.

Not done yet:

- `quality` has no row, and `SetQuality` needs the stream renegotiated.
- Nothing calls `ToClient::ShowOverlay` or `HostDisplay` from Dusk's UI.
- The client is tagged (`dusk-v0.2.2`) and pinned, but no Dusk release
  carries it until the next `v*` tag.

## Phases 3–6: built, waiting on real machines

All Dusk-side; the fork did not change. Each is unit-tested and the Windows
build cross-checks clean, and none has run end to end yet.

- **Credentials (3).** `sunshine/provision.rs` makes up a `dusk` / 32-char
  credential, and `HostBackend::set_credentials` runs `sunshine --creds` with
  the service's own binary and restarts it. Dusk keeps it in the keystore
  and checks Sunshine accepts it. It runs after Dusk installs Sunshine, and
  as **Let Dusk manage this host** for one that was already there — which
  replaces the password Sunshine's web page knew. The sign-in form is gone.
  `--creds` was checked against a throwaway config on Bazzite; the full
  path, which restarts the real service, was not run, so as not to replace
  the password on this machine unasked.
- **Bundled Sunshine (3).** `SUNSHINE_TAG` in `release.yml` pins it;
  `scripts/stage-sunshine.sh` fetches and checks the installer, and
  `install/bundled.rs` installs from it after re-checking the checksum.
  Linux and Windows only: a disk image nested in a notarized macOS app is
  untested with Apple's notary, so macOS still downloads. `src-tauri/licenses/`
  carries the GPL text and source offer; CI writes `versions.txt`.
- **Service registration (4).** The AppImage gets a systemd user unit, the
  macOS app a launch agent; an existing one is left alone. Linux has a new
  step for the uinput udev rule via polkit, which recognises Bazzite's own
  rule as done. Windows firewall is one prompt, not two. **Set up this
  machine** runs every automatable step in order. Not done: a consented
  virtual display driver install — which driver to trust is a decision, not
  an implementation detail.
- **Zero-click pairing (5).** `peer.rs`. Each Dusk listens on TCP 47850.
  Pairing starts Moonlight with a fresh PIN and sends it to the host's Dusk,
  which shows **Allow** naming the machine and enters the PIN into Sunshine
  itself. No Dusk there falls back to showing the PIN. Checked here only as
  far as the decline path ("not set up to host yet"), because this Dusk does
  not manage its Sunshine. Firewalls must allow 47850; Windows will prompt.
- **Choreography (6).** Dusk hides once the client authenticates on the
  session channel and comes back when it exits, or on a failure. Nothing was
  done about the macOS dock: hiding the client's dock icon changes focus and
  input grab, and wants a Mac to test on.

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
