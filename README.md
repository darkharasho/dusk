# dusk

A single desktop app that wraps Sunshine (host) and Moonlight (client) behind one
device-centric UI, so hosting and connecting stop feeling like two separate
programs. Networking is accepted as-is — LAN or your own VPN, using Sunshine's
existing pairing and mDNS discovery — so there are no accounts, rendezvous
servers, or relay infrastructure to run. The payoff is a device grid where every
machine appears as a card showing online, paired, and hosting state.

## Status

M0 and M1 are done: the skeleton, the device model, the host abstraction, and a
working device grid fed by real mDNS discovery and `serverinfo` polling.
Streaming, host control, and the installer are not built yet.

| Milestone | What it covers | State |
| --- | --- | --- |
| M0 | Tauri shell, device model, `HostBackend` trait + mock | done |
| M1 | mDNS + manual address book, liveness polling, device grid | done |
| M2 | Client path — pair, list apps, launch via `moonlight-qt` | built; identity, remembered hosts and the app list verified on Linux against a real host |
| M3 | Host path — Sunshine service control on all three platforms | built; probe verified on macOS, probe/start/stop verified on Linux |
| M4 | Config UI — schema-driven, replaces Sunshine's web UI | built; needs a signed-in host to exercise |
| M5 | First-run install per platform | built; download path verified against real releases |

## Running it

```sh
npm install
npm run dev                  # real discovery on the local network
DUSK_MOCK=1 npm run dev      # fixtures, no network, for UI work
```

`npm run dev` starts the whole app — Vite and the Tauri window. `dev:vite`
serves the frontend alone, and exists only because Tauri's `beforeDevCommand`
has to call something other than `dev` to avoid recursing into itself. Opening
it in a browser gets you a page with no backend, so it is rarely what you want.

`DUSK_MOCK=1` seeds one device in each card state and swaps in a mock host
backend. It exists because a Mac cannot usefully run Sunshine (see below), and
the rest of the UI should not be blocked on that.

```sh
npm run typecheck                 # frontend
cd src-tauri && cargo test        # backend
cd src-tauri && cargo test -- --ignored    # download tests; hit the network
DUSK_TEST_HOST=192.168.1.40 cargo test -- --ignored   # + the TLS guard, needs a paired host
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings
python3 scripts/make_icon.py      # regenerate the app icon set
```

CI runs the backend on Linux, macOS and Windows. That matrix is the point of
it: two of the three host backends, the Windows registry reader and every
elevation path only compile on their own platform, and this is developed on
a Mac — so without it they would rot unnoticed. The network download tests
are excluded there, because an outage or a rate limit would produce a red
build that says nothing about the commit.

### Checking the Windows build without Windows

```sh
brew install llvm lld
cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc

./scripts/check-windows.sh        # ~30s cold, ~2s warm
```

Worth the setup: the first two CI failures on this project were both
Windows-only compile errors that no amount of local testing could have
found. This runs the same compile and the same lints against
`x86_64-pc-windows-msvc` in seconds instead of a push-and-wait round trip.

It cross-compiles, it does not run. A test that passes here is a test that
*builds* here — the third CI failure was tests calling `/bin/sleep`, which
compiles on Windows perfectly well and then fails at runtime. CI is still
the only place Windows tests actually execute.

## How it fits together

```
mDNS (_nvstream._tcp) ──┐
manual address book ────┼─→ Registry ─→ poller ─→ serverinfo ─→ Snapshot ─→ UI
moonlight-qt's hosts ───┘
```

- **`registry.rs`** is the interesting file. Three sources feed it and the same
  machine routinely appears in both, or twice over mDNS on two interfaces.
  Devices are keyed by address until `serverinfo` returns Sunshine's `uniqueid`,
  at which point the entry is re-keyed and merged. That is what makes "one
  machine on LAN and VPN" a single card.
- **`moonlight/hosts.rs`** is the third source, and the one that stops the
  grid being shorter than Moonlight's. mDNS only finds what is advertising
  this second: on the network this was written against, four machines were
  known to Moonlight, one was advertising, and one of the three missing was
  up and answering `serverinfo` perfectly well while publishing nothing over
  multicast. Moonlight shows them because it remembers them, so Dusk reads
  the same list. Its `uuid` is byte-for-byte Sunshine's `uniqueid`, so a
  remembered host lands on the key a probe would have given it — the merge
  is exact rather than an address heuristic. `remoteaddress` is dropped on
  the way in: it is the public IP, which every machine behind one router
  shares, so keeping it would fold them all into a single card.
