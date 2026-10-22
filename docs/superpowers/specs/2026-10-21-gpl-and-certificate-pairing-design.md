# Dusk goes GPL-3.0, and your own machines pair by certificate

Date: 2026-10-21. Status: design, not built.

## Why

Most of the breakage in Dusk's history has been in streaming's surroundings
rather than streaming itself: virtual displays, installers, identity. Those are
the product. One seam keeps breaking, though, and it is pairing. v0.3.16,
v0.3.17 and v0.3.26 each shipped to fix it. Every time the cause was the same
shape: Dusk automates Moonlight's PIN handshake across two programs and two
machines. The client invents a PIN, the peer channel carries it, and the host
submits it to Sunshine's web API. That is three timeouts that have to agree
and an API built for Sunshine's own web page, whose pairing endpoint changed
under Dusk in 2026.

For two machines on the same GitHub account the PIN proves nothing. Their
account keys already establish that they belong together. The PIN is only
there because Moonlight's handshake needs one. Pairing *is* each side
holding the other's certificate: Sunshine keeps paired clients in its state
file, and moonlight-qt keeps each host's server certificate in its settings.
Dusk can put them there directly.

The architecture kept everything at arm's length because Dusk was not GPL.
The owner has decided Dusk will be GPL-3.0. Process separation stays, but as
an engineering choice rather than a legal one: a crashed stream must not take
the window with it, and video does not belong in a webview.

## Goals

- Dusk is licensed GPL-3.0, and its docs say so instead of describing a
  boundary that no longer applies.
- Two machines on the same account pair by exchanging certificates, with no
  PIN, no Sunshine pairing API and no Moonlight pairing handshake.
- A pairing never interrupts a stream that is running on the host.
- Account pairings survive Sunshine rewriting its state file, and survive
  Sunshine being reinstalled.

## Non-goals

- **Replacing the moonlight-qt fork with a client of Dusk's own.** It was
  considered and is out of scope. The fork stays.
- **Forking Sunshine now.** Dusk already pins and bundles one tested Sunshine
  (`SUNSHINE_TAG` in `.github/workflows/release.yml`), so a fork would buy
  little until something is found that cannot be done from outside. See
  "Later" below.
- **Changing pairing for machines that are not on your account.** PIN pairing
  plus the "Let other devices pair" Allow prompt stays exactly as it is.
- **Any change to streaming, discovery or screen sizing.**

## Anti-goals: failures even if it technically works

- Restarting Sunshine while someone is streaming from it.
- Dusk editing or removing a `named_devices` entry it did not add. The
  person's iPhone, paired the ordinary way, is not Dusk's to touch.
- A machine that is not on the account getting paired without the Allow
  prompt. Certificate pairing must be at least as strict as today's signed
  PIN.
- A machine that is on the account silently ending up half paired: the host
  trusting the client but the client not trusting the host, or the reverse,
  with the card claiming it is paired.

## Part 1: the licence

- Add `LICENSE` at the repo root with the GPL-3.0 text (the same text as
  `src-tauri/licenses/GPL-3.0.txt`), and set `"license": "GPL-3.0-or-later"`
  in `package.json` and `license = "GPL-3.0-or-later"` in
  `src-tauri/Cargo.toml`. Choosing "-or-later" over "-only" is the designer's
  call, matching Sunshine and moonlight-qt. Flag it to the owner.
- Rewrite the README section that begins "**moonlight-common-c is GPL-3.0,
  not LGPL.**". Its facts still hold: Limelight.h has no pairing, and the
  client is a separate process. But the reason changes from licence to
  robustness, and the line "pipes and sockets are fine, linking and shared
  in-memory structures are not" goes.
- Update `docs/architecture.md`'s "The constraint everything else bends
  around" the same way. Keep the three-process shape, and give the
  engineering reasons for it.
- `src-tauri/licenses/README.md` stays largely as is, since the two programs
  are still shipped and still GPL. Drop the sentence implying Dusk itself is
  under a different licence, if any wording there implies it.

## Part 2: certificate pairing

### The two halves of a pairing

| Side | What it must hold | Where it lives |
| --- | --- | --- |
| Host (Sunshine) | the client's Moonlight certificate | `sunshine_state.json` → `root.named_devices[]`: `{name, cert, uuid, enabled}` |
| Client (moonlight-qt) | the host's Sunshine server certificate, against the host's `uniqueid` | moonlight-qt's settings, `hosts/N/…` |

