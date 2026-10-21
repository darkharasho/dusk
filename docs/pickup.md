# Picking this up elsewhere

Written to move work from the Mac it was started on to the Bazzite machine.
`docs/architecture.md` is the plan; this is where the plan actually stands,
what to set up, and what bit hard enough to be worth not rediscovering.

## Where things stand

**v0.3.15 is released**, all three platforms, from `main`, carrying the
client's `dusk-v0.2.4`. **Read this before touching the account code.** The
OAuth app was set to expire user tokens at eight hours, and `parse_poll`
kept only `access_token`. Eight hours after any sign-in the machine could
no longer read the account's machine list, so every pairing proof from one
of your own machines failed verification and presented as "you are not
signed in to the same GitHub" — while the Account screen still showed the
user as signed in. All four machines were correctly enrolled in the gist
throughout; only the token was dead. Diagnose this with
`github-authentication-token-expiration` in any API response header.
**Renewing is not possible from Dusk**: `grant_type=refresh_token` with only
a `client_id` answers `incorrect_client_credentials` — the refresh grant is
a confidential-client operation and wants the client secret, which a
desktop app cannot hold (`strings` finds it) and which this project will
not run a server for. The fix is on GitHub's side: turn **off** "Expire
user authorization tokens" in the OAuth app settings. Dusk's half is
`github::Tokens::expired()` plus `AccountView::expired`, a notice on the
Account screen and an `Account:` line in `dusk status`. `Stored` gained
`refresh`/`expires_at` behind `#[serde(default)]`, so entries written
before v0.3.15 load untouched — they carry no expiry and so are never
reported as lapsed, which is correct since nothing could be done about them
anyway. A disproved theory worth not re-running: signing a second machine in
does **not** revoke the first one's token; both were 200 simultaneously.
v0.3.14 added `dusk signin`: GitHub's device code printed
to stdout and polled, so a headless host can join the account — the last
step that still needed the window, and the one without which a headless
machine can never be PIN-free-paired with. `account::wait_for_token` and
`account::finish` moved off `&AppState` onto `(&reqwest::Client,
&AccountState)`, the same narrowing `provision` had, so the window and the
terminal share one implementation; `finish` returns the `User`. Note
`github::client_id()` reads the **runtime** env var before the compile-time
one, so `DUSK_GITHUB_CLIENT_ID=… dusk signin` works against a dev build
without rebuilding. v0.3.13 made `secrets/mod.rs` the one place that decides
where this machine's secrets live; `account/mod.rs` and
`sunshine/credentials.rs` both go through it instead of reaching for
`keyring::Entry`. Two backends: the OS keystore (default, always) and blobs
sealed with `systemd-creds encrypt --with-key=tpm2` under
`$XDG_DATA_HOME/dusk/sealed`. Which one is in use is decided by that
directory existing, not by a setting — a setting would have to be read
identically by the window and by `dusk setup` from two different config
paths, and the day they disagreed one would write a secret where the other
never looks. `dusk setup --store=tpm` turns it on and carries existing
entries over; it refuses if there is no TPM rather than falling back to the
clear. The reasoning for why sealing is not a downgrade is in the module
doc and worth reading before changing it: a keyring protects a secret only
while *locked*, and in any logged-in session it is unlocked and readable
over D-Bus by anything running as you, so sealing matches it on both
threats while needing no password. `dusk setup --unlock-keyring` reads a
password from **stdin** (never argv) and unlocks the login collection via
`org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface.UnlockWithMasterPassword`
over zbus, leaving its password alone. **Do not** reach for
`gnome-keyring-daemon --unlock`: measured on Venus it left the keyring
locked *and* `--replace` tore down the session's own daemon, taking
`GNOME_KEYRING_CONTROL` and `SSH_AUTH_SOCK` with it. `service.rs` now has
`LIFECYCLE_TIMEOUT` (90s) separate from the 10s query budget: Sunshine's
terminate handler waits 10s before forcing shutdown, so a restart measured
10.13s against a 10s limit and `dusk setup` twice reported "systemctl did
not answer" for work that had succeeded. One more Linux gotcha from the
same session: a Sunshine with an empty `sunshine.conf` auto-detects capture,
cannot gain `CAP_SYS_ADMIN` from an AppImage, finds no `WAYLAND_DISPLAY`,
fails `[portalgrab] RemoteDesktop CreateSession`, and then **hangs without
binding any port at all** — `capture = x11` in `sunshine.conf` fixes it on
an X11 session. v0.3.12 added `dusk setup` and `dusk status` (`cli.rs`),
handled in `main.rs` before Tauri starts — the same hook the elevated driver
helper uses — so a machine with no display can be set up. The work list is
`install::steps::build`, the same call `get_setup` makes, and each step runs
the same function the button runs; `provision` moved from `commands.rs` to
`sunshine::provision::run(&dyn HostBackend, &CredentialStore)` so both share
it. Optional steps (the Windows display driver) are never run unasked, and
steps marked Unknown don't fail the run. The bundled installer only, never
the download: a headless run can't show what it is about to fetch.
`resource_dir()` rediscovers what `app.path().resource_dir()` would give —
`/usr/lib/Dusk` on Linux, the exe dir on Windows, `../Resources` on macOS —
by looking for `sunshine/bundled.json`. The Linux user unit is now
`WantedBy=graphical-session.target xdg-desktop-autostart.target`: Ubuntu's
GNOME launches autostart entries as its own `app-gnome-*.scope` units and
never activates `xdg-desktop-autostart.target`, so the unit was enabled and
never started and hosting didn't survive a reboot. `register_user_unit`
still won't overwrite an existing unit, so this only reaches hosts set up
from v0.3.12 on. **Known gap:** WebKitGTK under Xorg ignores XTEST synthetic
input, so the window can't be driven with `xdotool` — use the CLI for
headless work rather than trying to automate the UI. v0.3.11 fixes three Windows faults in the elevation and
spawn layer (`host/service.rs`). `Start-Process -ArgumentList` takes an
array, joins it with spaces and quotes nothing, so the staged installer's
path — always under `C:\Program Files\Dusk\resources` — reached `msiexec`
as `C:\Program`: Setup › Install hosting had never worked on any Windows
machine. `elevation_script` now builds a real Windows command line
(`command_line`/`quote_argument`, `CommandLineToArgvW` rules) and passes it
as one string; `platform.rs` had already hit this and dodged it with
`-EncodedCommand` for the creds script. Everything elevated goes through
that hop, so `sc`, `netsh` and the driver install were all one space away
from the same bug. `run_for` now sets `CREATE_NO_WINDOW` (tokio's inherent
method, not std's `CommandExt`): `sc query` on the five-second poll tick was
opening a console window each time. Settings › Hosting offers "Go to setup"
when nothing is installed instead of a disabled button. `trafficLightPosition.y`
is not a top offset — it sets the titlebar container to `buttonHeight + y`
and the 14pt button sits 9pt above its bottom, so the centre is at `y - 2`;
21 centres it in the 38px strip. Untested on real Windows hardware as of
tagging: check the install on R-3X first. v0.3.10 adds Settings › Hosting › "Stream a screen sized
to the other device" (`host/virtual_screen.rs`), Windows only, needing the
virtual display driver. Enabling it (one UAC prompt) merges `MODES` × `RATES`
into `C:\VirtualDisplayDriver\vdd_settings.xml`, restarts the device with
`pnputil /restart-device` (the driver only rereads resolutions on a device
restart), restarts the host, reads the driver's device id from the host log
(it is only logged at startup), and sets `output_name` to it with
`dd_configuration_option = ensure_only_display` and resolution/refresh on
`auto`. The peer `display` request now carries the client's `screen`; a host
with the setting on answers `best_mode` from the modes the XML lists, because
an unlisted mode makes the host revert to the physical monitor. Untested on
real Windows hardware as of tagging: check it on Venus first. Fixed and
Match-this-display resolutions don't consult the host, so they only land on
the virtual screen at a listed size. v0.3.9 added Settings › Streaming › Bitrate (Automatic
or 5–150 Mbps, `--bitrate`) and Video codec (Automatic, H.264, HEVC, AV1,
`--video-codec`, passed only when not Automatic). The client now keeps the
in-stream handle inside a letterboxed picture on VDPAU too, which is what
an NVIDIA card picks on Linux; before, its handle rect was never set. The
Windows firewall step now also allows Dusk itself on TCP (the peer port,
47850), deleting any block rules Windows left for dusk.exe first: without
it "Match the host's screen" timed out and fell back to 1080p. A Linux
host running firewalld blocks the peer port the same way and Dusk does
not handle that yet. v0.3.8 was a UI polish pass (steady masthead, cards
with latency or "Seen … ago", a slim This machine card, the settings rail
with icons). v0.3.7 fixed v0.3.6, which deadlocked building its first
snapshot (two `store.lock().await`s in one struct literal) and so opened
to an empty window; `state::tests::snapshot_does_not_hang` guards it.
Since v0.3.6 Dusk streams at the host's screen
size by default (Settings › Streaming › Resolution; the host's Dusk
answers a `display` question on the peer port, and anything that can't
gets 1080p). The rarer renderers (VA-API direct, DXVA2, DRM, MMAL,
AVSampleLayer) still place the handle relative to the window.
<https://github.com/darkharasho/dusk/releases/tag/v0.3.15>

**Startup settings** (Settings › Startup): open at sign-in, start in the
tray, keep running when the window is closed; on macOS the dock icon
follows the window. `src-tauri/src/startup.rs`, with
`tauri-plugin-autostart` for the sign-in entry and
`tauri-plugin-single-instance` so a second launch shows the window instead
of opening another Dusk. The window is created hidden and shown in setup
unless a `--background` launch says otherwise. Compile- and unit-tested on
Linux, built in CI for all three; not run as an app yet.

**"Looking for machines"** is a timed sweep (`discovery::sweep`, six
seconds) at startup, after Refresh, and when the window comes back from
hidden. mDNS browsing never ends, so the old
flag tied to it stayed on forever.

| Phase | State |
| --- | --- |
| 0 — bundle the fork, fix the client search path | done, verified in artifacts |
| 1 — rename and identity | done, verified in artifacts |
| 2 — control socket, overlay actions, live stats | **done and live-tested; client `dusk-v0.2.3`, shipped since v0.3.0; quality not started** |
| 3 — bundle Sunshine, provision credentials | built, not live-tested |
| 4 — installer, service registration | built, not live-tested; Windows display driver built, not run on Windows |
| 5 — zero-click pairing | built, not live-tested |
| 6 — window choreography | built, not live-tested |

The fork's half of phase 2 is merged to its `master` and published as
`dusk-v0.2.3`, which `CLIENT_TAG` pins. v0.3.0 was the first Dusk release
to ship it, with phases 3–6. See *Phase 2* and *Phases 3–6* below.

## The map

Two repositories, side by side. Several things assume they are siblings.

```
<parent>/
├── dusk/            main             tags v0.1.0, v0.2.0, v0.3.0, v0.3.1, v0.3.2, v0.3.3, v0.3.4, v0.3.5 (never published: failed its formatting check), v0.3.6, v0.3.7, v0.3.8, v0.3.9, v0.3.10, v0.3.11, v0.3.12, v0.3.13, v0.3.14, v0.3.15
└── moonlight-qt/    master @ 7b39d282  tags dusk-v0.1.0 … dusk-v0.2.4
```

The fork builds and publishes a client; Dusk downloads a pinned one at
release time. That pin is one line — `CLIENT_TAG` in
`.github/workflows/release.yml`, currently `dusk-v0.2.4`. Bumping the client
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
gh release download dusk-v0.2.3 --repo darkharasho/moonlight-qt \
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
- Dusk's UI sends `Disconnect` (**Leave the stream**, beside the live numbers,
  which leaves the game running on the host, unlike **End the session**).
  Nothing sends `ShowOverlay` or `HostDisplay` yet. `ShowOverlay` is no use
  from Dusk's window until the fork also restores a stream window that the
  overlay's Open Dusk minimised. `HostDisplay` needs the host's display list,
  which Dusk does not have.

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
  `src-tauri/licenses/` carries the GPL text and source offer; CI writes
  `versions.txt`.
