# Certificate pairing spike: what the state file actually does

Run on 2026-10-21 against the Linux host `bazzite`, Sunshine
**2026.516.143833** from Homebrew under `systemctl --user`. That is older
than the pinned bundle (`v2026.914.233613`). The state-file behaviour below is
unlikely to differ between the two, but it should be checked again against the
pinned build (see "Still open").

The test used throwaway client certificates made with `openssl` and separate
client configs (`XDG_CONFIG_HOME`), with the Dusk fork at `7b39d282`. The real
pairings (`mAc mIni`, `iPhone 14 Pro Max`) were never touched. The state file
was backed up first and restored afterwards, with matching sha256.

## Verdict

**The gate passes.** A certificate written into `named_devices` by hand,
followed by a restart, is fully trusted. So is a host entry written into the
client's settings by hand. A real stream started with no pairing prompt.
Tasks 3–10 can go ahead as planned.

## Findings, step by step

**Step 1 — load at startup: works.** I appended
`{"name","cert","uuid","enabled":"true"}`, with the PEM written as Sunshine
writes it, and restarted Sunshine. Then:

- HTTPS `serverinfo` returned 200.
- `applist` returned both apps.
- A real stream connected, and Sunshine logged `CLIENT CONNECTED`. The
  client started its control, video, audio and input streams with no
  prompt.

As a control, a certificate that was not in the file got `401`, and the client
reported "has not been paired".

**`PairStatus` needs `uniqueid`.** An HTTPS `serverinfo` with no `uniqueid`
query parameter answers `PairStatus 0` even for a trusted certificate. With
`?uniqueid=…` it answers `1`. Clients always send it. Any Dusk code that probes
pairing over HTTPS must send it too, or it will report a working pairing as
unpaired.

**Step 2 — live reload: no.** With Sunshine running, the hand-added
certificate stayed `401` until a restart. Task 6's restart is needed.

**Step 3 — Sunshine's own rewrite drops hand-added entries: confirmed.** I
added an entry while Sunshine was running and did not load it, then PIN-paired
another client. Sunshine rewrote the file from its in-memory list:

- the new PIN client was there;
- the hand-added entry was gone.

So the reconciler must re-merge from the file on disk immediately before every
restart it triggers. It must not trust a merge it wrote earlier. This is
exactly Review Focus 1.

**Step 4 — server certificate.** The path is
`~/.config/sunshine/credentials/cacert.pem`. After a normal PIN pair, the
client's stored `srvcert` is **byte-identical** to that file (1038 bytes).

**Step 5 — a hand-written client entry: works.** Starting from an empty
config, `[General]` needs:

- `certificate` and `key` (PKCS#8 `BEGIN PRIVATE KEY`), each as
  `@ByteArray(...)` with `\n` escapes;
- `uniqueid`.

Under `[hosts]`, a single entry needed only:

- `1\uuid` (the host's `uniqueid`);
- `1\hostname`;
- `1\srvcert` (as `@ByteArray(...)`);
- `1\manualaddress` and `1\manualport`;
- `size=1`.

The client filled in every other key with defaults on its next write:
`customname`, `localaddress`/`localport`, `remoteaddress`/`remoteport`,
`ipv6address`/`ipv6port`, `mac`, `nvidiasw`. It fetched `apps` itself.
`list` and `stream` both worked.

**Without `srvcert` the client says "not been paired"**, even though the host
trusts its certificate. Both halves are required, which backs up Task 9's rule
that a card never shows paired when the client has no `srvcert`.

**Step 6 — Windows and macOS: not yet checked.** These need the owner's
machines.

**Step 7 — `file_state`, `cert`, `pkey`.** `GET /api/config` returns only the
keys set in `sunshine.conf`, plus `platform`, `status` and `version`. When
these keys are unset they are **absent**, not reported with their defaults. I
did not set them on this host, since moving the state file was out of scope.
Dusk should read them from the config API when present and fall back to the
default paths when absent.

## Also noticed

- This host's Sunshine answers `GET /api/pin` with `404`. `POST /api/pin`
  with only `{pin, name}` is accepted. That is the older contract `api.rs`
  already handles, and it is another sign this host is not running the pinned
  build.
- `sunshine --version` does not just print a version. It loads the config and
  logs it. Use `GET /api/config`'s `version` instead.

## Still open

1. Windows: the state file's real path for the bundled Sunshine, and
   **whether a non-elevated user can write it**. If not, Windows hosts keep
   PIN pairing for now, which is the spec's "Later" trigger.
2. macOS (Dusk Host.app): the state file's path, and Step 1.
3. Repeat Step 1 against the pinned `v2026.914.233613`. A quick run of the
   same commands after the bundle is installed is enough.
