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
| M2 | Client path — pair, list apps, launch via `moonlight-qt` | built, not yet tested against a second machine |
| M3 | Host path — Sunshine service control on all three platforms | built; probe verified on macOS |
| M4 | Config UI — schema-driven, replaces Sunshine's web UI | built; needs a signed-in host to exercise |
| M5 | First-run install per platform | built; download path verified against real releases |

## Running it

```sh
npm install
npm run tauri dev            # real discovery on the local network
DUSK_MOCK=1 npm run tauri dev # fixtures, no network, for UI work
```

`DUSK_MOCK=1` seeds one device in each card state and swaps in a mock host
backend. It exists because a Mac cannot usefully run Sunshine (see below), and
the rest of the UI should not be blocked on that.

```sh
npm run typecheck                 # frontend
cd src-tauri && cargo test        # backend
python3 scripts/make_placeholder_icon.py   # regenerate the placeholder icon
```

## How it fits together

```
mDNS (_nvstream._tcp) ─┐
                       ├─→ Registry ─→ poller ─→ serverinfo ─→ Snapshot ─→ UI
manual address book ───┘
```

- **`registry.rs`** is the interesting file. Two sources feed it and the same
  machine routinely appears in both, or twice over mDNS on two interfaces.
  Devices are keyed by address until `serverinfo` returns Sunshine's `uniqueid`,
  at which point the entry is re-keyed and merged. That is what makes "one
  machine on LAN and VPN" a single card.
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

None of the Windows or Linux paths have run on a real machine.

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

**Sunshine is not bundled.** It gets fetched and verified at first run (M5).
That keeps Dusk out of GPL-3.0 conveying obligations, lets Sunshine ship security
updates without a Dusk release, and defers to the distro package on Linux.

**`src/types.ts` mirrors `src-tauri/src/model.rs` by hand.** If it starts
drifting, generate it (ts-rs or specta) rather than patching it up.
