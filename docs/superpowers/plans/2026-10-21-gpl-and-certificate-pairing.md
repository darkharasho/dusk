# GPL-3.0 and Certificate Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relicense Dusk as GPL-3.0-or-later, and let two machines on the same GitHub account pair by exchanging certificates. The host's half goes straight into Sunshine's state file and the client's half into moonlight-qt's settings. There is no PIN.

**Architecture:** The peer channel (`peer.rs`, port 47850) gains `pair_cert`/`paired` messages, signed in both directions with the account machine keys. The host keeps its own list of account pairings in the `Store`. A reconciler merges that list into `sunshine_state.json` and restarts Sunshine only when nothing is being hosted. The client asks the Dusk moonlight-qt fork to record the host through a new `add-host` command-line action. Anything that cannot take this path falls back to today's signed-PIN `Pair` flow, unchanged.

**Tech Stack:** Rust (Tauri 2, tokio, serde_json, ring Ed25519, sha2, rustls), React/TypeScript UI, the moonlight-qt fork (C++/Qt6) in the sibling checkout `~/Documents/GitHub/moonlight-qt` (branch `master`).

**Spec:** `docs/superpowers/specs/2026-10-21-gpl-and-certificate-pairing-design.md`

## Global Constraints

- Licence identifier everywhere: `GPL-3.0-or-later`.
- Dusk only ever adds or removes `named_devices` entries whose `uuid` is in its own account-pairings list. Every other entry, and every field it does not understand, is preserved byte-for-byte in meaning.
- New `named_devices` entries are `{"name", "cert", "uuid", "enabled": "true"}`. `enabled` is the **string** `"true"`.
- Sunshine is never restarted while local `serverinfo` reports `Activity::Hosting`.
- A failed or stale machine-list fetch never removes a pairing.
- Certificate pairing is attempted only when the client is `ClientKind::Dusk` and reports the `add-host` capability. Any outcome other than `paired` falls back to the existing `Pair` flow.
- Signed payloads are domain-separated and versioned, like the existing `dusk-pair-v1\n{pin}`.
- The UI must not name Sunshine or Moonlight (project rule; only the licence notice may).
- Rust checks match CI: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, all run in `src-tauri/`.
- Commits are signed through 1Password. If signing fails, stop and ask; never pass `--no-gpg-sign`.

## Review Focus

1. **A state file Sunshine rewrote while Dusk's restart was pending.** The reconciler must re-merge immediately before every restart it triggers, not reuse the bytes it computed earlier (Task 6 test `a_restart_re_merges_from_the_file_on_disk`).
2. **A state file with a `named_devices` entry already holding the same certificate,** for example a machine PIN-paired before. Dusk must not add a duplicate, and must not adopt or later remove that entry (Task 3 test `a_certificate_already_paired_is_not_added_twice`).
3. **Signing out or switching accounts while Sunshine is hosting.** The removals are written, but the restart waits (Task 6 test `revocation_waits_for_hosting_to_end_like_pairing_does`).
4. **A reply from a host that is not on the account,** or one signed over a different client certificate. The client must store nothing (Task 8 test `a_reply_signed_for_another_client_is_refused`).
5. **A PEM certificate longer than the old 1024-byte line limit,** with CRLF line endings, as Windows Sunshine writes them (Task 5 test `a_pair_cert_line_with_a_crlf_certificate_fits_and_round_trips`).

---

### Task 1: Spike — prove the state-file approach (throwaway, gates everything after Task 2)

No product code. The output is a findings file. **Stop after this task and show the findings to the owner** before Tasks 3–10.

**Files:**
- Create: `docs/superpowers/specs/2026-10-21-certificate-pairing-spike.md`