- **`serverinfo.rs`** talks to the stable GameStream endpoint rather than
  Sunshine's config API. Moonlight depends on it, so it cannot change freely.
- **`moonlight/cli.rs`** drives moonlight-qt for the three things that need
  it — pair, stream, quit. Everything it asserts about that binary was
  measured, not assumed: it exits `255` on failure, puts all human-readable
  output on stderr behind a log banner, and `quit` against a host that will
  not answer **hangs forever**, which is why every run carries a timeout.
- **`applist.rs`** gets the app list from GameStream rather than
  `moonlight list`: structured XML beats CLI text, and it is what turns a
  running app's id into the name a card shows.
- **`sunshine/api.rs`** drives Sunshine's config API. Unlike GameStream,
  nothing depends on this staying still, so every call degrades rather than
  assumes. Two measured quirks: auth failures are a real `401` with a JSON
  body, and `POST /api/pin` validates `Content-Type` **before** auth — send
  it without `application/json` and you get `400 Content type mismatch`
  regardless of credentials, which is easy to misread as an unauthenticated
  endpoint.
- **`src/sunshineSchema.ts`** curates the settings worth a considered
  control and lets everything else render generically from whatever the API
  returns. That is what makes full coverage affordable without mirroring a
  hundred fields by hand — and a setting added by a future Sunshine appears
  on its own instead of silently going missing. The copy is Dusk's own:
  Sunshine is GPL-3.0 and its help text is creative work, while key names
  and types are interface facts.
- **`sunshine/credentials.rs`** keeps the web-UI password in the OS keystore.
  Where no keystore exists it holds the password for the run and says so,
  rather than silently downgrading to plaintext on disk.
- **`host/`** is the platform abstraction. Capability matrices are real and
  drive the UI; `probe`/`start`/`stop` land in M3.
- The backend pushes a whole `Snapshot` on every change and the UI is a pure
  function of it. Cheap at this scale, and diffing would be premature.

## The look

