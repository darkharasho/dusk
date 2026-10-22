# Clipboard Sync While Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While machine A streams from host B, text copied on either side can be pasted on the other, in both directions, until the stream ends.

**Architecture:** On A, the stream window (the moonlight-qt fork) reads and writes A's clipboard through SDL. It relays copies to Dusk over the existing per-session control socket. A's Dusk opens a TLS connection to B's Dusk on a new port, with both certificates pinned to the ones pairing already put in place. B's Dusk polls and writes B's clipboard with `arboard`. B only admits a paired client while its own host is busy streaming. A small loop guard on each side stops echoes.

**Tech Stack:** Rust (Tauri 2, tokio, rustls 0.23 with the ring provider already in the tree, new `arboard` dependency, new `rcgen` dev-dependency), React/TypeScript settings UI, C++/SDL2 in `../moonlight-qt`.

**Spec:** `docs/superpowers/specs/2026-10-22-clipboard-sync-design.md`

## Global Constraints

- Text only (UTF-8). Every clipboard message carries `"format":"text"`. A message with any other format is ignored, not treated as an error.
- Size cap: 1 MB of text per copy, measured in UTF-8 bytes: `pub const MAX_TEXT_BYTES: usize = 1024 * 1024;`. Anything over the cap is not sent. Instead the overlay shows a brief notice.
- Clipboard contents are never logged, never written to disk, and never included in an error string or a `Debug` output.
- Clipboard bytes never cross the network outside TLS. Both ends pin the exact certificate DER. There is no CA validation and no trust-on-first-use.
- B admits a connection only when all of these hold: the client certificate is one of Sunshine's paired-device certificates, sharing is on at B, B's desktop is supported, and B's Sunshine reports `Activity::Hosting`. B rechecks the last one every 5 s and closes the connection when it stops holding.
- Nothing is sent when a stream starts. A side sends only once its clipboard differs from the baseline it read at connection start.
- The UI never names Sunshine or Moonlight.
- Setting copy: label "Share clipboard while streaming". Detail: "Text you copy on either machine can be pasted on the other while a stream is running." Default on.
- GNOME on Wayland as a host (`XDG_SESSION_TYPE=wayland` and `XDG_CURRENT_DESKTOP` contains `GNOME`) is unsupported. B declines with reason `unsupported`, and A's drawer explains why.
- An older Dusk on B (nothing listening on the port) means no sync and no message. The stream itself is unaffected.
- Commits: in this repo, signing needs the owner's 1Password prompt, so `git commit` fails from an agent session. Each "Commit" step means: stage the files, write the message to `.git/COMMIT_PENDING_MSG`, and tell the owner. Messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Backend checks before a task is done: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`. Frontend checks: `npm run typecheck`.

## Review Focus

- **Stale contents at stream start:** A copied something an hour ago, then starts a stream. Nothing is sent until A or B copies something new. Pinned in Task 2 (`guard_does_not_send_the_baseline`) and Task 7 (fork baseline).
- **Echo after applying a remote copy:** B writes text that came from A, then its 500 ms poll sees that text. It must not send it back. Pinned in Task 2 (`applied_text_is_not_sent_back`) and Task 5 (`host_loop_does_not_echo`).
- **Text that is awkward to frame:** CRLF line endings, multi-line text, emoji, a JSON-looking string, embedded `\u{0}`. Each must arrive byte-identical, as one line on the wire. Pinned in Task 2 (`awkward_text_round_trips_on_one_line`).
- **Empty clipboard:** clearing the clipboard, or copying an image (which reads as no text), must not send an empty string and wipe the other side. Pinned in Task 2 (`empty_text_is_never_sent`).
- **Host stops streaming but the TCP connection stays up:** B must close within about 5 s. Text copied on B after the stream ends must not leave the machine. Pinned in Task 5 (`host_closes_when_streaming_stops`).

---

### Task 1: The setting

