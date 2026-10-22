# Picking this up elsewhere

Written to move work from the Mac it was started on to the Bazzite machine.
`docs/architecture.md` is the plan; this is where the plan actually stands,
what to set up, and what bit hard enough to be worth not rediscovering.

## Where things stand

**v0.3.30 is released**, all three platforms, from `main` (PR #3), still
carrying the client's `dusk-v0.2.5`. It fixes certificate pairing from Linux:
every `add-host` there failed with "--name not provided" (see the client half
below). **v0.3.29** (PR #2) made **Dusk GPL-3.0-or-later, and your own machines
pair without a PIN.** Design:
`docs/superpowers/specs/2026-10-21-gpl-and-certificate-pairing-design.md`;
the measurements it rests on:
`docs/superpowers/specs/2026-10-21-certificate-pairing-spike.md`.

*How pairing works now.* Machines on the same GitHub account try
**certificates first**; everything else, and any certificate attempt that does
not finish, uses the PIN as before. The two machines swap certificates over
the peer channel (`peer.rs`, signed for the receiving host), and each writes
its own half:

- **The host half** (`cert_pairing/{host,reconcile}.rs`) adds the client's
  certificate to `named_devices` in Sunshine's `sunshine_state.json` (default
  `~/.config/sunshine/sunshine_state.json`; `file_state` and `cert` in the
  config API override it, absent means the default) and **restarts the host**,
  because it reads the file only at startup. It **never restarts mid-stream**;
  the restart is owed and kept in Dusk's settings until the stream ends.
  "Mid-stream" is serverinfo busy **and** a client still connected by the
  host's log (`/api/logs`, `reconcile::client_connected`): the host stays busy
  while the app a stream launched runs, which outlasts a client that dropped
  instead of quitting. Until v0.3.31 that held every certificate pairing to
  such a host forever. And it
  **re-merges from disk immediately before every restart**, because the host
  rewrites the file from memory whenever it PIN-pairs anyone and drops entries
  it never loaded (the spike reproduced this). A pairing leaves the settings
  only after the host has loaded the file without it.
- **A restart counts only once it is seen** (`reconcile.rs`, `came_back`). The
  restart request counts only on a 2xx or a connection that dies after it was
  sent. Then serverinfo is asked every 250 ms, for up to 20 s, and the host has
  to stop answering at least once and then answer again. Until both happen the
  restart stays owed, removals stay in the settings, and the reply says
  `ready: false`. The next pass asks again. After 3 unconfirmed restarts in a
  row, Dusk waits between tries: 60 s at first, doubling each time, up to
  1 h. The count lives in memory only and resets when a restart is confirmed
  or the file gets something new. The back-off only limits how often the host
  is asked; the restart stays owed and removals stay kept. An answer before
  the host has gone down is the old process and counts for nothing.
- **When the host's settings cannot be read**, Dusk never falls back to the
  default paths. A `pair_cert` request is declined, so the other machine uses
  the PIN; the defaults could reply with a certificate the host does not use.
  A background pass writes nothing, forgets nothing, and leaves any restart
  owed; the defaults could change a file the host does not read.
- **Retired certificates.** When a machine's certificate is replaced, the old
  one's sha256 (of the normalised PEM) is kept per machine in Dusk's settings
  (`retired_certs`). A request to pair it again is an old one played back. It is
  declined in the same words as any request that does not check out.
- **The client half** (`cert_pairing/client.rs`) shells out to the fork's
  `add-host` action, which records the host's certificate and address. It takes
  a lock file and **exits 2 while another client is open**, so Dusk waits
  (the drawer says so). `--version` prints `Dusk fork: add-host available`;
  a client without that line is never offered the new path.
  **Every value goes as `--option=value`** (`moonlight/cli.rs`,
  `add_host_args`). On Linux, Qt's X11 plugin takes `--name <value>` as its
  own option and strips both words from argv before the client parses it;
  the joined form passes through. Until v0.3.30 this failed every Linux
  pairing with "This machine couldn't save the pairing", after the host had
  already trusted the client.
- **Re-pairing.** When a host's certificate changes, the stored one no longer
  matches what it presents, so the card drops to not paired and the next Pair
  runs again; the host side gives the changed certificate a fresh uuid, since
  the host matches by uuid.
- **No half pairings on a card** (`poller.rs`, `shown_pairing`). A host that
  trusts us while the client has no certificate for it (or a different one)
  shows not paired, since a stream would fail. Only two answers demote it: the
  client holds no certificate for the host, or holds one that differs from the
  one the host presents. Paired **stands** in every other case:
  - the client's host list could not be read;
  - the list is empty;
  - the host has no `uniqueid` yet;
  - the host presented no certificate to compare;
  - the client's stored PEM does not decode.

  An empty or unreadable list is never evidence.
- Probing pairing over HTTPS must send a non-empty `uniqueid` (Dusk sends
  `uniqueid=0`), or even a trusted certificate reads `PairStatus 0`.
- **Membership is unknown, never empty, when in doubt.** It is unknown if the
  keystore is locked, the stored sign-in will not load, no list has been
  fetched, or the list does not name this machine. Unknown keeps every
  pairing.

*Fork and release.* The `add-host` action (fork commit `4a59931`) is merged
into the fork's `master` (darkharasho/moonlight-qt#1, merge `ded3c2f1`) and
released as `dusk-v0.2.5`. `CLIENT_TAG` in `.github/workflows/release.yml`
pins `dusk-v0.2.5`, so v0.3.29 is the first packaged Dusk that carries
`add-host` and pairs your own machines by certificate.

*Owner-deferred checks* (they need machines this was not built on):

- **Windows and macOS spike.** The state file's real path, and whether a
  non-elevated user can write it. If Windows cannot, Windows hosts keep the PIN.
- **Step 1 against the pinned `v2026.914.233613`.** The spike ran on
  `2026.516.143833`; write a certificate in by hand, restart, expect a stream
  with no prompt.
- **Manual end to end:** two account machines pairing; a host that is
  streaming to a third device while one pairs (it should wait, then finish); a
  stock client giving the PIN fallback; and a host certificate rotation (the
  card drops, re-pair works). Rotation would unpair the real PIN pairings on
  the host, so it was not run.
- **Per platform, a live check that the client's host-list read finds every
  host.** A partial read would show hosts as not paired.

*Known follow-ups.*

- A fresh client with **no identity** still needs a PIN for its first pairing.
  Suggested fix: a fork flag such as `--ensure-identity` that Dusk calls when
  `identity::load` returns nothing and `supports_add_host()` is true.
- `add-host` can report success on a failed flush: it checks a fresh
  `QSettings`, not the flush thread's write. A read-back verify before the
  release would close it.
- Closing the drawer does not cancel a pairing in progress; a second Pair
  starts a second loop (harmless, the host is idempotent).
- The Windows `config_dir` (`%ProgramFiles%\Sunshine\config`) is a guess until
  the Windows spike. Wrong means PIN fallback, not breakage.

**v0.3.28**, carrying the client's `dusk-v0.2.4`, lets a **`.deb` install update itself**, which was
the one format that could not. The plugin handles the other four and cannot
handle this one — the files belong to dpkg, and writing over them leaves the
package database describing a version that is no longer there — so
`updates/deb.rs` reads the same manifest, checks the same minisign
signature, and hands the file to `apt-get install` behind one `pkexec`
prompt. Three things to know before touching it. The manifest's **`deb` key
sits beside `platforms`, not in it**, and that placement is load-bearing:
the plugin reads `platforms`, so a `.deb` listed there would be handed to it
and written over dpkg's files as though it were an AppImage;
`linux-x86_64` stays the AppImage. The **key and endpoint come from
`tauri.conf.json` through `build.rs`** rather than a second copy, with a
test pinning the baked values to the configured ones, because a drifted key
rejects every update and only on the format nobody develops on. And the
verification test that matters is that a **genuine signature is accepted**:
a verifier that refuses everything passes every "rejects a bad signature"
test there is while leaving every Debian install unable to update. The
fixture is a real `tauri signer sign` signature from a throwaway key. Note
that **editing a minisign key's comment does not make a different key** —
the material is on the second line — which made the wrong-key case pass for
the wrong reason until it was given a separately generated keypair.
Bootstrapping is the same as the updater's own: a copy must already carry
this to use it, so v0.3.28 is the **last manual `.deb` install**.

**v0.3.27** gives a **Linux host** the screen-sizing that was
Windows-only, and fixes a grid bug that had nothing to do with it.

The Linux mechanism is `host/screen_mode.rs` and it is not the Windows one.
There is no virtual display to stream: the driver is user-mode IddCx, a
Windows interface, `amdgpu` offers no `VIRTUAL` output, and **Sunshine's
display-device settings are Windows-only** — a Linux host's config has no
`dd_*` key at all, measured on `2026.914.233613`. So the output the host is
really using is changed around the stream, through Sunshine's own
`global_prep_cmd`, with **Dusk registered as its own prep command** and
re-run as `dusk --dusk-match-screen do|undo`. Those hooks are the only thing
that brackets a stream reliably; the host's Dusk has no equivalent.

Four things here were got wrong first and are worth not repeating. **(1)
Only using modes the monitor lists is useless.** That was the first cut, and
it did nothing whatsoever for the two machines it was built for: this Mac is
3440x1440 and Venus's monitor lists no ultrawide mode, so the request
snapped to the nearest 16:9 size, which was the size it already was. A
monitor only lists the shapes it *is*, and "match the device watching" is
mostly asked by a device of a different shape. The answer is `Plan::Scaled`:
keep the panel on the mode it already had and resize **only the
framebuffer**, which is what Sunshine captures. `--scale` with six decimals
lands exactly — measured on 3440x1440 and on an awkward 2000x1300 — and the
framebuffer is read back from the **`Screen 0:` line** and pinned with
`--fb` if it is ever a pixel out. Reading the *output's* mode back will not
do: it stays where it was and would report that nothing had happened.
`cvt`/`--newmode` is still refused, because driving the panel at a mode it
may not display leaves the host dark if `undo` never runs; scaling cannot do
that. **(2) Sunshine reads `sunshine.conf` when it starts, not when a stream
does.** Writing the prep command and stopping there gives a host that has
the setting and ignores it — a stream ran with the command in the file and
never invoked it. `enable`/`disable` restart the host, which is what the
Windows half always did and should have been the clue. **Do not try to
confirm that restart with `ps`:** `/api/restart` re-execs Sunshine, which
keeps both the PID *and* the start time, so the process looks untouched
while its configuration has genuinely been reloaded. Reading it as "no
restart happened" nearly got this fix reverted as unnecessary. What settles
it is the behaviour: with the identical config written and no restart, a
stream runs no prep command; after a restart, the same config resizes the
screen. **(3) `undo` goes
with the session, not the client.** A client that is killed rather than
asked to quit leaves the session up and the screen resized; Sunshine logs
`CLIENT DISCONNECTED` and runs nothing. Hence
`screen_mode::restore_if_left_resized`, called at startup. **(4) The mode in
use is not the preferred mode.** Venus sits at 2560x1440 while `+` marks
1920x1080, so `xrandr --auto` would "restore" it to the wrong size; the mode
in use is recorded before the switch.

`dusk screen on|off` exists for the same reason `dusk setup` does: a Linux
host could otherwise only turn this on from its own screen, which a host in
a cupboard has not got. It needs the settings directory **without Tauri to
ask**, so `store::config_dir` reproduces `app_config_dir` — and the window
compares the two at startup and says so if they differ, because the day they
disagree one writes a setting the other never reads. That is the failure
`secrets/mod.rs` argues about; `config_base` takes its environment values as
arguments so the choice between them is tested rather than assumed.

Separately, **a machine of your own could be filed as a stranger and stay
that way**. The tell was an inversion: the only machine in "Your machines"
was the one that was *switched off*. A machine is asked `identify` only
while it has no id (the poller filters on `machine_id.is_none()`), so the
answer arrives once and the `mine` computed from it sticks for the life of
the window; if the account's gist has not been read by then, nothing is
known to be yours. It keeps its id, is never asked again, and `sync_account`
skips it ever after because a found id needs no placeholder. An undiscovered
machine was right precisely because the placeholder path re-decides every
tick. `sync_account` now re-decides `mine` for every machine that has
answered, so the list is the authority and not the timing. It presents as
"fine all morning, wrong after an update restarted the window".

Useful while working on any of this: `peer::identify` and the `display`
answer can be asked straight from another machine, which tests the host
without a stream —
`printf '{"type":"display","screen":{"width":3440,"height":1440}}\n' | nc <host> 47850`.

**v0.3.26** fixes pairing against a 2026 Sunshine, which had
looked like every pairing layer at once and was none of them. **Sunshine
grew a second step in `/api/pin`**: a host from 2026 holds each pairing
request under an id, lists them at `GET /api/pin`, and **refuses a POST that
does not name one** — `400 pairing_id must contain exactly 32 hexadecimal
characters`, *whatever the PIN*. Measured on `2026.914.233613`; bazzite
paired throughout because its Sunshine is older. Dusk sent only the PIN and
the name, so the host spent the whole `PIN_WINDOW` posting a body that was
rejected before the PIN was looked at, then reported it as a PIN that was
not accepted. `submit_pin` now reads the list first and posts an id; a host
that does not list them gets the old body, so an older Sunshine is
untouched, and **an empty list is kept apart from "this host does not name
them"** — the first is retried (the ordinary case while a cold client is
still starting), the second is not. Two things measured and worth not
re-deriving: the id is minted **per request, not per client** (two attempts
gave two ids), so it must be read immediately before use and never
remembered; and a well-formed but unknown id gets `200 status:false`, not a
400. Choosing among several waiting requests uses the asking machine's
address as a **hint, not a filter** — Sunshine's view of the client's
address and the peer connection's source address are picked independently,
and this Mac has two — so matching nothing offers the PIN to all of them.
**Still unverified:** whether a rejected PIN cancels a pending request on
the host. If it does, that loop could disturb a third machine's concurrent
pairing.

