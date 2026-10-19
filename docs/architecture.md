# Dusk as the only visible program

Where Dusk is going: one app that handles hosting and streaming end to end,
with Sunshine and Moonlight as implementation details nobody is introduced to.
This document is the shape of that, the reasoning behind each decision, and
what it deliberately does not try to be.

It is a design document, not a record of what is built. The phase table at the
end says where the work actually stands.

## The constraint everything else bends around

Sunshine and moonlight-qt — and moonlight-common-c underneath it — are
**GPL-3.0**. Dusk's shell is not. Linking any of them into Dusk relicenses
Dusk, and `moonlight-common-c` is not LGPL, so there is no quiet way around
it.

So invisibility cannot come from embedding. It has to come from
**process-level integration**: three programs, one face.

That is not the compromise it first looks like. Parsec is a single process
because it owns its protocol; Dusk is three because it borrows two mature
ones and gets a working product years sooner. The person using it never needs
to know the difference. But every decision below exists because that boundary
is load-bearing and has to stay real: pipes and sockets are fine, linking and
shared in-memory structures are not.

## Target shape

```
Dusk.app / Dusk.exe / dusk              the only thing anyone sees
├─ dusk                shell: grid, orchestration, IPC hub   (Dusk's licence)
├─ Helpers/
│  ├─ DuskStream       moonlight-qt fork, renamed            (GPL-3.0)
│  └─ DuskHost         Sunshine, service-registered          (GPL-3.0)
└─ Resources/licenses/ GPL text and a written source offer for both
```

Three binaries, one visible application. This is mere aggregation on a
distribution medium, so Dusk's own licence is unaffected; the obligation is to
carry each GPL component's licence text and an offer of its corresponding
source. Both forks are public, so that is a link and a text file rather than a
burden.

Note that this **reverses** the reasoning currently written into
`install/release.rs`, which avoids bundling Sunshine specifically to stay out
of conveying obligations. That dodge costs the single-installer goal, so the
trade is deliberate and should be made with eyes open: accept conveyance, gain
one installer.

## Layer 1 — Host: a daemon nobody is introduced to

Much of this is already built. Sunshine's configuration is driven through its
local API, its tray icon is turned off at sign-in, and PINs are accepted
natively. What remains:

**Provision the credential automatically.** Today `SelfCard` asks the person
to sign in to Sunshine, which is Sunshine leaking directly through Dusk's own
UI. Instead, at install time Dusk generates a random username and a
high-entropy password, sets them with `sunshine --creds <user> <pass>`, stores
them in the OS keystore, and never mentions them again. The sign-in panel, the
`SignIn` step in `install/steps.rs`, and the whole "Checking sign-in" state
delete themselves. `CredentialStore` does not change — it simply gets filled
by the installer rather than by a person.

**Bundle it and own its lifecycle.** Ship Sunshine, register the service at
install, start and stop it from Dusk. The runtime download goes away, and with
it the risk of a Sunshine whose configuration keys no longer match
`src/sunshineSchema.ts`.

**Suppress the remaining surfaces.** `system_tray=disabled` already covers
both the menu bar item and Sunshine's desktop notifications. Beyond that: do
not install Sunshine's own application bundle or Start Menu shortcut, and
never open its web UI.

**Keep the line on the virtual display.** Parsec installs a kernel-mode
driver silently. Dusk should not, and the existing refusal to do so is a good
line — a kernel driver from another project, outside the release Dusk can
verify, is not something to install on someone's behalf without asking. Make
it one consented click. This is the one seam worth leaving visible.

## Layer 2 — Client: a stream window that says Dusk

**Identity.** Rename the fork's bundle identifier, display name, icon, window
title, and `WM_CLASS` on Linux. This is also the correct move for trademark
reasons: a fork shipping under the Moonlight name invites exactly the
confusion the project does not want.

**No second dock or taskbar entity.** `LSUIElement` on macOS, tool-window
styles on Windows so it claims no taskbar button, the skip-taskbar hint on
Linux. The stream window should appear as though Dusk opened it, because
effectively it did.

**Window ownership is the main fork in the road:**

- **(a) A separate borderless window, choreographed.** Dusk positions and
  raises it, hides itself on connect and restores on disconnect. Works
  everywhere, Wayland included, and gets most of the way to the feel.
- **(b) True embedding** — reparent the helper's native surface into a Dusk
  window (`NSView`, `SetParent`, XEmbed). Pixel-perfect, but **Wayland cannot
  do it at all**, and it is fragile on the other two.

Take (a). Revisit (b) as Windows-only polish if the seam turns out to show.

## Layer 3 — The control plane