**Files:**
- Modify: `src-tauri/src/store.rs` (new field and enum, next to `virtual_screen`)
- Modify: `src-tauri/src/state.rs:401-447` (snapshot field)
- Modify: `src-tauri/src/commands.rs` (new command after `set_codec`, ~line 765) and `src-tauri/src/lib.rs` (register it)
- Modify: `src/types.ts`, `src/api.ts`, `src/components/StreamingSection.tsx`
- Test: `src-tauri/src/store.rs` tests module

**Interfaces:**
- Produces: `pub enum ClipboardSharing { #[default] On, Off }` (serde `rename_all = "camelCase"`), `Store.clipboard_sharing: ClipboardSharing`, `impl ClipboardSharing { pub fn is_on(self) -> bool }`. Tauri command `set_clipboard_sharing(on: bool)`. Snapshot field `clipboard_sharing: bool` (TS `clipboardSharing: boolean`). TS `setClipboardSharing(on: boolean): Promise<void>`.

An enum rather than a `bool` so the derived `Default` on `Store` gives On, both for a fresh install and for an existing settings file without the field.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn clipboard_sharing_is_on_for_an_existing_file_without_it() {
    let store: Store = serde_json::from_str(r#"{"codec":"auto"}"#).unwrap();
    assert!(store.clipboard_sharing.is_on());
}

#[test]
fn clipboard_sharing_off_survives_a_save() {
    let mut store = Store::default();
    store.clipboard_sharing = ClipboardSharing::Off;
    let back: Store = serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
    assert!(!back.clipboard_sharing.is_on());
}
```

- [ ] **Step 2: Run them to verify they fail.** Run `cd src-tauri && cargo test store::tests::clipboard_sharing`. Expected: a compile error, because there is no `clipboard_sharing` yet.
- [ ] **Step 3: Implement.** Add the enum and the field. Add `set_clipboard_sharing`, shaped like `set_codec`: lock, set, `save`, `emit_snapshot`. Add the snapshot field and its TS type, plus `setClipboardSharing` in `api.ts`. In `StreamingSection.tsx`, add a checkbox below the codec block using the label and detail copy from Global Constraints, saved through the section's existing `save(() => …)` helper.
- [ ] **Step 4: Verify.** `cargo test store::tests::clipboard_sharing` passes, and the backend checks and `npm run typecheck` pass.
- [ ] **Step 5: Commit.** "Add the Share clipboard while streaming setting"

---

### Task 2: Clipboard core: wire messages, size cap, loop guard

**Files:**
- Create: `src-tauri/src/clipboard/mod.rs` (module docs explain the design in this file's prose style; it re-exports the items below), `src-tauri/src/clipboard/wire.rs`, `src-tauri/src/clipboard/guard.rs`
- Modify: `src-tauri/src/lib.rs` (`mod clipboard;`)

**Interfaces:**
- Produces, in `clipboard::wire`:
  ```rust
  pub const PORT: u16 = 47851;
  pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
  /// Longest line read from the other Dusk: the cap, JSON-escaped (worst case 6×), plus framing.
  pub const MAX_LINE: usize = MAX_TEXT_BYTES * 6 + 1024;

  #[derive(Clone, PartialEq, Serialize, Deserialize)]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum Wire {
      Hello { version: u32 },                 // first line each way, version 1
      Clip { format: String, text: String },  // format is "text"
      Declined { reason: Decline },
      Bye,
      #[serde(other)] Unknown,
  }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum Decline { SharingOff, NotStreaming, NotPaired, Unsupported { detail: Option<String> } }

  pub fn line(message: &Wire) -> String;          // one JSON object + "\n"
  pub fn text_of(message: &Wire) -> Option<&str>; // Some only for Clip with format "text"
  pub fn fits(text: &str) -> bool;                // text.len() <= MAX_TEXT_BYTES
  ```
  `Wire` gets a hand-written `Debug` that prints `Clip { format, len }` and never prints the text.
- Produces, in `clipboard::guard`:
  ```rust
  pub struct Guard { /* sha256 of the baseline, last sent, last applied */ }
  impl Guard {
      pub fn new(baseline: Option<&str>) -> Self;
      /// Local clipboard now reads `text`: should it be sent? Records it if so.
      pub fn outgoing(&mut self, text: &str) -> bool;
      /// `text` arrived from the other side: should it be written locally? Records it if so.
      pub fn incoming(&mut self, text: &str) -> bool;
  }
  ```
  `outgoing` returns false for empty text, and for text equal to the baseline, the last sent or the last applied. `incoming` returns false for empty text and for text equal to the last applied. After `incoming("x")`, `outgoing("x")` is false. Compare hashes only, so the guard never holds clipboard text.

- [ ] **Step 1: Write the failing tests** (in each file's `#[cfg(test)] mod tests`)