Observed on this Linux host (`~/.config/sunshine/sunshine_state.json`): each
entry is `name`, `cert` (PEM), `uuid`, and `enabled` as the *string*
`"true"`. Sunshine's own top-level `uniqueid` sits under `root`. Dusk already
reads its own client certificate from moonlight-qt
(`src-tauri/src/moonlight/identity.rs`) and the host list
(`moonlight/hosts.rs`, read-only today).

### The exchange

It runs over the existing peer channel (`src-tauri/src/peer.rs`, port 47850,
newline-delimited JSON):

```text
connecting → host   {"type":"pair_cert","name":"Mac mini",
                     "cert":"-----BEGIN CERTIFICATE-----…",
                     "proof":{"account":…,"machine":"…","signature":"…"}}
host → connecting   {"type":"paired","server_cert":"-----BEGIN CERTIFICATE-----…",
                     "uniqueid":"4F87…","ready":true}
                    {"type":"paired", …, "ready":false}   recorded; Sunshine
                                                          restarts after the
                                                          current stream
                    {"type":"declined","reason":"…"}
```

- The `proof` signs the certificate, not a PIN, using the same account
  signing key and checked against the same machine list as today's `Pair`
  proof (`crate::account::Proof`). The signature must cover the certificate
  bytes and the host's identity, so that a captured message cannot be
  replayed to a different host. The exact signed payload is the builder's
  call.
