# Letting the pointer be a pointer and a mouselook at once

Date: 2026-11-10. Status: **evaluation, nothing built**. The recommendation
is to do the small deterministic half now and *not* port the protocol work
yet, for reasons that are about who owns the host rather than about the code.

## Why

Streaming a fullscreen game from the Mac to the Bazzite host, the camera does
not turn. The cause is not a bug: `free_cursor` is `true` in the store, so
Dusk states `--absolute-mouse` on every launch (`cli.rs:170`), the client
sends absolute pointer positions, and a pointer-locked game ignores them.
Turning the Streaming toggle off fixes the game and costs the clickable
floating handle, because in relative capture there is no pointer position to
hit-test against — `overlayPointerUsable()` is `!isCaptureActive() ||
m_AbsoluteMouseMode` (`mouse.cpp:109`).

So the two things Dusk offers are, today, exclusive by construction:

| | pointer leaves ON | pointer leaves OFF |
| --- | --- | --- |
| Camera turns in a game | no | yes |
| Overlay clickable | yes | yes, after the hotkey |
| Handle clickable with no keypress | yes | no |
| Pointer can reach another screen | yes | no |

The question this doc answers is whether that exclusivity is inherent. It is
not. It is inherent only to *choosing the mode once per session*.

## What the rest of the world does

Three strategies exist, and only one of them actually solves it.

1. **Know, because you launched it.** Steam Remote Play and GameStream stream
   a particular app, so nothing has to be detected. Cheap and exact, and it
   covers nothing about a game entered from a desktop session.
2. **The host reports its cursor, and the client follows.** Parsec. The
   signal is cursor *visibility and lock*, not fullscreen. This is the real
   answer, and the important part is which signal it is: a fullscreen video
   player does not hide its cursor, and a mouselook game always does, so
   hide/lock separates the two cases that `_NET_WM_STATE_FULLSCREEN` cannot.
3. **Make the user toggle.** Moonlight today, and so Dusk today.

An earlier draft of this design proposed building (2) ourselves: a host-side
watcher reading `_NET_WM_STATE_FULLSCREEN`, a new `Wire` variant on the peer
channel, and a `mouse_mode` message over `duskchannel`. That was wrong twice
over — wrong signal, and wrong layer. The signal belongs *in the stream*,
because the host already knows and the stream already connects the two ends.

## It is already built, twice, and merged nowhere

One author (`jtsorlinis`) has implemented this across both ends, as two
parallel three-PR sets with incompatible names:

| Variant | Protocol | Client | Host | State |
| --- | --- | --- | --- | --- |
| A | `moonlight-common-c#45` — `LI_FF_CURSOR_STATE`, `LiSetCursorStateCallbacks`, `LiSendCursorControl` | `qiin2333/moonlight-qt#283` | `AlkaidLab/foundation-sunshine#1196` | **all closed** |
| B | `Nonary/moonlight-common-c#3` — `LI_FF_HOST_CURSOR`, `LiSetHostCursorCallbacks`, `LiSendHostCursorControl` | `Nonary/moonlight-qt#30` (+1983/-16, 24 files) | `Nonary/vibeshine#367` (+2712/-3, 21 files) | **open** |

Upstream has neither: a code search of `moonlight-stream/moonlight-common-c`
returns zero hits for `LI_FF_HOST_CURSOR` and zero for `LI_FF_CURSOR_STATE`.

The work itself is not rough. Variant B carries a pure state machine in
`cursormodepolicy.h` with the constants in one place, a pure
`hostcursormath.h` for the stream↔window mapping, three new test binaries
covering recentring detection, clip classification, tick wraparound and
held-button deferral, and a hardware pass on 2026-10-27 (macOS client against
a Windows 11 vibeshine service) reporting no flapping and ~5 µs per host
poll. It also records a bug worth stealing the fix for: ending an MMO
right-button camera drag swung the camera, because the client returned to
absolute while the button was still held and sent a stale pre-switch
position. Their rule is that the switch *to* relative is immediate but the
switch *back* waits for every button to be up.