- [ ] **Step 1: Linux host, by hand.** Stop Sunshine (`systemctl --user stop sunshine`, or however `host/platform.rs` stops it here). Back up `~/.config/sunshine/sunshine_state.json`. Add an entry to `root.named_devices` with a client's certificate: take a machine's moonlight-qt `certificate` from its settings and generate a fresh uppercase UUID. Start Sunshine. Using that machine's existing client, which already trusts this host, confirm a stream starts with no pairing prompt. Then restore the backup.
- [ ] **Step 2: Linux, live reload.** Repeat Step 1 with Sunshine running, and without restarting it. Record whether the new entry takes effect. If it does, Task 6's restart becomes unnecessary; note that and flag it to the owner.
- [ ] **Step 3: Linux, Sunshine's own rewrite.** With Sunshine running and a hand-added entry in the file but not yet loaded, PIN-pair some other client. Record whether Sunshine's write drops the hand-added entry. This confirms the re-merge-before-restart design.
- [ ] **Step 4: server certificate.** Record the path of the host's Sunshine server certificate (expected `~/.config/sunshine/credentials/cacert.pem`). Record whether moonlight-qt's stored `srvcert` for that host is byte-identical to it after a normal PIN pair.
- [ ] **Step 5: client side, by hand.** On one client with moonlight-qt closed, hand-write a host entry (`uuid`, `hostname`, `srvcert`, `localaddress`, `localport`, `httpsport` — read an existing paired entry for the full key set) for a host that has that client's certificate in `named_devices`. Open moonlight-qt and confirm it shows the host as paired and streams. List every key moonlight-qt needed.
- [ ] **Step 6: Windows and macOS.** These need the owner's other machines; ask the owner to run Steps 1 and 4 there, or record them as "not yet checked". For Windows, record the state file's path (expected `%ProgramFiles%\Sunshine\config\sunshine_state.json`), and **whether a non-elevated user can write it**. If not, cert pairing on Windows hosts falls back to PIN for now. Record that as the trigger for the spec's "Later" section. Do not add a UAC prompt to pairing.
- [ ] **Step 7: `file_state`, `cert`, `pkey`.** Record whether `SunshineApi::get_config` returns these keys when they are unset and when they are set. Dusk will honour them if present.
- [ ] **Step 8: Write the findings and commit.**

```bash
git add docs/superpowers/specs/2026-10-21-certificate-pairing-spike.md
git commit -m "Record what the state-file pairing spike found"
```

**Gate:** if Step 1 or Step 5 fails, stop. The spec's "Later" applies, and this plan's Tasks 3–10 are replaced by a new design.

---

### Task 2: Relicense Dusk as GPL-3.0-or-later

Independent of the spike; can run before or alongside Task 1.

**Files:**
- Create: `LICENSE` (copy of `src-tauri/licenses/GPL-3.0.txt`)
- Modify: `package.json` (add `"license": "GPL-3.0-or-later"`), `src-tauri/Cargo.toml` (`license = "GPL-3.0-or-later"` under `[package]`)
- Modify: `README.md` (the section beginning "**moonlight-common-c is GPL-3.0, not LGPL.**"), `docs/architecture.md` ("The constraint everything else bends around" and the target-shape paragraph about mere aggregation), `src-tauri/licenses/README.md` (only if it implies Dusk is under another licence)

- [ ] **Step 1:** Create `LICENSE` and the two manifest fields.
- [ ] **Step 2:** Rewrite the README section. Keep: Limelight.h has no pairing, and pairing lives in moonlight-qt's `NvPairingManager`. Keep: the client is a separate process and the overlay lives in the fork. Replace the licence rationale with the engineering one: a crashed stream must not take the window down, and video does not belong in the webview. Delete "pipes and sockets are fine, linking and shared in-memory structures are not."
- [ ] **Step 3:** Rewrite `docs/architecture.md`'s constraint section the same way. Retitle it, for example "Three programs, one face", and state that Dusk is GPL-3.0-or-later.
- [ ] **Step 4: Verify.** Run `grep -rn "unencumbered\|relicenses\|shell is not" README.md docs/architecture.md src-tauri/licenses/README.md`. Expected: no hits that still argue Dusk must stay non-GPL. Run `cd src-tauri && cargo metadata --no-deps --format-version 1 | grep -o '"license":"[^"]*"'`. Expected: `"license":"GPL-3.0-or-later"`.
- [ ] **Step 5: Commit.**

```bash
git add LICENSE package.json src-tauri/Cargo.toml README.md docs/architecture.md src-tauri/licenses/README.md
git commit -m "License Dusk under the GPL, and say why it is still three programs"
```

---

### Task 3: Sunshine's state file — locate, merge, write

**Files:**
- Create: `src-tauri/src/sunshine/state_file.rs`
- Modify: `src-tauri/src/sunshine/mod.rs` (add `pub mod state_file;`)
- Test: inline `#[cfg(test)] mod tests` in `state_file.rs`, following the repo's style

