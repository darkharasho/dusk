# Letting the pointer be a pointer and a mouselook at once

Date: 2026-11-10. Status: **probed on the Bazzite host; recommendation
reversed mid-document and this is the second version.** The first said the
host half was blocked behind a Sunshine fork and that the case it serves was
an edge case. Both were wrong, and the host told us so. Nothing is built yet
beyond the per-app change already on `main`.

## Why

Streaming a fullscreen game from the Mac to the Bazzite host, the camera does
not turn. The cause is not a bug: `free_cursor` is `true` in the store, so
Dusk states `--absolute-mouse` on every launch (`cli.rs:170`), the client
sends absolute pointer positions, and a pointer-locked game ignores them.
Turning the Streaming toggle off fixes the game and costs the clickable
floating handle, because in relative capture there is no pointer position to
hit-test against — `overlayPointerUsable()` is `!isCaptureActive() ||
m_AbsoluteMouseMode` (`mouse.cpp:109`).

That trade-off is real but it is not inherent. It is inherent only to
*choosing the mode once per session*.

## What the rest of the world does

Three strategies exist, and only one solves it.

1. **Know, because you launched it.** Steam Remote Play and GameStream stream
   a particular app, so nothing is detected. Exact, and covers nothing about a
   game entered from a desktop session.
2. **The host reports its cursor, and the client follows.** Parsec. The signal
   is cursor *visibility and lock*, not fullscreen — a video player does not
   hide the cursor and a mouselook game always does.
3. **Make the user toggle.** Moonlight, and so Dusk.

This is also being built in the Sunshine/Moonlight ecosystem, by one author,
as two parallel three-PR sets with incompatible API names:

| Variant | Protocol | Client | Host | State |
| --- | --- | --- | --- | --- |
| A | `moonlight-common-c#45` — `LI_FF_CURSOR_STATE` | `qiin2333/moonlight-qt#283` | `AlkaidLab/foundation-sunshine#1196` | all closed |
| B | `Nonary/moonlight-common-c#3` — `LI_FF_HOST_CURSOR` | `Nonary/moonlight-qt#30` | `Nonary/vibeshine#367` | open |

Upstream has neither: a code search of `moonlight-stream/moonlight-common-c`
returns zero hits for either flag. Variant B is well made — a pure state
machine in `cursormodepolicy.h`, three new test binaries, a hardware pass on
2026-10-27 — and worth reading for one fix in particular: ending an MMO
right-button camera drag swung the camera, because the client returned to
absolute while the button was still held and sent a stale pre-switch position.
Their rule is that the switch *to* relative is immediate and the switch
*back* waits for every button to be up. Steal that rule whatever we build.

**But both variants need a modified Sunshine on the host, and that is the
route this document now recommends against** — not on licence grounds, since
a GPL-3.0 fork at arm's length is what we already do for the client, but
because it would make `SUNSHINE_TAG` a branch we maintain forever.

## The host already runs Dusk, which changes everything

The first version of this document concluded that the host half required
forking Sunshine. That is true only of a *protocol-level* solution, because
Sunshine is what speaks the stream protocol. It is not true of a Dusk-level
one: **Dusk runs on the host too.** That is the premise of the whole project,
and it means the observer is a component we already ship and control.

Probed on the Bazzite host on 2026-11-10, all of it confirmed on hardware:

| Question | Answer |
| --- | --- |
| Compositor | `kwin_wayland`, KWin 6.8.2, KDE Plasma 6 |
| KWin on the session bus | yes — `org.kde.KWin`, reachable even from a plain SSH shell |
| Can an observer be loaded | yes — `org.kde.kwin.Scripting`: `loadScript`, `start`, `unloadScript` |
| Can it report out | yes — `callDBus` to a listener owning a well-known name |
| Is Dusk positioned to do it | **yes** — `app-Dusk@autostart.service`, a user unit in the graphical session, not a Flatpak, so no sandbox to cross |
| gamescope | installed at `/usr/bin/gamescope` |

So every hop exists. KWin script → Dusk-on-host over D-Bus → the peer channel
→ client Dusk → `duskchannel` → the mode toggle, which the client log already
proves works (`Detected mouse mode toggle combo`, twice in nine minutes).

Two traps found by probing rather than reading, recorded so nobody repeats
them: `qdbus` is **not** installed on Bazzite, so use `busctl`; and KWin
script `print()` goes to a Qt logging category that is off by default, with
no `qtlogging.ini` on the host — a working script is completely silent. Both
cost a round trip. `callDBus` to a listener is the reliable channel, and it is
what the real implementation wants anyway.

## What the host actually reports

The probe hooked `windowActivated` and `fullScreenChanged` and logged the
active window. The whole captured run:

```
13:24:56 Steam                          | class=steam                    | fullScreen=false
13:25:01 SAI                            | class=sai                      | fullScreen=false
13:25:09 Pixel Chaos: Tidy Up the Museum | class=steam_app_5149240        | fullScreen=true
13:25:35 SAI                            | class=sai                      | fullScreen=false
13:27:35 Discord                        | class=com.discordapp.Discord   | fullScreen=false
13:27:42 SAI                            | class=sai                      | fullScreen=false
```

Three findings, in order of how much they change the design.