- **Embedded hosting on macOS (3, after v0.3.0).** No installer: the app
  itself ships inside Dusk. `scripts/stage-host-macos.sh` downloads both
  per-CPU Sunshine images, checks their digests, merges them with `lipo`,
  and renames the result **Dusk Host** (`dev.dusk.app.host`, `LSUIElement`),
  so Screen Recording and Microphone list it by that name. `package-macos.sh
  --host` signs it into `Contents/Helpers` with upstream's entitlements
  (`audio-input`). "Installing" is `install/embedded.rs` +
  `apply::install_embedded`: a launch agent pointing at the helper, refused
  while Dusk runs from the disk image or translocated. An agent left by a
  copy of Dusk that has moved is rewritten; anyone else's is left alone.
  `MacosHost` finds the bundle from the agent's program. Built, signed and
  notarized in CI (v0.3.1); **not yet run on a Mac**.
- **Service registration (4).** The AppImage gets a systemd user unit, the
  macOS app a launch agent; an existing one is left alone. Linux has a new
  step for the uinput udev rule via polkit, which recognises Bazzite's own
  rule as done. Windows firewall is one prompt, not two. **Set up this
  machine** runs every automatable step in order, apart from optional ones.
- **Virtual display (4, Windows).** `install/display_driver.rs`. The driver
  is the Virtual Display Driver from github.com/VirtualDrivers: MIT,
  user-mode IddCx, signed through SignPath, and the one Sunshine's community
  uses. It is pinned to release `25.7.23`, and the driver-only zip's sha256
  is written in the source. Moving the pin means downloading the new zip,
  hashing it, and editing both constants. It is an *optional* step, which
  **Set up this machine** skips, and it asks for its own confirmation. Dusk
  restarts itself elevated with `--dusk-install-display-driver <inf>`
  (`main.rs`). That helper makes a `Root\MttVDD` device through SetupAPI and
  runs `UpdateDriverForPlugAndPlayDevicesW`, which is what `devcon install`
  does, and removes the device again if that fails. It copies the package's
  `vdd_settings.xml` to `C:\VirtualDisplayDriver` unless one is already
  there. Done-detection reads `HKLM\...\Enum\ROOT\*\*\HardwareID`.
  Cross-checked with `cargo xwin clippy`; **never run on Windows.**