```rust
// guard.rs
#[test] fn guard_does_not_send_the_baseline() {
    let mut g = Guard::new(Some("old"));
    assert!(!g.outgoing("old"));
    assert!(g.outgoing("new"));
}
#[test] fn applied_text_is_not_sent_back() {
    let mut g = Guard::new(None);
    assert!(g.incoming("from B"));
    assert!(!g.outgoing("from B"));
}
#[test] fn the_same_copy_is_sent_once() {
    let mut g = Guard::new(None);
    assert!(g.outgoing("a"));
    assert!(!g.outgoing("a"));
    assert!(g.outgoing("b"));
    assert!(g.outgoing("a"), "copying a again after b is a new copy");
}
#[test] fn empty_text_is_never_sent() {
    let mut g = Guard::new(None);
    assert!(!g.outgoing(""));
    assert!(!g.incoming(""));
}

// wire.rs
#[test] fn awkward_text_round_trips_on_one_line() {
    for text in ["a\r\nb", "line1\nline2\n", "🦀 ünïcode", r#"{"type":"bye"}"#, "nul\u{0}here"] {
        let l = line(&Wire::Clip { format: "text".into(), text: text.into() });
        assert_eq!(l.matches('\n').count(), 1);
        let back: Wire = serde_json::from_str(l.trim_end()).unwrap();
        assert_eq!(text_of(&back), Some(text));
    }
}
#[test] fn a_format_other_than_text_is_ignored() {
    let w: Wire = serde_json::from_str(r#"{"type":"clip","format":"png","text":""}"#).unwrap();
    assert_eq!(text_of(&w), None);
}
#[test] fn an_unknown_message_is_not_fatal() {
    let w: Wire = serde_json::from_str(r#"{"type":"from_the_future"}"#).unwrap();
    assert!(matches!(w, Wire::Unknown));
}
#[test] fn the_cap_is_one_megabyte_of_utf8() {
    assert!(fits(&"a".repeat(MAX_TEXT_BYTES)));
    assert!(!fits(&"a".repeat(MAX_TEXT_BYTES + 1)));
    assert!(!fits(&"é".repeat(MAX_TEXT_BYTES / 2 + 1)), "measured in bytes, not chars");
}
#[test] fn debug_never_shows_clipboard_text() {
    let w = Wire::Clip { format: "text".into(), text: "hunter2".into() };
    assert!(!format!("{w:?}").contains("hunter2"));
}
```

- [ ] **Step 2: Run them to verify they fail.** `cargo test clipboard::` fails to compile.
- [ ] **Step 3: Implement** the types and functions above. Use `sha2` for the guard's hashes; it is already a dependency.
- [ ] **Step 4: Verify.** `cargo test clipboard::` passes, and so do the backend checks.
- [ ] **Step 5: Commit.** "Add clipboard wire messages and the loop guard"

---

### Task 3: Pinned mutual TLS

**Files:**
- Create: `src-tauri/src/clipboard/tls.rs`
- Modify: `src-tauri/Cargo.toml` (add `rcgen = "0.13"` under `[dev-dependencies]`, with a comment saying why: certificates for the handshake tests)