**Interfaces:**
- Produces:
  - `pub struct NamedDevice { pub name: String, pub cert: String, pub uuid: String }`
  - `pub fn config_dir() -> Option<PathBuf>`: Linux and macOS give `$XDG_CONFIG_HOME/sunshine`, else `~/.config/sunshine`. Windows gives `%ProgramFiles%\Sunshine\config`. Adjust to Task 1's findings.
  - `pub fn state_path(config: Option<&serde_json::Map<String, Value>>) -> Option<PathBuf>`: honours a `file_state` key, resolved relative to `config_dir()`; the default is `sunshine_state.json`.
  - `pub fn server_cert_path(config: Option<&serde_json::Map<String, Value>>) -> Option<PathBuf>`: honours `cert`; the default is `credentials/cacert.pem`.
  - `pub fn merge(state: &Value, add: &[NamedDevice], remove_uuids: &[String]) -> Result<Option<Value>, StateFileError>`: `Ok(None)` means nothing changed.
  - `pub fn read(path: &Path) -> Result<Value, StateFileError>` and `pub fn write_atomic(path: &Path, state: &Value) -> Result<(), StateFileError>`. The write goes to `<path>.dusk-tmp`, then `std::fs::rename`, keeping Sunshine's pretty-printed form (`serde_json::to_string_pretty`).
  - `#[derive(Debug, thiserror::Error)] pub enum StateFileError { Missing, Unreadable(String), Unwritable(String), UnknownShape }`. `thiserror` is already used by `MoonlightError`; check `Cargo.toml` and match whatever the crate uses.

- [ ] **Step 1: Write the failing tests.** Use a fixture string shaped exactly like the observed file: top-level `username`/`salt`/`password`, then `root.uniqueid`, plus `root.named_devices` with one foreign entry and an extra unknown field `"future": 1` on root.

```rust
#[test] fn a_device_is_added_with_enabled_as_the_string_true()
    // merge(add=[d]) → named_devices.last() == {"name":d.name,"cert":d.cert,"uuid":d.uuid,"enabled":"true"}
#[test] fn everything_dusk_does_not_own_is_kept()
    // after merge: username/salt/password/root.uniqueid/root.future unchanged; foreign entry unchanged
#[test] fn adding_what_is_already_there_changes_nothing()
    // merge(add=[d]) on a state already holding d.uuid → Ok(None)
#[test] fn a_certificate_already_paired_is_not_added_twice()
    // foreign entry has cert C under uuid U1; merge(add=[{cert:C, uuid:U2}]) → Ok(None)
    // and merge(remove=[U2]) on that state → Ok(None) (foreign U1 untouched)
#[test] fn only_listed_uuids_are_removed()
    // merge(remove=[d.uuid]) removes d only; foreign entry stays
#[test] fn a_state_with_no_devices_yet_gets_the_list()
    // root without named_devices → root.named_devices == [d]
#[test] fn an_unrecognised_shape_is_refused_not_rewritten()
    // root.named_devices is an object, or root missing → Err(UnknownShape)
#[test] fn file_state_moves_the_file_relative_to_the_config_dir()
    // state_path(Some({"file_state":"other.json"})) ends with "sunshine/other.json"; absolute value used as-is
#[test] fn a_write_lands_whole_and_reads_back()
    // write_atomic to a temp dir then read → equal Value; no .dusk-tmp left behind
```

Certificate equality compares the PEM with whitespace and `\r` stripped, so a CRLF copy matches an LF copy.