### The wire, briefly

Version 2 of the existing `0x5509` message, routed by its first byte, on
reliable channel `CTRL_CHANNEL_CURSOR` (0x09). States are 44 bytes — flags,
sequence, shape id, reference size, 16.16 scale, position, clip. Shapes come
as a 28-byte header plus up to 16 KiB of BGRA per chunk, reassembled to at
most 256×256. v2 is only interpreted from a host that advertised the flag and
only with a callback registered.

### Why it degrades safely

This is the one property that makes the client half thinkable at all. With no
flag advertised, nothing subscribes and nothing is sent; the mouse uses the
existing setting exactly as today. Stock Sunshine, Apollo and Vibepollo all
fall in that bucket. So porting the client cannot regress a stock host — it
simply does nothing until a host answers.

## The thing that actually blocks it

**The host half is a Sunshine fork.** Dusk runs Sunshine as a separate,
unmodified process on purpose, and that decision is load-bearing in two
directions: it keeps GPL-3.0 off the shell, and it means the host is somebody
else's maintenance problem. Adopting automatic mouse mode means one of:

- **Wait for upstream.** Costs nothing, may never happen. Nothing is merged
  in `LizardByte/Sunshine` or in upstream `moonlight-common-c`, and variant A
  being closed shows the design has already churned through one naming round.
- **Ship a forked host.** Licence-wise this is fine — a GPL-3.0 fork at
  arm's length is exactly what we already do for the client. The cost is not
  legal, it is that Dusk would own tracking upstream Sunshine forever, and
  the installer would carry a host that is no longer the one users can read
  release notes for. `SUNSHINE_TAG` stops being a pin and becomes a branch.
- **Port the client half only.** Legal, safe, degrades to today's behaviour,
  and does nothing at all until the host side exists somewhere. Pure
  speculation on an unmerged protocol whose names have already changed once.

That is the whole decision, and it is not a technical one.

## Recommendation

**Do the deterministic half now.** Dusk already knows which app it is
launching — `stream_args(host, app, options)` takes the app name and passes
it through (`cli.rs:143`), and the tests already exercise
`stream_args("host", "Desktop", &held)`. `DeviceStream` (`store.rs:137`)
already models per-device overrides for resolution, bitrate, fps and codec.
So:

1. Add `free_cursor: Option<bool>` to `DeviceStream`.
2. Default it from the app: Desktop keeps the pointer, anything else gets
   relative capture.
3. Expose it beside the existing per-device stream settings.

That covers launching a game *as* a game, which is the common case, with no
detection, no heuristics and nothing to flap. `Ctrl+Alt+Shift+M` stays the
manual escape hatch for a game entered from a desktop session — it is already
enabled in the fork (`input.cpp:103-106`) and already toggles
`m_AbsoluteMouseMode` live.

**Do not port the protocol yet.** Watch `Nonary/moonlight-qt#30` and
`Nonary/vibeshine#367`. Revisit when either upstream `moonlight-common-c`
takes the flag, or the alt-tab-into-a-game case proves common enough in
practice to justify owning a Sunshine fork. Deciding that now would be
guessing at evidence we do not have.

## What this does not settle

- Whether the alt-tab case matters. Nobody has counted. If it turns out to be
  how the Bazzite host is actually used, the recommendation inverts.
- Whether the macOS client can even hold relative mode reliably.
  `setCaptureActive` falls back to hiding the cursor and setting
  `m_FakeMouseCaptureActive` when `SDL_SetRelativeMouseMode` fails
  (`input.cpp:396-400`), which would present as the same dead camera from a
  different cause. Unverified; worth ruling out before blaming a mode.
- The local-cursor latency win. Variant B draws the host cursor client-side
  with no video round trip, which is a real desktop-streaming improvement
  independent of games. Nothing in the deterministic half gets us that, and
  it is the strongest argument for eventually paying the host-fork cost.