- **The host's reply is signed too.** Today the PIN handshake is what proves
  to moonlight-qt that the server certificate is genuine. Without it, anything
  answering on 47850 could plant a host certificate in the client. So
  `paired` carries the host's own `proof`, signed with its account key over
  `server_cert`, `uniqueid` and the client's certificate. The client checks
  that the host is on the account's machine list before storing anything.
  *(Designer's call, required by the "at least as strict" anti-goal.)*
- `pair_cert` without a valid proof from a machine on this account is
  declined. There is no Allow-prompt path for certificate pairing; machines
  outside the account use the existing `Pair` flow.
- The `MAX_LINE` limit (1024 bytes) is too small for a PEM certificate. Raise
  it for this message, or generally, to something that fits a certificate
  with margin.
- **Only when both halves can finish.** The connecting side sends
  `pair_cert` only if the client Dusk will run is the Dusk fork with the new
  command-line action, detectable from its `--version` banner. Otherwise,
  for example a stock Flatpak moonlight-qt, it uses today's `Pair` flow, so
  the host never trusts a client that cannot trust it back. *(Designer's
  call.)*
- Mixed versions: a host too old to know `pair_cert` declines it as unknown,
  and the connecting side falls back to today's signed-PIN `Pair` flow. A new
  host keeps answering `Pair` for old clients. Nothing older breaks.

### On the host

1. Verify the proof. On success, record the certificate in **Dusk's own list
   of account pairings**: machine id, name, certificate and the uuid Dusk
   assigned. This list is the source of truth, kept in Dusk's store.
2. Make sure the state file contains every entry in that list. Add any that
   are missing and touch nothing else. Write it atomically (temp file and
   rename), preserving every field Dusk does not understand.
3. If no stream is running on this host, restart Sunshine through the
   existing `SunshineApi::restart` and reply `ready: true` once it answers
   again. If a stream is running, reply `ready: false` and restart as soon as
   the stream ends.
4. **Before every Sunshine start or restart that Dusk causes**, and when
   Dusk itself starts, re-apply the list to the state file. This covers
   Sunshine rewriting the file from memory, which it does when it pairs or
   unpairs anything else, and covers a reinstall.
5. **Revocation.** When a machine leaves the account's machine list, remove
   the entry Dusk added for it, from both Dusk's list and the state file, then
   restart Sunshine to enforce it, waiting for any running stream to end as
   with pairing. Signing this host out of the account removes all of Dusk's
   entries the same way. Only a *successful* fetch of the machine list counts:
   a failed or stale fetch keeps everything, so a GitHub outage never unpairs
   anyone. Only entries Dusk added are ever removed. The client side does not
   delete stored hosts, because a host that has revoked the client already
   refuses it. *(Designer's calls. Without revocation, a machine removed from
   the account would stay able to stream.)*

**Which Sunshine this covers.** The state-file path is used wherever Dusk can
find and parse the state file, whoever installed Sunshine. If Dusk cannot find
the file (a Flatpak sandbox, say), cannot write it, or finds entries in a
format it does not recognise, the host declines `pair_cert` with a reason that
makes the client fall back to the `Pair` flow. The spike's job is to cover the
Sunshine Dusk bundles; other installs get whatever the fallback gives them.
*(Designer's call.)*

Whether Sunshine reloads the state file without a restart is unknown, and the
spike settles it. If it does, the restart and the waiting go away.

### On the client

Store the host's `server_cert` and `uniqueid` in moonlight-qt's settings as a
paired host. moonlight-qt owns that store and may be running, so Dusk does not
write it directly. **The moonlight-qt fork gains a command-line action** that
adds or updates a paired host: uuid, name, addresses, server certificate. Dusk
calls it the way it already launches the fork. The fork's maintainers own how
it writes safely while a GUI instance might be open. The minimum acceptable
behaviour is refusing with a clear error, which Dusk retries after the stream
or window closes.

**This fork change is part of this work.** It is built in the sibling
checkout (`~/Documents/GitHub/moonlight-qt`, branch `dusk-overlay`) and
released by the fork's own repository first. Then Dusk's `CLIENT_TAG` is
bumped to it. *(Designer's call.)*

**A host whose certificate changed.** Reinstalling Sunshine, or losing its
state, gives it a new server certificate. The host half survives, because
Dusk re-applies its list, but the client's stored certificate no longer
matches. So when a connection to an account machine fails because the host's
certificate does not match the stored one, Dusk silently re-runs `pair_cert`
and replaces the client-side entry. The card does not show paired until that
is done. *(Designer's call.)*

The card shows paired only when **both** halves are done. If the host says
`ready: false`, the card says the host is in use and will be ready when its
stream ends, then becomes ready on its own. That moment can be learned by
polling `serverinfo`, which already reports pair state.

### Failure handling

- The state file is unwritable, for example because Windows elevation was
  refused: decline with a reason the card can show, and record nothing.
- The client-side write fails: the host half is already done. Retry the
  client half and show "finishing pairing", never "paired".
- The restart fails: keep the list, and the next start re-applies it. Surface
  the error as today's host errors are surfaced.

## Part 3: the spike (first, before Part 2 is built)

Throwaway. Its job is to prove or disprove the state-file approach on each
platform. Report:

1. **Linux** (this machine; the state file is under `~/.config/sunshine`, and
   Sunshine runs as the user): add a client's certificate to
   `named_devices` by hand, restart, and confirm the client streams with no
   pairing. Confirm whether it works without a restart.
2. **Windows**: find the state file's real location for the bundled Sunshine,
   expected under its install folder's `config\`, and the privileges needed to
   write it. Confirm the same format, and that the service restart through
   Dusk picks it up.
3. **macOS** (Dusk Host.app): the same questions.
4. **Client side**: hand-write a host entry with its server certificate into
   moonlight-qt's settings on one platform, and confirm moonlight-qt treats it
   as paired and connects. Note anything else moonlight-qt needs, such as the
   MAC address or the HTTPS port.
5. Whether Sunshine's `file_state` setting can move the file, and whether
   Dusk must read it from the config.

If any of the above cannot be done from outside Sunshine, write down exactly
what and stop. That finding is the trigger for "Later".

## Later, only if the spike says so

Fork Sunshine under `darkharasho`, built and released by its own repository
the way the moonlight-qt fork is (`CLIENT_REPO`/`CLIENT_TAG` in
`release.yml`), and add the smallest endpoint that closes the gap. For
example, adding a named device live. That is a separate design.

## What is left to the builder

- The exact signed payload for `pair_cert`, and where Dusk's account-pairings
  list sits in the store.
- How "a stream is running on this host" is detected. Dusk already steps
  aside during streams and tracks sessions, so reuse whatever it has.
- The name of the fork's new command-line action, and its arguments.
- Tests: follow the repo's existing style, with the state-file merge as a
  pure function tested against real Sunshine-written JSON (unknown fields
  preserved, foreign entries untouched, duplicates not added) and the
  protocol messages round-tripped.
- Updating `docs/pickup.md` and the `peer.rs` module docs to describe the
  new flow.