- **Zero-click pairing (5).** `peer.rs`. Each Dusk listens on TCP 47850.
  Pairing starts Moonlight with a fresh PIN and sends it to the host's Dusk,
  which shows **Allow** naming the machine and enters the PIN into Sunshine
  itself. No Dusk there falls back to showing the PIN. Checked here only as
  far as the decline path ("not set up to host yet"), because this Dusk does
  not manage its Sunshine. Firewalls must allow 47850; Windows will prompt.
- **Choreography (6).** Dusk hides once the client authenticates on the
  session channel and comes back when it exits, or on a failure. On macOS
  Dusk's own dock icon now goes with its window (`tray::set_dock_icon`,
  on the `startup-settings` branch), so during a stream only the client's
  shows. The client's icon is left alone: hiding it changes focus and input
  grab, and wants a Mac to test on.

## Its own app: rebrand, Settings tab, GitHub sign-in

- **No Sunshine or Moonlight in the UI.** User-visible copy says "hosting" and
  "streaming client". The code keeps its module names. One deliberate leftover:
  the GPL notice under Settings › About › Open-source licences (the licence
  asks for it to be reachable). The macOS privacy steps now say "Dusk Host",
  the embedded helper's name.
- **Settings tab.** `SettingsPage.tsx` has the sections Account, Hosting
  (`HostingPanel.tsx`, the old SelfCard), Setup, Host settings and About.
  `SelfCard.tsx` is now a summary on the Machines tab that links into it.