**The class is a positive game identifier, not a generic one.**
`steam_app_5149240` is the Steam app id. The first version of this document
worried that fullscreen alone over-triggers — a fullscreen video player taking
the pointer away. With the class in hand that mostly dissolves: a browser
reports `firefox`, Discord reports `com.discordapp.Discord`, a Steam game
reports `steam_app_<id>`. The rule is `fullScreen=true` **plus** a deny-list
of known non-games, with `steam_app_*` as a high-confidence fast path. Not an
allow-list — see the limits below.

**The trigger is one event, not two.** `activated` already carried
`fullScreen=true`. There is no race between learning about focus and learning
about fullscreen, and no need to correlate them.

**Focus churns constantly, so the trigger must be the fullscreen edge.** Four
of the six events above are focus moving between non-fullscreen windows:
Steam to SAI in five seconds, Discord to SAI twice in twenty-three. A watcher
keyed on "focus changed" would have flapped repeatedly while doing nothing
useful. Keyed on "is the active window a fullscreen game", every one of those
is correctly ignored.

That last point also simplified the policy. The probe cannot distinguish the
game quitting from the game merely losing focus — it reports the newly
activated window, not window removal. It does not matter: the question is only
ever *is the active window a fullscreen game*, so focus leaving a game that is
still running should free the pointer exactly as quitting it would. No window
lifetime tracking, no `windowRemoved` hook.

## Why this is the primary case, not an edge case

The first version of this document shipped the per-app change — `free_cursor`
on `DeviceStream`, defaulted from the launched app name — and listed, under
what it did not settle, whether the alt-tab case mattered, saying that if it
turned out to be how the Bazzite host is used then the recommendation
inverts.

It is how the host is used. The live launch during this session was:

```
Moonlight stream 192.168.50.146 Desktop --resolution 3440x1440 ... --absolute-mouse
```

Desktop, then a fullscreen game entered from inside it. The per-app change
does nothing for that, because the app Dusk launched *is* the desktop and
`free_cursor_for("Desktop", None, true)` is correctly `true`. It helps only
when a game is launched as a game from the app list. The manual toggle was
pressed twice in nine minutes to compensate.

So the per-app change is not wasted — it makes the semantics honest and the
app-list path correct — but it addresses the secondary path. This addresses
the primary one.

## Design

1. **Observer.** Dusk-on-host loads a KWin script at startup via
   `org.kde.kwin.Scripting.loadScript` + `start`, and unloads it on exit. The
   script hooks `windowActivated` and `fullScreenChanged` and reports the
   active window's `resourceClass` and `fullScreen` back over `callDBus`.
2. **Classify.** `fullScreen=true` and the class not in a deny-list
   (browsers, video players, terminals, Discord) means pointer-locking. A
   `steam_app_*` class is a fast path to the same answer.
3. **Carry.** A new `Wire` variant on the peer channel. The protocol is
   already built for this: `#[serde(other)] Unknown` means an older Dusk
   ignores an unknown message rather than erroring, and `TooBig` is the
   precedent. **Generalise the channel first** — it only runs when clipboard
   sharing is on (`store.clipboard_sharing.is_on()`, `commands.rs:528`), and a
   channel named for one of its payloads silently disabling the mouse is a
   bug waiting to be filed.
4. **Apply.** A `mouse_mode` message over `duskchannel`, handled like
   `host_display`, calling the same path as `KeyComboToggleMouseMode`.
5. **Hysteresis.** Switch into relative immediately; switch back only once
   every mouse button is up, per variant B's MMO bug. Log every switch. Show
   the current mode in the overlay. Keep `Ctrl+Alt+Shift+M` as an override
   that wins until the session ends.
6. **Opt-in**, at least until it has been wrong a few times and been taught
   not to be.

## What this does not settle

- **A fullscreen non-game was never measured.** The deny-list is reasoning,
  not evidence. A fullscreen YouTube is one event away and should be captured
  before the deny-list is written.
- **gamescope was never observed in the path.** It is installed, and this
  launch did not use it — KWin saw the game's own window. A gamescope-wrapped
  launch presumably presents gamescope as the fullscreen window, which may be
  a *better* signal, but nobody has looked.
- **Non-Steam games are unknown.** Native, Lutris and Heroic launches report
  some other class. This is why the rule cannot be an allow-list of
  `steam_app_*`.
- **Only KDE Plasma was probed.** The observer is KWin-specific. A GNOME or
  wlroots host needs its own, or needs to say it is unsupported and why —
  the precedent being the GNOME Wayland host that turns clipboard sharing off
  and explains itself.
- **The cursor hide/lock signal remains unavailable.** Pointer constraints are
  a client↔compositor protocol with no third-party introspection, and KWin
  does not surface them. So this stays a fullscreen heuristic, and will
  eventually be wrong about something, which is the whole argument for the
  override and the visible state.
- **The latency of the full loop is unmeasured.** KWin reported promptly by
  eye, but nobody has timed observer → D-Bus → TLS → `duskchannel` → toggle
  against how fast a hand reaches the mouse after a game takes focus.
- **The local-cursor latency win is not available this way.** Variant B draws
  the host cursor client-side with no video round trip, which is a real
  desktop-streaming improvement that a fullscreen-classifier gets us nothing
  of. It remains the strongest argument for eventually paying the host-fork
  cost, and it is a separate decision from this one.