**Interfaces:**
- Produces:
  ```rust
  /// A's side: present `identity`, accept only a server whose leaf cert DER equals `host_cert_pem`'s.
  pub fn client_config(identity: &crate::moonlight::identity::ClientIdentity, host_cert_pem: &str)
      -> Result<Arc<rustls::ClientConfig>, String>;
  /// B's side: present Sunshine's cert and key, require a client cert whose DER equals one of `paired_pems`.
  pub fn server_config(server_cert_pem: &str, server_key_pem: &str, paired_pems: &[String])
      -> Result<Arc<rustls::ServerConfig>, String>;
  /// SNI name to pass to the connector; ignored by the pinned verifier.
  pub const SERVER_NAME: &str = "dusk-clipboard";
  ```
  Write two verifiers: `PinnedServer` (`rustls::client::danger::ServerCertVerifier`) and `PinnedClients` (`rustls::server::danger::ClientCertVerifier`, with `client_auth_mandatory` true). Each compares the end-entity DER byte for byte and ignores names and validity dates, because Sunshine's certs are self-signed with no SAN. Signature checks go through `rustls::crypto::verify_tls12_signature` and `verify_tls13_signature`, using the ring provider's algorithms, so the peer still has to prove it holds the key. `state.rs:122` has the trait boilerplate shape (`AcceptAnyServerCert`). Don't reuse that verifier, because it accepts anything.

- [ ] **Step 1: Write the failing tests.** Run the handshake over `tokio::io::duplex` with `tokio_rustls`, and add `tokio-rustls` as a normal dependency on the ring provider if it isn't already reachable. Each test generates certs with `rcgen::generate_simple_self_signed`.

```rust
#[tokio::test] async fn pinned_pair_completes_the_handshake() { assert!(handshake(&host, &client, &[client.cert]).await.is_ok()); }
#[tokio::test] async fn an_unpaired_client_is_refused() { assert!(handshake(&host, &stranger, &[client.cert]).await.is_err()); }
#[tokio::test] async fn a_host_with_a_different_cert_is_refused() { /* client pins host.cert, server presents other_host */ assert!(…is_err()); }
#[tokio::test] async fn a_client_without_a_cert_is_refused() { /* client config with no client auth */ assert!(…is_err()); }
```
`handshake(server, client, paired)` is a test helper. It builds both configs from the functions above, runs both sides concurrently, and returns `Ok` only if both complete and one `Wire::Hello` line crosses each way.

- [ ] **Step 2: Run them to verify they fail.** `cargo test clipboard::tls` fails to compile.
- [ ] **Step 3: Implement** both config builders and both verifiers.
- [ ] **Step 4: Verify.** The four tests pass, and so do the backend checks.
- [ ] **Step 5: Commit.** "Pin both certificates on the clipboard connection"

---

### Task 4: B's local clipboard

**Files:**
- Create: `src-tauri/src/clipboard/local.rs`
- Modify: `src-tauri/Cargo.toml` (add `arboard = { version = "3", features = ["wayland-data-control"] }`, with a comment)