- [ ] **Step 2:** Run `cd src-tauri && cargo test state_file`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement the interfaces above.
- [ ] **Step 4:** Run `cd src-tauri && cargo test state_file && cargo clippy --all-targets -- -D warnings`. Expected: PASS, no warnings.
- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/sunshine/state_file.rs src-tauri/src/sunshine/mod.rs
git commit -m "Read and merge Sunshine's list of paired machines"
```

---

### Task 4: Signed certificate-pairing payloads

**Files:**
- Modify: `src-tauri/src/account/keys.rs`, `src-tauri/src/account/mod.rs`
- Test: inline tests in both files

**Interfaces:**
- Produces, in `keys.rs`:
  - `pub fn cert_request_message(client_cert_pem: &str, host_uniqueid: &str) -> Vec<u8>` = `format!("dusk-pair-cert-v1\n{host_uniqueid}\n{}", sha256_hex(normalised client cert))`
  - `pub fn cert_reply_message(client_cert_pem: &str, host_uniqueid: &str, server_cert_pem: &str) -> Vec<u8>` = `format!("dusk-paired-v1\n{host_uniqueid}\n{}\n{}", sha256_hex(client), sha256_hex(server))`
  - Here "normalised" means `\r` and trailing whitespace removed, using the same helper Task 3 uses for comparison. Export it from `state_file` as `pub fn normalise_pem(&str) -> String`, and use it here.
  - `MachineKey::sign(&self, message: &[u8]) -> String` and `pub fn verify(public_hex: &str, message: &[u8], signature_hex: &str) -> bool`. Re-express `sign_pair`/`verify_pair` through them; their behaviour must not change.
- Produces, in `account/mod.rs`:
  - `AccountState::sign(&self, message: &[u8]) -> Option<Proof>`, with `proof(pin)` becoming `sign(&pair_message(pin))` (make `pair_message` `pub(crate)`)
  - `AccountState::vouches_for(&self, http: &reqwest::Client, proof: &Proof, message: &[u8]) -> bool`, the existing `vouches` logic over arbitrary bytes, with `vouches(pin)` delegating to it
  - `pub enum Membership { Unknown, SignedOut, Machines(std::collections::HashSet<String>) }` and `AccountState::membership(&self) -> Membership`. It returns `Machines` only if `MachineList.fetched` is `Some` (any age), `SignedOut` when no account is loaded, and `Unknown` otherwise.

- [ ] **Step 1: Write the failing tests.**

```rust
// keys.rs
#[test] fn a_pin_signature_still_verifies_exactly_as_before()
    // sign_pair("1234") verifies with verify_pair and with verify(&pair_message("1234"))
#[test] fn a_request_signature_does_not_verify_for_another_host()
    // sign(cert_request_message(c, "H1")) fails verify against cert_request_message(c, "H2")
#[test] fn a_request_signature_cannot_pass_as_a_reply()
    // sign(cert_request_message(c, h)) fails verify against cert_reply_message(c, h, s)
#[test] fn crlf_and_lf_copies_of_a_certificate_sign_the_same()
// mod.rs
#[test] fn membership_is_unknown_until_a_list_has_been_fetched()
#[test] fn membership_lists_the_machines_once_fetched()
```

- [ ] **Step 2:** Run `cd src-tauri && cargo test account::`. Expected: the new tests FAIL.
- [ ] **Step 3:** Implement the interfaces above.
- [ ] **Step 4:** Run `cd src-tauri && cargo test account:: && cargo clippy --all-targets -- -D warnings`. Expected: PASS, existing account tests included.
- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/account/keys.rs src-tauri/src/account/mod.rs src-tauri/src/sunshine/state_file.rs
git commit -m "Sign a certificate pairing both ways, and tell a known machine list from none"
```

---

### Task 5: Peer protocol — `pair_cert` and `paired`

**Files:**
- Modify: `src-tauri/src/peer.rs` (`Message`, `MAX_LINE`, `serve`'s reader limit, module docs' protocol block)
- Test: inline tests in `peer.rs`

**Interfaces:**
- Produces, as new `Message` variants (snake_case tags, like the rest):
  - `PairCert { name: String, cert: String, proof: crate::account::Proof }`
  - `Paired { server_cert: String, uniqueid: String, ready: bool, proof: crate::account::Proof }`
- `MAX_LINE` becomes `16 * 1024`. Both `serve` and `ask_at` keep their existing multipliers off it.
- `serve` dispatches `PairCert` to `crate::cert_pairing::host::handle(&state, &address, name, cert, proof).await -> Message`. That function is produced in Task 6. Until then, stub it to return `Declined { reason: "…".into() }` so this task compiles and tests alone.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn a_pair_cert_line_with_a_crlf_certificate_fits_and_round_trips()
    // a 2 KiB PEM with \r\n; serde round-trips; serialized line len < MAX_LINE
#[test] fn an_older_dusk_reads_pair_cert_as_something_it_declines()
    // the JSON of PairCert does not parse as any pre-existing variant (so old serve hits the `_ =>` decline arm)
#[tokio::test] async fn a_host_too_old_for_pair_cert_declines_over_the_wire()
    // use host_expecting(...) helper with reply {"type":"declined","reason":"Dusk did not understand that request."}
