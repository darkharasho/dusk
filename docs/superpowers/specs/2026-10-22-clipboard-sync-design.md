# Copy and paste across a stream

Date: 2026-10-22. Status: design, not built.

## Why

Streaming to another machine should feel like using it, and today the
clipboard breaks that. Nothing copied on the host reaches the machine you are
streaming from. The stock client's only help goes the other way: a shortcut
that types out the local clipboard on the host as keystrokes, which only works
for text and is slow. Sunshine has no clipboard channel at all.

Dusk already runs on both ends of a stream and already talks Dusk to Dusk
(the peer port, `src-tauri/src/peer.rs`). It also talks to the stream client
over the per-session control socket (`src-tauri/src/session/`). So clipboard
sync can be built entirely out of Dusk's own pieces. It does not depend on
Sunshine or Moonlight, and it can behave the same on Windows, macOS and
Linux.

## Goals

- While a stream is running from machine A (the one you sit at) to machine B
  (the host), the two clipboards stay in sync in both directions. Copy inside
  the stream and you can paste on A. Copy on A and you can paste inside the
  stream.
- Text, in this first step. Images come in a second step (see below).
- Works whichever pair of Windows, macOS and Linux is on the two ends.
- Clipboard contents never cross the network unencrypted, and only reach
  the machine that is actually streaming to this one.
- Sync stops when the stream ends.

## Non-goals

- Syncing clipboards between machines when no stream is running, or among
  more than the two machines in a stream. (The owner asked for streaming
  only.)
- Images, in this step. The stream window's clipboard support only handles
  text, and its Qt event loop is suspended while streaming
  (`duskchannel.h`), so images on A need native clipboard code per OS in the
  fork (Win32, NSPasteboard, X11, Wayland). That is the planned second step.
  The protocol should leave room for it.
- Files, rich text/HTML, and multiple formats per copy.
- Persisting a copy on A after the stream window closes. GNOME and KDE
  already keep clipboard contents after the app that made them exits, so
  nothing extra is built for this (my call).
- Clipboard history.
- Changing the stock client's type-the-clipboard shortcut. It stays as it is.

## Anti-goals

- A clipboard ending up on any machine other than the one at the other end
  of the current stream.
- Sending clipboard contents over the network unencrypted. The peer port
  sends everything unencrypted today. That is an accepted risk for a pairing
  PIN, but not for clipboards, which often hold passwords.
- Sync loops: the same contents bouncing back and forth, or each side
  overwriting a newer copy with an older one.
- Pasting something on A that you copied on B ten minutes and two streams
  ago. Stale contents should not appear after a reconnect.
- Naming Sunshine or Moonlight in the UI (project rule).

## How it fits together

```
 A (client)                                   B (host)
 ┌───────────────┐  control socket  ┌──────┐  encrypted peer   ┌──────┐
 │ stream window │ ───────────────▶ │ Dusk │ ────────────────▶ │ Dusk │ ⇄ B's clipboard
 │ (the fork)    │ ◀─────────────── │      │ ◀──────────────── │      │
 └───────────────┘                  └──────┘                   └──────┘
   ⇄ A's clipboard
```

**On A, the stream window does the clipboard work, not Dusk.** It has focus
while you are streaming, so it can read and write the clipboard on every
platform, including Wayland compositors that stop background apps from
reading it (GNOME in particular). When the clipboard changes, or when the
window regains focus (the moment you come back from copying something on A),
the window sends the contents to Dusk over the control socket. Dusk sends it
anything that arrives from B, and the window puts it on A's clipboard. This
lives in the moonlight-qt fork (`../moonlight-qt`, `app/streaming/duskchannel.*`),
as new message types on the existing control protocol
(`src-tauri/src/session/protocol.rs`). Policy stays in Dusk: the window
reports and applies, and Dusk decides whether sync is on.

**Between machines,** A's Dusk opens a clipboard connection to B's Dusk when
a stream starts and closes it when the stream ends. One long-lived connection
per stream, rather than the peer port's usual one request per connection, so
changes flow both ways without polling. It could be a new request type on the
peer port that upgrades to a long-lived connection, or its own port. That is
the builder's call.

**On B,** Dusk watches the clipboard and writes to it directly. It runs as
the signed-in user in the session being streamed, so it sees the same
clipboard you do inside the stream.

## Trust and encryption

- The clipboard connection is TLS with both sides presenting a certificate,
  built from the certificates that pairing already put in place. A presents
  its client identity, which Dusk already holds a copy of
  (`src-tauri/src/moonlight/identity.rs`). B presents Sunshine's server
  certificate. Each side checks the other's against what it recorded at
  pairing time. My suggestion is that this needs no new keys. If B's Dusk
  can't read Sunshine's server key on some platform, the builder can propose
  something else, such as a Dusk-owned key exchanged during pairing. Either
  way it must be pinned, and it must not be trust-on-first-use.
- B accepts a clipboard connection only from a client that is streaming from
  it right now, and ends it when that stream ends. How B knows a stream is
  live (Sunshine's API, A telling it, or both) is the builder's call, but
  being a paired client must not be enough on its own.
- Nothing is written to disk. Clipboard contents are never logged.
- An older Dusk on the other end that doesn't know the request declines it,
  and the stream works as before without sync.

## Behaviour

- **Direction:** both ways, for the whole stream.
- **Formats:** plain text (UTF-8) only. Other formats are ignored. Message
  shapes carry a format field so images can be added later without a
  protocol break.
- **Size cap:** 1 MB of text per copy (my call; 16 MB is meant for images
  later). Anything larger isn't sent, and
  the in-stream overlay says so briefly. It doesn't fail silently.
- **Loops:** each update carries an origin and a sequence number or content
  hash. A side doesn't re-send what it just applied. Last writer wins.
- **Stale contents:** when a stream starts, nothing is sent until someone
  copies something. Clipboard state from an earlier stream isn't sent again.
- **Setting:** one global toggle per machine, "Share clipboard while
  streaming", on by default (my call). Sync happens only when it is on at
  both ends. It goes on the Settings page next to the other streaming options
  (`src/components/StreamingSection.tsx`), not in the device drawer (my call).
- **Feedback:** no notification on every copy. At most, a small indicator in
  the overlay that the clipboard is shared, plus the oversize message above.

## Known platform edge

When **B is Linux on a GNOME Wayland session,** a background Dusk may have no
reliable way to notice that the clipboard changed. Writing to it is easier
than reading it. The builder should test this early, on GNOME and on
KDE/wlroots compositors, which do support the data-control protocol. On
GNOME, writing in the background may be blocked too, since it normally
needs focus or a data-control protocol that GNOME doesn't offer. The owner's
decision: if it can't be done reliably on a GNOME Wayland host, sync is
turned off for that host, and the app says why rather than leaving it looking
broken. One-way sync is fine where it works. No GNOME Shell extension, XWayland
bridge or other workaround in this step.
Windows, macOS and X11 have no such limit.

## Left to the builder

- The clipboard library on B (e.g. `arboard` with Wayland support). On A,
  the fork uses SDL2's text clipboard and how B
  watches for changes (polling vs. platform notifications).
- Message shapes on the control socket and the peer connection, following
  the existing newline-JSON style, plus how binary image data is framed.
- Whether the clipboard connection is on the peer port or its own.
- How B confirms a stream is live.
- Tests. At minimum: protocol round-trips, the loop guard, the size cap,
  rejection of an unpaired or not-streaming client, and the old-Dusk
  fallback.