**Interfaces:**
- Produces:
  ```rust
  pub trait LocalClipboard: Send {
      fn read_text(&mut self) -> Option<String>;          // None for empty / non-text
      fn write_text(&mut self, text: &str) -> Result<(), String>; // error strings never contain `text`
  }
  pub struct System(arboard::Clipboard);
  impl System { pub fn open() -> Result<Self, String>; }
  impl LocalClipboard for System { … }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Support { Supported, Unsupported(&'static str) }
  pub fn support_for(session_type: Option<&str>, current_desktop: Option<&str>) -> Support;
  pub fn support() -> Support; // reads XDG_SESSION_TYPE / XDG_CURRENT_DESKTOP on Linux, Supported elsewhere
  pub const POLL: Duration = Duration::from_millis(500);
  ```
  The `Unsupported` text is shown in A's drawer as-is: "Clipboard sharing isn't available when this machine runs GNOME on Wayland." On X11, `arboard` hands contents over only while its process lives. That's fine, because `System` lives as long as the host loop.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn gnome_on_wayland_is_unsupported() {
    assert!(matches!(support_for(Some("wayland"), Some("ubuntu:GNOME")), Support::Unsupported(_)));
}
#[test] fn kde_on_wayland_and_gnome_on_x11_are_supported() {
    assert_eq!(support_for(Some("wayland"), Some("KDE")), Support::Supported);
    assert_eq!(support_for(Some("x11"), Some("GNOME")), Support::Supported);
}
#[test] fn an_unknown_session_is_assumed_supported() {
    assert_eq!(support_for(None, None), Support::Supported);
}
#[test] #[ignore = "touches the real clipboard"] fn system_round_trips_text() { /* write "dusk test", read it back */ }
```

- [ ] **Step 2: Run them to verify they fail.** `cargo test clipboard::local` fails to compile.
- [ ] **Step 3: Implement.** `support_for` checks the session type case-insensitively and checks the desktop by splitting on `:`, matching `GNOME` case-insensitively.
- [ ] **Step 4: Verify.** The tests pass, `cargo test clipboard::local -- --ignored` passes on the dev machine, and the backend checks pass on Linux. CI covers Windows and macOS.
- [ ] **Step 5: Commit.** "Read and write the host's clipboard"

---

### Task 5: B's clipboard server

**Files:**
- Create: `src-tauri/src/clipboard/host.rs`
- Modify: `src-tauri/src/sunshine/state_file.rs` (add `server_key_path` next to `server_cert_path:73`, using `configured_path(config, "pkey", "credentials/cakey.pem")`, plus `pub fn paired_certs(state: &Value) -> Vec<String>` built on the existing `devices`/`str_field` helpers and returning each device's `cert`)
- Modify: `src-tauri/src/lib.rs` (call `clipboard::host::spawn(app, state)` where `peer::spawn` is called)

**Interfaces:**
- Consumes: `Wire`, `Decline`, `line`, `text_of`, `fits`, `PORT`, `MAX_LINE` (Task 2); `server_config` (Task 3); `LocalClipboard`, `System`, `support`, `POLL` (Task 4); `Store.clipboard_sharing` (Task 1).
- Produces:
  ```rust
  pub fn spawn(app: AppHandle, state: Arc<AppState>);
  /// Pure admission decision, checked after the TLS handshake.
  pub fn admit(sharing_on: bool, support: &Support, hosting: bool) -> Result<(), Decline>;
  /// One admitted connection: Hello, then relay until either side ends or `still_hosting` turns false.
  pub async fn run<S, C, F, Fut>(stream: S, clipboard: C, still_hosting: F) -> io::Result<()>
  where S: AsyncRead + AsyncWrite + Unpin, C: LocalClipboard, F: FnMut() -> Fut, Fut: Future<Output = bool>;
  ```
  Behaviour of `spawn`: bind `0.0.0.0:47851`. A bind failure is logged once, and then there is no sync on this machine. For each connection, build the server config fresh: read the Sunshine config, the cert and key, and the paired certs from the state file, so that a pairing made after Dusk started is accepted. Before TLS, B always writes exactly one plain line. Normally that is `Hello { version: 1 }`, and then the TLS accept follows. If the key can't be read (a Windows permissions case), the line is `Declined { reason: Unsupported { detail: None } }` and the connection closes. That plain line is the only thing ever sent outside TLS, and it never carries clipboard data. After it, `hosting` means B's own `serverinfo` over HTTP on localhost reports `Activity::Hosting`. A declined client gets one `Declined` line, then the connection closes. `run` reads the baseline with `clipboard.read_text()` before sending `Hello`. Then it polls every `POLL` and sends `Clip` when `Guard::outgoing` allows it and the text `fits`. Incoming `Clip` goes through `Guard::incoming` and then `write_text`. Every 5 s it awaits `still_hosting()`, and on false it sends `Bye` and returns. The clipboard is accessed on a blocking thread (`spawn_blocking`) or behind a `std::sync::Mutex`, because `arboard` is sync. Lines are read with a cap of `MAX_LINE`. A line over the cap closes the connection.

- [ ] **Step 1: Write the failing tests.** Use `tokio::io::duplex`, a `Fake` clipboard (`Arc<Mutex<Option<String>>>` that counts writes), and paused time (`#[tokio::test(start_paused = true)]`) wherever `POLL` or the 5 s recheck matters.