Today Dusk drives the client with one-way command line arguments, and ending a
session means invoking a second process. That ceiling is the reason the
overlay is a hotkey rather than a user interface.

Replace it with a **local socket** — a unix domain socket on macOS and Linux,
a named pipe on Windows. JSON lines, Dusk is the server, and the helper
connects on launch carrying a one-time token passed in argv so that nothing
else on the machine can drive it.

```
helper → dusk   ready
                stats {fps, bitrate, rtt, decodeMs, loss}
                overlay_action {disconnect | settings | quality}
                disconnected {reason}

dusk → helper   set_quality {bitrate, fps, resolution}
                disconnect
                show_overlay
                host_display {id}
```

This is what turns the floating handle into a real control rather than a
shortcut: the overlay raises an intent, **Dusk decides**, and Dusk commands the
helper. It also strengthens the licence boundary rather than weakening it — a
documented protocol spoken between two separate programs is the textbook
aggregation case, and considerably more defensible than scraping a CLI.

## Layer 4 — Zero-click pairing

The single most "two programs" moment left, and the one Dusk is uniquely
placed to remove: **Dusk runs on both ends.**

Clicking Connect on an unpaired machine should have Dusk generate the PIN and
ask the remote Dusk to accept it through the `accept_pin` path that already
exists, completing the pairing with no PIN ever shown to anyone.

This needs a small Dusk-to-Dusk channel and a first-contact trust decision —
nothing on the local network should be able to pair itself to you unattended.
The effort-to-delight ratio is the best of anything in this document.

## Layer 5 — One updater

Dusk updates itself through the Tauri updater and owns the pinned versions of
both helpers, with a compatibility matrix per Dusk release. The failure being
guarded against is a Sunshine whose configuration keys have moved out from
under `sunshineSchema.ts` — the schema degrades gracefully by design, but a
host that silently lost half its settings screen is still a bad day.

## What this deliberately is not

The project rules out accounts, rendezvous services, and NAT traversal. So the
*experience* here can be Parsec; the *reach* cannot. It stays LAN or your own
VPN.

This is worth stating plainly rather than discovering late, because "the full
Parsec experience" normally includes "works from anywhere", and that part is
pure infrastructure the project has said it will not run. Everything in this
document is achievable without revisiting that decision. If the reach ever
matters more than the non-goal, the honest way to get it without operating
servers is to lean on something like Tailscale rather than to build a relay —
but that is a different conversation, and it should be had explicitly.

## Phases

Ordered so that each one ships something usable on its own.

| # | Phase | Payoff | State |
| --- | --- | --- | --- |
| 0 | Bundle the fork, fix the client search path | The overlay works in release builds | **done** |
| 1 | Rename and identity | The stream stops announcing "Moonlight" | **done** |
| 2 | Control socket; overlay actions; live stats | The overlay becomes a real interface | not started |
| 3 | Bundle Sunshine, auto-provision credentials, delete the sign-in UI | Sunshine leaves the UI entirely | not started |
| 4 | One installer, service registration, consented display driver | First run is effortless | not started |
| 5 | Zero-click pairing | Connecting stops involving a PIN | not started |
| 6 | Window choreography, and no second dock entry | The seam closes | not started |

Phase 0 is verified on all three platforms: a tagged client release is built
by the fork, downloaded and inserted during Dusk's release, and the resulting
macOS bundle passes notarization with the client nested inside it.

Suppressing the second dock and taskbar entry moved from phase 1 into phase 6.
It is `LSUIElement` on macOS and window-style changes elsewhere, and a
UIElement app has different focus and input-grab behaviour — which is exactly
what a fullscreen game stream depends on. It belongs where it can be tested
against a real session rather than reasoned about.

## Risks worth pricing now

- **Notarizing nested helpers.** Inside-out signing, hardened runtime on each
  nested binary. This got fiddly before it got reliable, and it gates every
  macOS release once the helpers ship. The hard-won parts are written up at
  the top of `scripts/package-macos.sh`: Tauri's resource copier flattens the
  symlinks a Qt framework is built from, a bundle's main executable cannot be
  signed on its own, and `codesign` reads standard input, which silently ends
  the loop that is signing everything else.
- **Artifact size.** Roughly 71 MB for the client helper and 100 MB for
  Sunshine, on top of Dusk. Installers go from about 10 MB to around 200 MB.
- **Fork maintenance.** Dusk now carries a moonlight-qt fork against upstream
  drift, indefinitely. The overlay commits are small; keeping them cleanly
  rebasable is a standing cost that is easy to under-budget.
- **Sunshine's configuration API.** It is explicitly the thing nothing else
  depends on holding still, and bundling means shipping against a pinned
  version. Pin hard, and re-test the settings screen on every bump.