How it was found, since the method generalises: every layer the handoff
suspected was already working, and the thing that localised it was
**reading the host's own answer rather than Dusk's report of it**. Venus's
log said `is on this account; pairing without asking`, so the account,
proof, sealed-address and peer paths were all fine; `GET /api/pin` then
showed the client's request *pending*, which meant the client side was fine
too and only the approval was failing. Reproducing it by hand — `GET
/api/pin`, then POST the same PIN with the id — returned `{"status":true}`
and moved `named_certs` from `[]` to the paired machine, which both proved
the contract and finished the pairing. Two further notes. Sunshine logs
nothing at all for a pin submission, so its log's **silence is not
evidence**, and the `SSL Verification error :: Client certificate identity
is not paired` flood is just Dusk's own certless 5s poll on 47984 (the same
non-fault v0.3.17 recorded, in its newer wording). To read a TPM-sealed
secret for a test, `systemd-creds decrypt` **needs `--name=`** or it refuses
on the filename it just read: `systemd-creds decrypt
--name=sunshine-local-api ~/.local/share/dusk/sealed/sunshine-local-api.cred -`.
v0.3.25 made an open window notice a sign-in that
had changed underneath it. v0.3.24 fixed two things found by running it. (1) Duplicate cards:
a placeholder only withdrew on a matching `machine_id`, which only arrives
from `identify`, which a Dusk older than v0.3.19 never answers — so every
not-yet-updated machine showed twice, account copy (no address, "Your
machines") *and* discovered copy ("Other machines"). `sync_account` now
claims the discovered card by **normalised name** when there is no id, and
the poller keeps asking so the weak key is replaced. (2) `from_stored`
minted a `BoxKey` when the stored sign-in had none and **never kept it**,
so such a machine published a different public key every launch and could
never open a box addressed to it — sealed addresses would have looked fine
and silently never arrived. It is now written back once, only for the
machine that had none. Field-ops notes from the same session: `identify` is
answered by the **app**, not the service, so a machine with Dusk not running
never answers; and **ufw blocks 47850** on Linux unless opened by hand
(`install/steps.rs` marks the firewall step non-automatable there), which
looks identical to Dusk being too old. v0.3.23 proved the identity fix. `AccountState` takes its keystore reader and writer
as fields (`Reader`/`Writer`, same shape as `credentials.rs`), and the
carry-or-mint decision lives in `identity_for`, which needs no network —
`finish` cannot be tested for this because it reaches GitHub before it ever
reads the keystore. `account::identity_tests` now proves the refusal
end-to-end, and was checked by reinstating the bug: the two tests naming it
fail, the other seven pass. **Any test here must inject a writer**, or it
deletes the developer's real sign-in — the mistake `credentials.rs` records
having made. v0.3.22 stopped a machine minting a new identity when the
keystore would not answer. It fixes machines minting new identities: observed
twice in one day, this Mac `8717645a`→`f8e28caa` and the Linux box
`d2335d8d`→`ecd36ea7`, each orphaning an entry (the duplicate `bazzite`).
`finish` chose carry-vs-mint from `account.load()`, and **`load` returns
`None` both for "nothing stored" and for "the keystore would not say"** —
the second being a locked keyring or a dismissed keychain prompt, i.e. most
of what went wrong today. `AccountState::stored()` keeps them apart and
`finish` now refuses on `Err` rather than minting; `load()` remains for
callers that legitimately degrade. Signing key and box key are carried or
minted **together** — one without the other is a machine that can prove who
it is and cannot be told anything. The same conflation is called out in
`credentials.rs` ("a read that fails is not an answer"), which got it right;
this is the place that did not. Note `account/mod.rs` has no injectable
reader the way `credentials.rs` does, so the decision is tested through the
pure `carries_its_key` and the Result-vs-Option distinction rather than
end-to-end. v0.3.21 sealed published addresses. It is step two of `docs/addresses.md`: published
addresses are sealed (`account/sealed.rs`). Each machine holds a second
X25519 keypair beside its Ed25519 signing one — Ed25519 does not encrypt,
converting it needs the private scalar `ring` will not expose, and **`ring`
cannot hold the receiving end at all** because its X25519 is ephemeral-only
and a recipient's key must outlive the message; hence `x25519-dalek` with
the `static_secrets` feature. Ephemeral-static ECDH per recipient, HKDF
(`dusk-addresses-v1`), ChaCha20-Poly1305 from `ring`. The payload is tiny so
it is sealed once per recipient rather than wrapping a content key. Both
keys live in the one keystore entry (`Stored::box_key`, defaulted so older
entries load and mint one); **both are carried across a re-sign-in as the
same user**, because a new box key makes every box already addressed to this
machine unreadable. `Entry::addresses` is now **read-only** — written in the
clear only by v0.3.20, which existed for hours. A machine with no published
`box_key` cannot receive and is skipped. Republished when the addresses
change *or* when the recipient count does, so a machine joining the account
gets a box without waiting for an address to move. v0.3.20 published them
in the clear, deliberately, to find out whether the reach was worth the
encryption work. It implements step one of `docs/addresses.md`:
machines publish `addresses`/`updated` into their `github::Entry`, and
`Registry::sync_account` gives an unfound machine those addresses so it
reaches `probe_targets` — without that the card is decorative. Read that
doc before extending this; the two decisions it argues are **encryption**
(step two, not done — a second X25519 key beside the signing one, because
Ed25519 signs rather than encrypts) and **staleness** (`updated` is a
timestamp, written only on change — a timer would be a heartbeat and a
heartbeat is a rendezvous service with extra steps). `reachable_addresses`
in `lib.rs` is deliberately *not* `local_addresses`: the latter answers "is
this me?" and includes loopback, which must never be published. Link-local
and ULA are dropped too, the latter being what the client already discards
(`Ignoring ULA:`). LAN and overlay addresses are not told apart on purpose.
v0.3.19 made the account's list seed the registry. The account's machine list now seeds the registry
(`Registry::sync_account`, called from the poll tick), so the grid is your
machines rather than what is advertising: a machine that is off keeps its
card with `Reachability::Unknown` — deliberately *not* `Offline`, which is
a probe's verdict and nothing was probed. Placeholders are keyed
`account:<machine_id>` and `registry::account_tests` pins the three ways
this goes wrong: this machine listed twice (skip `own`), one machine as two
cards (placeholder must withdraw once discovery finds it and it identifies),
and a machine signed out elsewhere lingering forever (dropped when it leaves
the list). An **empty list means "not read yet", never "you own nothing"** —
the poller returns early so the grid stays as discovery found it. v0.3.18
grouped the grid by `mine`. The grid is grouped by account membership: `mine`
first under "Your machines", everything else under "Other machines on this
network". The join is `Device::machine_id`, answered by `peer::identify`
(`Message::Identify`/`Identity`) on the poll tick and checked against
`AccountState::knows_machine`. **It has to be the id, not the name** — this
Mac is `Michaels-Mini` on the account and `Michaels-Mac-mini.local.` over
mDNS. Three distinct answers, and the distinction is load-bearing: an id
(check the list), no id (signed out — a settled "not yours"), or a decline
(too old/not a Dusk — cannot be claimed). `App.tsx` falls back to one
ungrouped list when nothing is identified, so a signed-out Dusk behaves as
it always did. **The other half is not done**: a machine on your account
that is not advertising is still absent from the grid, because the registry
is still seeded only by discovery. Seeding it from the account's machine
list via `Registry::insert_prebuilt` is the next piece, and it is what makes
a host that is off still appear as yours. v0.3.17 derived `peer::PIN_WINDOW`
from the client's own timeout. `peer::PIN_WINDOW` is now derived —
`moonlight::cli::PAIR_TIMEOUT - 10s`, so 110s — rather than a number of its
own. v0.3.16 had raised it from 5s to 30s and that was still short: measured
on a real pair of machines, a *cold* client took about three minutes to
reach the point of pairing and only the **second** attempt ever worked,
because by then it was warm. A cold moonlight-qt sends ~50 WoL packets
across every interface it can see (ZeroTier, Tailscale, two LAN addresses,
IPv6 multicast), browses mDNS, fetches `serverinfo`, and only then pairs.
Any independently chosen host timeout can drift back under the client's, so
it is computed from it. Diagnostic notes from that hunt, all of which cost
time: `serverinfo` on **47984 returns RemoteHostClosedError for an unpaired
client and that is normal** — 47984 wants a client certificate, and
Sunshine logs `SSL Verification error :: ok` for every certless probe; do
not read either as a fault. The ~108 `CLOSE_WAIT` sockets on 47984 are a
real leak from Dusk's 5s poll but are a *consequence*, not a cause.
v0.3.16 fixed Dusk-to-Dusk pairing, which failed with
"That PIN was not accepted" on a correctly configured pair of machines. It fixes Dusk-to-Dusk pairing, which failed with
"That PIN was not accepted" on a correctly configured pair of machines.
`peer::enter_pin` retried five times at one-second intervals; the client it
was waiting for is a Qt app that `moonlight::cli::PAIR_TIMEOUT` allows 120s
to start and send its request. The host declared failure at five seconds,
and since `Outcome::Declined` makes `pair_device` return, that dropped the
pairing future and killed the client — so Sunshine never saw a pair request
and the symptom read as a bad PIN. `PIN_WINDOW` is now 30s with
`peer::budget_tests` asserting it stays under `PAIR_TIMEOUT`; the invariant
is that the *client* decides when pairing has failed. Sunshine returns the
same `status: false` for "nobody waiting yet" and "wrong PIN", so there is
nothing to distinguish them by and a wrong PIN now takes 30s to report.
How it was found, since the same method will work again: watch `netstat`
for the peer port during an attempt — a completed connection on 47850
proves the proof path ran — and check the host's `sunshine.log` for pairing
activity, whose *absence* localises the fault to the client side. Also
noted on the Mac: ~108 leaked `CLOSE_WAIT` sockets on 47984 from Dusk's
5s poll, not yet investigated. v0.3.15 is where sign-in expiry is handled. **Read this before touching the account code.** The
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
<https://github.com/darkharasho/dusk/releases/tag/v0.3.30>

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
├── dusk/            main             tags v0.1.0, v0.2.0, v0.3.0, v0.3.1, v0.3.2, v0.3.3, v0.3.4, v0.3.5 (never published: failed its formatting check), v0.3.6, v0.3.7, v0.3.8, v0.3.9, v0.3.10, v0.3.11, v0.3.12, v0.3.13, v0.3.14, v0.3.15, v0.3.16, v0.3.17, v0.3.18, v0.3.19, v0.3.20, v0.3.21, v0.3.22, v0.3.23, v0.3.24, v0.3.25, v0.3.26, v0.3.27, v0.3.28, v0.3.29, v0.3.30
└── moonlight-qt/    master @ ded3c2f1  tags dusk-v0.1.0 … dusk-v0.2.5
```

The fork builds and publishes a client; Dusk downloads a pinned one at
release time. That pin is one line — `CLIENT_TAG` in
`.github/workflows/release.yml`, currently `dusk-v0.2.5`. Bumping the client
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

**`security find-generic-password -w` answers in hex** when the value holds a
newline, with nothing to say it has done so. Decode it before building an
auth header from it, or the 401 that follows is a phantom of your own
making. On Linux the equivalent is that `systemd-creds decrypt` refuses
unless `--name=` matches the embedded credential name.

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