```rust
#[test] fn admission_needs_everything() {
    assert_eq!(admit(true, &Support::Supported, true), Ok(()));
    assert_eq!(admit(false, &Support::Supported, true), Err(Decline::SharingOff));
    assert_eq!(admit(true, &Support::Unsupported("x"), true), Err(Decline::Unsupported { detail: Some("x".into()) }));
    assert_eq!(admit(true, &Support::Supported, false), Err(Decline::NotStreaming));
}
#[tokio::test(start_paused = true)] async fn host_sends_a_new_copy_but_not_the_baseline() { /* fake starts "old"; after Hello + 1 s, no Clip; set "new"; next poll → Clip "new" */ }
#[tokio::test(start_paused = true)] async fn host_writes_what_arrives() { /* send Clip "x" → fake reads "x", 1 write */ }
#[tokio::test(start_paused = true)] async fn host_loop_does_not_echo() { /* send Clip "x"; advance 2 s; the peer side receives no Clip */ }
#[tokio::test(start_paused = true)] async fn host_closes_when_streaming_stops() { /* still_hosting → false; within 6 s the peer reads Bye, then EOF; a later fake change is never sent */ }
#[tokio::test(start_paused = true)] async fn host_skips_an_oversized_copy() { /* fake set to MAX_TEXT_BYTES+1 bytes → no Clip */ }
#[test] fn paired_certs_come_from_the_state_file() { /* fixtures::STATE → contains fixtures::CLIENT_CERT */ }
```

- [ ] **Step 2: Run them to verify they fail.** `cargo test clipboard::host state_file::tests::paired_certs` fails to compile.
- [ ] **Step 3: Implement** `admit`, `run`, `spawn`, `server_key_path` and `paired_certs`.
- [ ] **Step 4: Verify.** The tests pass, and so do the backend checks.
- [ ] **Step 5: Commit.** "Serve the clipboard to the machine streaming from this one"

---

### Task 6: A's side: control-socket messages, client connection, wiring and drawer note

**Files:**
- Modify: `src-tauri/src/session/protocol.rs` (new variants and tests)
- Create: `src-tauri/src/clipboard/client.rs`
- Modify: `src-tauri/src/commands.rs:360-500` (`launch_app`)
- Modify: `src/components/DeviceDrawer.tsx` and `src/types.ts` (the note)

**Interfaces:**
- Produces, in the control protocol:
  - `FromClient::Clipboard { format: String, text: String }` becomes `Effect::Clipboard(String)` when the format is `"text"`, and `Effect::Ignore` otherwise.
  - `ToClient::Clipboard { format: String, text: String }`
  - `ToClient::Notice { text: String }`
  - Give `FromClient`, `Effect` and `ToClient` hand-written `Debug` impls, or wrap the text in a newtype that redacts itself, so clipboard text never reaches a log. Existing tests compare `Effect`s with `assert_eq!`, which needs only `PartialEq`, and a failing assertion would print `Debug`, which is now redacted.