```

- [ ] **Step 2:** Run `cd src-tauri && cargo test peer::`. Expected: new tests FAIL.
- [ ] **Step 3:** Add the variants, the `MAX_LINE` change, the dispatch and the stub. Update the protocol block in the module docs.
- [ ] **Step 4:** Run `cd src-tauri && cargo test peer:: && cargo clippy --all-targets -- -D warnings`. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/peer.rs src-tauri/src/cert_pairing
git commit -m "Teach the peer channel to carry a certificate"
```

---

### Task 6: The host's half — account pairings, reconcile, deferred restart

**Files:**
- Create: `src-tauri/src/cert_pairing/mod.rs`, `src-tauri/src/cert_pairing/host.rs`
- Modify: `src-tauri/src/lib.rs` (`mod cert_pairing;`, spawn the reconciler at startup), `src-tauri/src/store.rs` (new field), `src-tauri/src/state.rs` (hold the reconciler handle), `src-tauri/src/account/mod.rs` (`sign_out` and successful list refresh nudge the reconciler)
- Test: inline tests in `cert_pairing/host.rs`

**Interfaces:**
- Consumes: `state_file::{read, merge, write_atomic, state_path, server_cert_path, NamedDevice, StateFileError}` (Task 3); `keys::{cert_request_message, cert_reply_message}`, `AccountState::{sign, vouches_for, membership}`, `Membership` (Task 4); `serverinfo::query_http(...).activity()` against `127.0.0.1` on `DEFAULT_HTTP_PORT`; `SunshineApi::restart`; `state.credentials.load()`.
- Produces:
  - `store.rs`: `#[serde(default)] pub account_pairings: Vec<AccountPairing>`, with `pub struct AccountPairing { pub machine: String, pub name: String, pub cert: String, pub uuid: String }`. `uuid` is a fresh uppercase v4, minted by Dusk.
  - `cert_pairing::host::plan(pairings: &[AccountPairing], membership: &Membership) -> (Vec<NamedDevice>, Vec<String>)` (pure). `Machines(set)` keeps the pairings whose machine is in the set and removes the rest. `SignedOut` removes all. `Unknown` keeps all and removes none.
  - `pub struct Reconciler`. It holds a `tokio::sync::Notify`, with `pub fn nudge(&self)` and `pub fn spawn(app, state)`. It runs one pass immediately at Dusk startup, which covers a Sunshine that rebooted or was reinstalled. Then one loop runs: wait for a nudge or a 60 s tick → `plan` → if the plan would change the file, write it → if written, enter "restart pending". While pending, every 15 s: query local activity; when `Idle`, **re-read and re-merge the file**, write if changed, then `SunshineApi::restart`. After a successful write, drop removed pairings from the `Store`.
  - `pub async fn handle(state: &Arc<AppState>, address: &str, name: String, cert: String, proof: Proof) -> Message`:
    - verify `vouches_for(proof, cert_request_message(cert, own_uniqueid))`;
    - upsert an `AccountPairing` keyed by `proof.machine`, replacing the cert if it changed;
    - run one reconcile pass inline;
    - reply `Paired { server_cert, uniqueid, ready, proof: sign(cert_reply_message(...)) }`. `ready` is true only if the file was already current, or the restart has completed and Sunshine answers `serverinfo` again within 20 s.
    - Any `StateFileError`, a missing server cert, an unverified proof, or a host not set up (`credentials.load()` is `None`) gives `Declined` with a person-readable reason in Dusk's voice that never names Sunshine. The client then falls back to the PIN flow.
  - Own uniqueid: read from the state file's `root.uniqueid`.

- [ ] **Step 1: Write the failing tests.** Use pure-function tests where possible. For the loop, inject the activity probe, the restart and the clock behind a small trait (`trait Host { async fn hosting(&self) -> Option<bool>; async fn restart(&self) -> Result<(), String>; }`), the way `account` injects its keystore reader.