- **Sign in with GitHub** (`src-tauri/src/account/`). Uses the device flow,
  with scope `gist` and no backend. Each machine has an Ed25519 key in the
  keystore and publishes its public key to one secret gist, "Dusk machines",
  with one file per machine. A pair request from a signed-in machine carries
  `proof` (account id, machine id, and a signature over the PIN). The host
  pairs without a prompt when the account matches and the key checks out
  against the gist; anything else falls back to the Allow prompt.
  - **Needs an OAuth App:** GitHub › Settings › Developer settings › OAuth
    Apps › New, with **Enable Device Flow** ticked. Set its client ID as the
    repo variable `DUSK_GITHUB_CLIENT_ID`; `release.yml` bakes it in. That
    is done: the app exists and the variable is set. For dev, put
    `DUSK_GITHUB_CLIENT_ID=...` in the repo's untracked `.env`, which
    `build.rs` reads, or export it. Without it, the Account section says the
    build has no sign-in. Signing in has been checked live on Bazzite. A
    pair between two signed-in machines has not.
  - This deliberately revises CLAUDE.md's "no account system". GitHub is the
    only party; there is still nothing Dusk hosts.
- **Own titlebar.** `components/Titlebar.tsx`. Windows and Linux have no
  native frame (`decorations: false`), so the strip draws the buttons and is
  the drag region (`data-tauri-drag-region`, which does not reach children).
  macOS keeps its traffic lights over the strip (`tauri.macos.conf.json`,
  `titleBarStyle: Overlay`). The page under it is `.dusk-body`, the one
  scrolling region; drawers start below the strip. Top-layer `<dialog>`s
  still cover it. Not yet seen on a real Windows or macOS window.
- **Self-update.** `src/updates.ts`, `tauri-plugin-updater`. Checks 10s after
  launch and every 6 hours, downloads in the background, then shows **Restart
  to update** in the titlebar and in Settings › About. Never restarts on its
  own: Dusk may be hosting or mid-stream. Off on a `.deb` (the package
  manager's job) and in debug builds (`updates.rs`). The manifest is
  `latest.json` on the latest release, written by
  `scripts/write-latest-json.sh`. MSI and NSIS installs each update through
  their own installer. The macOS archive is re-made in CI from the finished,
  notarized app, because Tauri's own is from before the client and host went
  in.
  - **Signing key:** `~/.tauri/dusk.key` (no password), public half in
    `tauri.conf.json`. CI reads the repo secret `TAURI_SIGNING_PRIVATE_KEY`.
    Lose the key and installed copies can never update again; they would
    need a manual reinstall. `npm run tauri build` uses the local key, or
    skips the update archives if it is missing.
  - The first release with the updater can only be installed by hand; the
    first automatic update is the release after it.

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