The UI is drawn in [`@axiapps/axi-design`](https://darkharasho.github.io/axi-design/),
bundled rather than linked from its Pages URL — a desktop window opened offline
still has to paint. The accent is left at the language's default, Axi Gold,
which is also the right ink here: the strip on a card mid-session is the sun in
sunshine/moonlight.

`src/app.css` holds only what the language does not draw, and should stay that
way. Anything in it that grows into a component belongs upstream instead.

The one thing worth knowing is how device state is encoded, because the
language is opinionated about it. Status is a **cap across the head of a card**,
never a stripe down its edge: a full-height stripe reads as the card's border,
and a grid of them becomes a grid of coloured frames that say nothing about any
one machine. Offline gets no cap and a neutral chip — a muted status ink is
forbidden, so the absence of status is drawn as the absence of colour rather
than as a faded version of it.

| State | Cap | Chip |
| --- | --- | --- |
| Hosting | accent | `axi-chip--accent` |
| Ready / Online | ok | `axi-chip--ok` |
| Not paired | warn | `axi-chip--warn` |
| Offline | none | plain `axi-chip` |

## First-run setup

Setup is a checklist, not a wizard, because Sunshine is usually already
installed and the outstanding item is often just one step.

Installing means three different things: mount the DMG and `ditto` the
bundle into Applications, hand the MSI to `msiexec` under `RunAs`, or drop
the AppImage into `~/.local/bin` and make it executable. On macOS, if
Applications is not writable the copy is retried through `osascript` so the
system's own authorisation prompt appears rather than Dusk asking for a
password itself.

**Dusk will not install a virtual display driver.** That is a kernel-mode
driver from a different project, outside the Sunshine release Dusk can
verify, and installing one silently would mean putting unverified code in
the kernel on someone's behalf. The step stays on the checklist and explains
what to do; it just is not automated. A test enforces this.

Everything on Windows that needs administrator rights — the installer, the
firewall rules, and starting or stopping the service — goes through one
elevation helper in `host/service.rs`. Three details there are load-bearing
and each one fails silently if missed: `-PassThru` with an explicit `exit`,
or PowerShell's success at *starting* the process is reported instead of
the result; `sc.exe` rather than `sc`, which in PowerShell is an alias for
`Set-Content`; and a much longer timeout, because the ordinary one would
cut off a UAC prompt before the command it guards had begun. The script is
built by a function that is unit-tested from any platform, since that is
the only way it gets checked before reaching Windows.

None of the Windows paths have run on a real machine. The Linux ones have —
see below.

## Things worth knowing

**Dusk adopts Moonlight's client identity rather than imposing one.** The plan
was for Dusk to generate a keypair and point moonlight-qt at a private profile.
The spike killed it: moonlight-qt has no `--config` flag and its QSettings
backend is CFPreferences on macOS and the registry on Windows, neither
redirectable by environment variable. But the identity turns out to be plain
PEM under two keys, so Dusk reads it and never writes. Ownership flips at the
moonlight-common-c swap, and because Dusk already holds the keypair that swap
costs nobody a re-pair.

**A rejected certificate is an answer, not a failure.** Asked for `serverinfo`
over TLS by an unpaired client, Sunshine replies `HTTP 200` with
`status_code="401"` in the body. Reading only the HTTP status would drop that
on the floor and fall back to the plain probe, which can never say more than
"unknown" — so the parser reads the body's status and treats a rejected
certificate as a definite "not paired". Pairing still reads as unknown where
no identity exists yet, which is an honest first-run state.

**macOS hosting is experimental upstream.** Not a Dusk limitation — Sunshine
itself treats macOS hosting as experimental: no gamepad support, no system audio
without a loopback device like BlackHole, and Screen Recording plus Accessibility
have to be granted by hand because no installer can script a TCC prompt. The UI
flags this rather than promising parity. Streaming *to* a Mac is unaffected and
first-class.

**moonlight-common-c is GPL-3.0, not LGPL.** The plan recorded here — embed it
later and keep the shell unencumbered — cannot be done: linking it relicenses
Dusk. Nor would it save much work, because it is the streaming protocol only.
`Limelight.h` has no pairing function at all; pairing lives in moonlight-qt's
`NvPairingManager`, along with the decoders, audio and input.

So the client stays a **separate process**, and the in-stream overlay lives in
a GPL-3.0 fork of moonlight-qt rather than in Dusk. Arm's-length IPC — CLI
arguments today, a local socket once the overlay reports back — is aggregation
rather than a derivative work, which is what keeps the boundary real. The line
to hold is that they stay separate programs: pipes and sockets are fine,
linking and shared in-memory structures are not.

Most of that overlay is already built upstream. `overlaymanager.{h,cpp}` keeps
one `SDL_Surface` per overlay type and **every renderer already composites
them** — `vt_metal` and `vt_avsamplelayer` on macOS, `d3d11va` and `dxva2` on
Windows, `vaapi`/`drm`/`eglvid`/Vulkan on Linux, plus the SDL fallback, with
dedicated overlay shaders. The surface is generic RGBA; text is only what the
manager happens to draw today. An interactive overlay is a new overlay type, a
centred rect per renderer, input interception while it is open, and a hotkey
beside the existing combos in `input/keyboard.cpp`.

**Sunshine is not bundled.** It gets fetched and verified at first run (M5).
That keeps Dusk out of GPL-3.0 conveying obligations, lets Sunshine ship security
updates without a Dusk release, and defers to the distro package on Linux.

**Two separate things broke TLS to a host, and one hid the other.** The
symptom was the same for both — an app list that failed most ticks and a
pairing state flickering between Paired and Unknown — and both logged only
`error sending request`, because `reqwest::Error`'s `Display` stops at the
outer layer. `serverinfo::describe` now walks the source chain, which is
what made the second cause visible at all.

*Connection pooling.* Sunshine answers without `Connection: close` and then
closes the socket anyway, on the plain port and the TLS port alike — curl
reports `left intact` followed immediately by `Connection 0 seems to be
dead`. curl reconnects; hyper's pool hands out the dead socket and whether
the next request notices in time is a race. Pooling buys nothing against a
server that closes every connection, so `pool_max_idle_per_host(0)` on both.

*TLS session resumption.* Sunshine aborts a resumed handshake with
`received fatal alert: InternalError`. One client reusing its rustls session
cache goes: first request fine, next two dead, one fine again as rustls
gives up on the ticket — eight requests through eight fresh clients succeed
eight times, so the server is healthy and the cache is the whole problem.
Capping to TLS 1.2 makes it *worse*, so it is not a 1.3-ticket quirk.
reqwest exposes no resumption knob, which is the only reason `state.rs`
assembles a `rustls::ClientConfig` by hand instead of using the builder.
`applist`'s ignored `live_probe` test is the guard: it needs a real paired
host, and it only fails from the second request onward, which is exactly why
a single-request test would have passed throughout.

**The keystore is read once per run, not once per snapshot.** `load` is
called from `AppState::snapshot`, which is built on every poll tick — so
reading through to the Keychain each time put an authorisation prompt on
screen every few seconds. It also never settles in development, because the
Keychain grants access to a *binary* and an unsigned one has a new identity
after every `cargo build`, which revokes "Always Allow" on each rebuild. The
answer is cached for the run and writes go through `save`/`forget`, so the
cache cannot drift. Expect one prompt per rebuild in dev regardless; that one
only goes away with a signed build.

**A missing capability file made the grid look frozen, not broken.** Tauri v2
gates `listen` behind `core:event`, and with no `capabilities/` directory at
all nothing is granted — so `onSnapshot` was rejected, every pushed snapshot
was dropped, and the UI showed its first `get_snapshot` forever. Machines
therefore sat on "Checking" while the backend knew perfectly well they were
online. Nothing logs when this happens: app-defined commands are not gated,
so `invoke` keeps working and only the push path dies. `capabilities/default.json`
grants `core:default` to the main window.

**`src/types.ts` mirrors `src-tauri/src/model.rs` by hand.** If it starts
drifting, generate it (ts-rs or specta) rather than patching it up.

**Three things only a real Linux machine could have found.** All three were
written against a Mac, all three compiled and passed CI, and each one failed
silently and completely the first time Dusk ran on a KDE Wayland desktop with
Flatpak installs — which is to say, on a very ordinary Linux gaming machine.

*Flatpak is where Linux keeps its config.* A Flatpak Moonlight writes its
settings inside the sandbox, at
`~/.var/app/com.moonlight_stream.Moonlight/config/...`, not `~/.config`.
Looking only at the native path found nothing, and that one file holds both
the client identity and the remembered-host list — so a single wrong path
lost TLS probing and an entire discovery source at once, with no error
anywhere. Both paths are searched now, native first, taking the first store
that *answers* rather than the first that exists.

*QSettings quotes a value when its content requires it.* The certificate's
base64 padding puts an `=` in it, so that key is written quoted and `key`
beside it is not — in the same file. The quotes survived the `@ByteArray(...)`
unwrap and the PEM check then failed, which reads as "moonlight-qt has never
paired" rather than as a parse error. That is the honest first-run state, so
nothing looked wrong.

*A Flatpak Sunshine's unit is not called `sunshine`.* It ships one named
after its application id, `app-dev.lizardbyte.app.Sunshine`. Probing only for
`sunshine` reported a perfectly good Sunshine as not installed — and on an
immutable distribution the Flatpak is the only Sunshine there can be.

**WebKit's DMA-BUF renderer cannot draw on a compositor with explicit sync.**
KWin has it, so on every KDE Wayland session — Bazzite and the Steam Deck
among them, which a game-streaming app cannot afford to miss — the window
never appeared. GTK printed one `Error 71 (Protocol error)` line and exited,
which says nothing about the cause. `WAYLAND_DEBUG=1` gave the real one:
`wp_linux_drm_syncobj_surface_v1: explicit sync is used, but no acquire point
is set`. It is a webkit2gtk bug, so the only lever on this side is to not
take that path: `run` sets `WEBKIT_DISABLE_DMABUF_RENDERER` before GTK
starts. Scoped to Wayland, since the DMA-BUF renderer is the faster one and
is sound under X11, and skipped when already set.

### Building on Linux

The system GTK and WebKit development packages are what Tauri needs
(`webkit2gtk-4.1`, `gtk+-3.0`). A Homebrew-on-Linux earlier on `PATH` than
`/usr/bin` shadows the system `pkg-config`, and Homebrew's tree carries no
X11 or `javascriptcoregtk` `.pc` files — so the build fails claiming `cairo`
"was not found" while cairo is plainly installed, which sends you hunting a
package that isn't missing.

Nobody should have to know that to run `npm run dev`, so the npm scripts go
through `scripts/tauri.mjs`, which probes the `pkg-config` on `PATH` and falls
back to `/usr/bin/pkg-config` only when the first cannot resolve `gdk-3.0` and
`javascriptcoregtk-4.1` and the second can. A probe rather than an assumption:
a healthy machine is untouched, an explicit `PKG_CONFIG` is left alone, and
nothing runs off Linux. Invoking `cargo` directly bypasses it — that is what
`PKG_CONFIG=/usr/bin/pkg-config cargo test` is for.