```rust
#[test] fn an_unknown_membership_keeps_every_pairing()
#[test] fn signing_out_removes_every_pairing_dusk_added()
#[test] fn a_machine_off_the_account_is_removed_and_the_rest_kept()
#[tokio::test] async fn nothing_restarts_while_the_host_is_hosting()
    // fake Host: hosting=Some(true) for 3 polls then Some(false); restart called exactly once, after the 4th poll
#[tokio::test] async fn a_restart_re_merges_from_the_file_on_disk()
    // after planning, overwrite the temp state file without Dusk's entry (as Sunshine would); on restart the file contains it again
#[tokio::test] async fn revocation_waits_for_hosting_to_end_like_pairing_does()
#[tokio::test] async fn an_unreachable_host_is_not_treated_as_idle()
    // hosting=None → no restart
#[tokio::test] async fn a_request_signed_for_another_host_is_declined()
#[tokio::test] async fn a_state_file_it_cannot_read_declines_rather_than_pairs()
```

- [ ] **Step 2:** Run `cd src-tauri && cargo test cert_pairing::`. Expected: FAIL.
- [ ] **Step 3:** Implement. Replace Task 5's stub with `handle`. Spawn the `Reconciler` in `lib.rs` next to `peer::spawn`. Nudge it after a successful `refresh_list` and in `sign_out`, **before** `state.account.save(None)`, so `SignedOut` is what it sees.
- [ ] **Step 4:** Run `cd src-tauri && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`. Expected: PASS.
- [ ] **Step 5: Manual check on this Linux host.** Run `npm run tauri dev`, then from another account machine run Task 8's flow, or simulate it with `nc localhost 47850` and a hand-signed request using a test helper. Confirm the entry appears in `~/.config/sunshine/sunshine_state.json` and that Sunshine restarts only once nothing is streaming.
- [ ] **Step 6: Commit.**

```bash
git add src-tauri/src/cert_pairing src-tauri/src/lib.rs src-tauri/src/store.rs src-tauri/src/state.rs src-tauri/src/account/mod.rs src-tauri/src/peer.rs
git commit -m "Pair your own machines on the host without a PIN, and never mid-stream"
```

---

### Task 7: The fork's `add-host` action, released and pinned

Work in `~/Documents/GitHub/moonlight-qt` (branch `master`). Build as in the "Building the Dusk Moonlight fork" memory: the `fedora44-build` distrobox, then `patchelf --set-rpath '$ORIGIN' app/moonlight` after every `make`.

**Files (fork):**
- Create: `app/cli/addhost.cpp`, `app/cli/addhost.h`
- Modify: `app/cli/commandlineparser.cpp/.h` (new mode), `app/main.cpp` (dispatch), `app/app.pro` (sources), and the code that prints `--version` (add a line)

**Files (Dusk):**
- Modify: `.github/workflows/release.yml` (`CLIENT_TAG`)

**Interfaces:**
- Produces this command line:

```
moonlight add-host <address> --uuid <uniqueid> --name <name> --server-cert <path-to-pem> [--port <http-port>]
```

  - Exit 0: the host is stored as paired, with that `srvcert`, `uuid` and address, plus whatever else Task 1 Step 5 found it needs. An existing entry with the same `uuid` is updated in place.
  - Exit 2: another moonlight-qt instance is running. Refuse, and print one line to stderr.
  - Exit 1: any other failure, with one line to stderr.
- `moonlight --version` gains the line `Dusk fork: add-host available`.

- [ ] **Step 1:** Implement. Reuse `NvComputer` serialisation and `ComputerManager`'s persistence rather than writing `QSettings` keys by hand, so the stored form is exactly what moonlight-qt writes itself. Detect a running instance with the same mechanism moonlight-qt already uses for single-instance (`SingleApplication`/`QLocalServer`, whichever the fork has).
- [ ] **Step 2: Manual check.** With moonlight-qt closed, run `./app/moonlight add-host <this host> --uuid … --name … --server-cert ~/.config/sunshine/credentials/cacert.pem` against a host whose `named_devices` already holds this client's certificate (from Task 1). Open `./app/moonlight`. Expected: the host shows as paired and a stream starts. Run it again with the GUI open. Expected: exit 2.
- [ ] **Step 3:** Commit, merge to `master`, tag and release the fork as `dusk-v0.2.5` through its own workflow.
- [ ] **Step 4:** In Dusk, set `CLIENT_TAG: dusk-v0.2.5` in `release.yml` and commit:

```bash
git add .github/workflows/release.yml
git commit -m "Ship the client that can record a host it was told about"
```

---

### Task 8: The client's half — try certificates first