- Produces, in `clipboard::client`:
  ```rust
  pub enum Ended { Refused, Declined(Decline), Closed }
  /// Connect to B, relay until the stream ends (`from_window` closes) or B goes away.
  pub async fn run<S>(stream: S, to_window: crate::session::Control,
                      from_window: mpsc::UnboundedReceiver<String>) -> Ended
  where S: AsyncRead + AsyncWrite + Unpin;
  pub async fn connect(address: &str, config: Arc<rustls::ClientConfig>)
      -> Result<impl AsyncRead + AsyncWrite + Unpin, Ended>;
  pub const UNAVAILABLE_EVENT: &str = "dusk://clipboard-unavailable";
  pub const TOO_BIG_NOTICE: &str = "Too large to share — copies over 1 MB stay on this machine.";
  ```
  `connect` uses `peer::host_part(address)` and `clipboard::PORT`, with a 5 s connect timeout. A refused or timed-out connection gives `Ended::Refused`. It reads B's one plain line (Task 5) with a 5 s timeout. `Declined` gives `Ended::Declined`, and `Hello` is followed by the TLS handshake. `run` relays text from the window to B as `Clip`, after `fits`. Text over the cap is not sent and makes `run` send `ToClient::Notice { text: TOO_BIG_NOTICE }` to the window. A `Clip` from B goes to the window as `ToClient::Clipboard`. A uses no `Guard`, because the fork does its own baseline and echo checks (Task 7). `Declined` gives `Ended::Declined`, and `Bye` or EOF gives `Ended::Closed`.
- Wiring in `launch_app`: on `Effect::Authenticated`, if `store.clipboard_sharing.is_on()`, find the host's pinned cert: the `moonlight::hosts::load(...)` entry whose `uuid == Some(id)`, or else whose `addresses` contain `address`. Load the client identity (`moonlight::identity::load`). If either is missing, log one line and skip. Otherwise create an `mpsc::unbounded_channel::<String>()`, spawn `client::connect` followed by `client::run`, and keep the sender in the effects loop. `Effect::Clipboard(text)` goes into the sender. The sender drops when the effects loop ends, which ends `run`. On `Ended::Declined(Decline::Unsupported { detail })`, emit `UNAVAILABLE_EVENT` with `{ deviceId, message }`. `message` is "Clipboard sharing isn't available when that machine runs GNOME on Wayland." when `detail` is `Some`. B's key-unreadable case uses the same event with "Clipboard sharing isn't available on that machine yet." To tell the two apart, use `Decline::Unsupported`'s `detail`: B fills it from `Support::Unsupported`'s text, and it is `None` for the key case. `Refused`, `SharingOff`, `NotStreaming`, `NotPaired` and `Closed` are logged only.
- Drawer: while a session is up on that device, listen for `UNAVAILABLE_EVENT` and show its `message` as a dim note under the session controls. Clear it when the session ends.

- [ ] **Step 1: Write the failing tests**

```rust
// protocol.rs
#[test] fn a_text_copy_from_the_window_comes_through() {
    let mut c = authed();
    assert_eq!(c.accept(r#"{"type":"clipboard","format":"text","text":"hi"}"#), Effect::Clipboard("hi".into()));
}
#[test] fn a_non_text_copy_is_ignored() { assert_eq!(authed().accept(r#"{"type":"clipboard","format":"png","text":""}"#), Effect::Ignore); }
#[test] fn clipboard_before_the_handshake_closes() { assert!(matches!(Conversation::new("t").accept(r#"{"type":"clipboard","format":"text","text":"x"}"#), Effect::Close(_))); }
// add ToClient::Clipboard and ToClient::Notice to every_outgoing_message_round_trips_as_one_line

// client.rs (duplex; the far end plays B with raw Wire lines; `to_window` from a test Control that exposes its receiver)
#[tokio::test] async fn a_copy_from_the_window_reaches_the_host() { … }
#[tokio::test] async fn a_copy_from_the_host_reaches_the_window() { … ToClient::Clipboard { format: "text", text: "x" } }
#[tokio::test] async fn an_oversized_copy_becomes_a_notice() { … Notice { text: TOO_BIG_NOTICE }, and the host sees no Clip }
#[tokio::test] async fn a_decline_ends_the_run_with_its_reason() { … Ended::Declined(Decline::NotStreaming) }
#[tokio::test] async fn the_run_ends_when_the_stream_does() { drop(sender); assert!(matches!(run.await, Ended::Closed)); }
#[tokio::test] async fn nothing_listening_is_refused_quietly() { /* connect to a port bound and then dropped → Ended::Refused */ }
```
For the test `Control`, add a `#[cfg(test)] pub(crate) fn pair() -> (Control, mpsc::UnboundedReceiver<ToClient>)` next to `Control::detached`.