**Files:**
- Create: `src-tauri/src/cert_pairing/client.rs`
- Modify: `src-tauri/src/moonlight/cli.rs` (capability and `add_host`), `src-tauri/src/peer.rs` (`ask_cert`), `src-tauri/src/commands.rs` (`pair_device`), `src/api.ts` and `src/components/DeviceDrawer.tsx` (new stage)
- Test: inline tests in `cert_pairing/client.rs`, `moonlight/cli.rs`, `peer.rs`

**Interfaces:**
- Consumes: `Message::{PairCert, Paired}` (Task 5); `keys::{cert_request_message, cert_reply_message}`, `AccountState::{sign, vouches_for}` (Task 4); `ClientIdentity.cert_pem` from `moonlight::identity`; the fork CLI (Task 7).
- Produces:
  - `Moonlight::supports_add_host(&self) -> bool`: `kind == ClientKind::Dusk` and `--version` stdout contains `Dusk fork: add-host available`. Run it once and cache the result in a `OnceCell`.
  - `Moonlight::add_host(&self, address: &str, uuid: &str, name: &str, server_cert_pem: &str) -> Result<(), MoonlightError>`. It writes the PEM to a temp file, runs the action with `ACTION_TIMEOUT`, and deletes the file. Exit 2 maps to a new `MoonlightError::ClientOpen`.
  - `peer::ask_cert(address: &str, request: Message) -> CertOutcome`, with `pub enum CertOutcome { NoPeer, Paired { server_cert: String, uniqueid: String, ready: bool, proof: Proof }, Declined(String) }`.
  - `cert_pairing::client::pair(state: &Arc<AppState>, address: &str, device_id: &str, on_waiting_for_host: impl FnOnce()) -> Result<bool, String>`. `Ok(true)` means paired by certificate. `Ok(false)` means not attempted, or the host declined or isn't a Dusk, so the caller should use the PIN flow. `Err` means the host half is done but the client half failed. The steps:
    1. If `supports_add_host()` is false, or there is no account proof or no client identity, return `Ok(false)`.
    2. Sign `cert_request_message(own cert, device_id)`, where `device_id` is the host's Sunshine uniqueid (the registry key), and send it.
    3. On `Paired`, require `reply.uniqueid == device_id` and `vouches_for(reply.proof, cert_reply_message(own cert, device_id, server_cert))`. If either fails, return `Ok(false)` and store nothing.
    4. Call `add_host`. On `ClientOpen`, retry every 5 s for up to 2 minutes, then `Err("Close the stream window to finish pairing.")`.
    5. If `ready` is false, call `on_waiting_for_host` and poll `serverinfo` over TLS every 5 s until `PairingState::Paired`, with no deadline while the host reports `Hosting`.
    6. Return `Ok(true)`.
- `pair_device`: call `cert_pairing::client::pair` first. On `Ok(true)`, finish as the existing tail does (`reload_identity`, `request_refresh`, `emit_snapshot`). On `Ok(false)`, run today's code unchanged.
- UI: `PairProgress.stage` gains `"waitingForHost"`. `DeviceDrawer` shows it in Dusk's voice, for example "That machine is in use. Pairing will finish when it's free."

- [ ] **Step 1: Write the failing tests.**

```rust
// moonlight/cli.rs
#[test] fn a_stock_client_is_never_asked_to_add_a_host()          // kind System → supports_add_host() false without running it
#[test] fn the_capability_line_is_found_in_version_output()       // parser over a captured --version string
// peer.rs
#[tokio::test] async fn a_paired_reply_travels_back_over_the_wire()  // host_answering with a Paired JSON → CertOutcome::Paired
// cert_pairing/client.rs
#[test] fn a_reply_signed_for_another_client_is_refused()
    // the verification fn rejects a proof over cert_reply_message(other_cert, …)
#[test] fn a_reply_naming_another_host_is_refused()               // reply.uniqueid != device_id
```

  Keep step 3's verification in a pure `fn accept_reply(own_cert, device_id, reply, verify: impl Fn(&Proof, &[u8]) -> bool) -> Option<(String, String, bool)>` so it can be tested without a network.

- [ ] **Step 2:** Run `cd src-tauri && cargo test cert_pairing:: moonlight:: peer::`. Expected: new tests FAIL.
- [ ] **Step 3:** Implement the interfaces above, including the UI stage.
- [ ] **Step 4:** Run `cd src-tauri && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`, then `npm run build`. Expected: all PASS.
- [ ] **Step 5: Manual check end to end.** Use two account machines, both on this branch and both with the Task 7 fork. Unpair the client in Dusk (or delete both halves). Pair from the client. Expected: no PIN is shown, and the card turns paired. Repeat while the host is streaming to a third device. Expected: "in use" is shown, and the card becomes paired by itself after that stream ends. Repeat with `DUSK_MOONLIGHT_BIN` pointing at a stock moonlight-qt. Expected: the old signed-PIN flow, working as before.
- [ ] **Step 6: Commit.**

```bash
git add src-tauri/src/cert_pairing src-tauri/src/moonlight/cli.rs src-tauri/src/peer.rs src-tauri/src/commands.rs src/api.ts src/components/DeviceDrawer.tsx
git commit -m "Pair your own machines by certificate, falling back to the PIN"
```

---

### Task 9: Re-pair when a host's certificate changed

**Files:**
- Modify: `src-tauri/src/moonlight/hosts.rs` (read `srvcert` into `KnownHost`), `src-tauri/src/serverinfo.rs` or `state.rs` (capture the server certificate a TLS probe saw), `src-tauri/src/poller.rs` (compare and trigger)
- Test: inline tests in `hosts.rs` and `poller.rs`

**Interfaces:**
- Consumes: `cert_pairing::client::pair` (Task 8); `AccountState::machines_on_account` / `knows_machine`.
- Produces:
  - `KnownHost.server_cert: Option<String>` (PEM), read from each platform's store the same way the other fields are.
  - The TLS probe exposes the DER of the certificate the host presented. Try reqwest's `ClientBuilder::tls_info(true)` with `Response::extensions().get::<reqwest::tls::TlsInfo>()` first. If that yields nothing with `use_preconfigured_tls`, record the end-entity cert in the custom verifier `state::tls_client` already installs.
  - `pub fn certificate_changed(stored_pem: &str, presented_der: &[u8]) -> bool` (pure; it decodes the PEM with `rustls-pemfile`).
  - In the poller: when the device is an account machine, a stored `server_cert` exists and `certificate_changed` is true, spawn `cert_pairing::client::pair` once per device per 10 minutes. Never show "paired" for that device until it succeeds.
  - **No half pairings on the card:** a device the TLS probe calls `Paired`, but for which moonlight-qt's store holds no `server_cert` under its uuid, shows `NotPaired`. That is a host that trusts this client while this client cannot trust it back. PIN-paired hosts always have a `srvcert`, so they are unaffected.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn the_stored_server_certificate_is_read_with_the_host()   // ini fixture with 1.srvcert=@ByteArray(...)
#[test] fn the_same_certificate_is_not_a_change()
#[test] fn a_different_certificate_is_a_change()
#[test] fn a_machine_off_the_account_is_never_re_paired()
#[test] fn a_re_pair_is_not_retried_more_than_once_in_ten_minutes()
#[test] fn a_host_trusting_us_that_we_cannot_trust_back_is_not_shown_paired()
```

- [ ] **Step 2:** Run `cd src-tauri && cargo test hosts:: poller::`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cd src-tauri && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`. Expected: PASS.
- [ ] **Step 5: Manual check.** On the Linux host, move `~/.config/sunshine/credentials/` aside and restart Sunshine, which mints a new certificate. Expected: within one poll, the client re-pairs without a prompt and streams. Then restore the original credentials directory.
- [ ] **Step 6: Commit.**

```bash
git add src-tauri/src/moonlight/hosts.rs src-tauri/src/serverinfo.rs src-tauri/src/state.rs src-tauri/src/poller.rs
git commit -m "Pair again when one of your machines got a new certificate"
```

---

### Task 10: Pickup notes

**Files:**
- Modify: `docs/pickup.md` (pairing section; licence; the fork's new action and `CLIENT_TAG`), `src-tauri/src/peer.rs` module docs (the "# The trust decision" section: certificates first, PIN for everything else)

- [ ] **Step 1:** Update both. Include Task 1's findings that a future session needs: the paths, whether a restart is required, and the Windows write permission.
- [ ] **Step 2:** Run `cd src-tauri && cargo doc --no-deps 2>&1 | grep -i warning`. Expected: no new warnings.
- [ ] **Step 3: Commit.**

```bash
git add docs/pickup.md src-tauri/src/peer.rs
git commit -m "Bring the pickup notes up to certificate pairing"
```