- [ ] **Step 2: Run them to verify they fail.** `cargo test session:: clipboard::client` fails to compile.
- [ ] **Step 3: Implement** the protocol variants, `client.rs`, the `launch_app` wiring and the drawer note.
- [ ] **Step 4: Verify.** The tests pass, and so do the backend checks and `npm run typecheck`.
- [ ] **Step 5: Commit.** "Relay the clipboard between the stream window and the host"

---

### Task 7: The fork: SDL clipboard in the stream window

**Files (in `../moonlight-qt`, built per the "Building the Dusk Moonlight fork" memory, in the fedora44-build distrobox):**
- Modify: `app/streaming/duskchannel.h/.cpp`
- Modify: `app/streaming/session.cpp` (event loop near `:2046` and `:2060-2070`)

**Interfaces:**
- Consumes: control messages from Task 6: incoming `{"type":"clipboard","format":"text","text":…}` and `{"type":"notice","text":…}`, and outgoing `{"type":"clipboard","format":"text","text":…}`.
- Produces in `DuskChannel`: `void sendClipboard(const QString& text);`, plus new user-event codes `SDL_CODE_DUSK_CLIPBOARD 202` and `SDL_CODE_DUSK_NOTICE 203`. The text travels in `event.user.data1` as a heap `QByteArray*`, which the main loop deletes.

Behaviour:
- Incoming `clipboard` lines are handled on the channel thread. They push `SDL_CODE_DUSK_CLIPBOARD`, and the main loop calls `SDL_SetClipboardText`, because SDL's clipboard must be used on the main thread. The loop also records the text as `m_LastClipboardFromDusk`.
- When the stream starts, the session reads `SDL_GetClipboardText()` into `m_ClipboardBaseline`.
- On `SDL_CLIPBOARDUPDATE` and on `SDL_WINDOWEVENT_FOCUS_GAINED`, the loop reads `SDL_GetClipboardText()` and sends it only if it is non-empty and differs from the baseline, from `m_LastClipboardFromDusk` and from the last text sent. After a send, it updates the last-sent text. It never logs the text.
- `notice` shows the text on the `OverlayStatusUpdate` overlay for 3 s.
- Only while `isConnected()`. A client that Dusk didn't launch does nothing new.

- [ ] **Step 1: Build the fork** in the distrobox. Expected: a clean build.
- [ ] **Step 2: Manual check, Linux to Linux (KDE or X11 host).** Copy on A and paste inside the stream. Copy inside the stream and paste on A. Copy the same thing twice, and confirm one send in Dusk's log, which logs lengths only. Start a stream with something already on A's clipboard, and confirm nothing is sent until a new copy.
- [ ] **Step 3: Manual check of the cap.** Copy about 2 MB of text on A, and confirm the overlay notice appears and B's clipboard is unchanged.
- [ ] **Step 4: Commit in the fork.** "Sync the clipboard with Dusk while streaming". Stage it and let the owner commit, as above.

---

### Task 8: Docs and cross-platform check

**Files:**
- Modify: `docs/architecture.md` (a short "Clipboard while streaming" section: the diagram from the spec, the port, the trust rule)
- Modify: `src-tauri/src/session/protocol.rs` module docs, if Task 6 didn't already describe the new messages
- Modify: `RELEASE_NOTES.md` (one user-facing line, without naming Sunshine or Moonlight)

- [ ] **Step 1: Write the docs above.**
- [ ] **Step 2: Manual matrix.** Check each direction for these pairs: Windows → Linux, macOS → Windows and Linux → macOS. Also check a GNOME Wayland host (expect the drawer note and no sync either way) and a host running the previous Dusk release (expect no note and a normal stream). Record the results in the PR description.
- [ ] **Step 3: Verify.** The backend checks, `npm run typecheck` and CI on all three OSes are green.
- [ ] **Step 4: Commit.** "Document clipboard sharing"
