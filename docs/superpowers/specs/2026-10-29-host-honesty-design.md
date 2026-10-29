# Telling the truth about the host that is actually running

Date: 2026-10-29. Status: **built on the Mac this was written on, not yet
run against the Bazzite host.** All four changes compile, `cargo clippy` is
clean and 536 tests pass, but every Linux path here — the version read, the
provenance comparison, the sandbox check — has only been exercised by unit
tests over captured output. It needs a run on a real Linux host before it is
believed.

## Why

A fullscreen game streamed from the Bazzite machine has no working mouse.
The cursor moves everywhere else — desktop, windowed games — so nothing
looks broken until you are in a game and aiming at nothing. Chasing it took
an hour and ended somewhere Dusk had already written down and then walked
past twice.

What the Bazzite host turned out to be running, measured this session:

```
sunshine --version   ->  2026.516.143833   (on PATH, no rpm)
flatpak list         ->  dev.lizardbyte.app.Sunshine  2026.914.233613
systemctl --user     ->  sunshine.service, "Sunshine Game Stream Server"
config               ->  'capture' = portal
```

Three separate things are wrong with that picture, and Dusk reports all of
it as a correctly set up host.

**Dusk is not running the Sunshine it ships.** `register_user_unit`
(`install/apply.rs:378`) deliberately leaves an existing
`~/.config/systemd/user/sunshine.service` alone, which is right — it may be
the person's own. But Bazzite's image ships its own Sunshine and its own
unit, so Dusk stepped aside and said nothing. The unit's description is
still "Sunshine Game Stream Server" where Dusk's would read "Sunshine,
managed by Dusk" (`apply.rs:462`). That is the only way to tell from the
outside, and nothing looks.

This is the second time. `docs/pickup.md:14` records the same thing on the
Mac — "a standalone Sunshine 2026.516 from `/Applications`, which Dusk
defers to and which ignores display switches outright; Dusk does not yet
warn about an older installed host." Same version, even. It was written
down as a known gap and left, and it cost the hour above.

**Dusk picked the older of two installed hosts.** `LINUX_UNITS`
(`host/platform.rs:180`) is `["sunshine", "app-dev.lizardbyte.app.Sunshine"]`
and `installed_unit()` takes the first that exists. On this machine that is
the image's 2026.516 — five months older than the 2026.914 Flatpak sitting
right there unused, which is also the exact version `SUNSHINE_TAG` pins. The
order is a hardcoded preference masquerading as a lookup.

**The uinput step passes on a host that cannot use uinput.** `capture =
portal` means input is injected through the XDG RemoteDesktop portal, not
`/dev/uinput` — the `[portalgrab] RemoteDesktop CreateSession` path already
written up at `pickup.md:629`. Dusk's setup step checks only whether *some
udev rule grants the seat `/dev/uinput`* (`rule_present`,
`install/apply.rs:417`), recognises Bazzite's own rule, and marks itself
done (`pickup.md:948`). The rule is real. It is also irrelevant to a host
that never opens the device.

That last one is the suspected cause of the dead mouse: portal-injected
motion moves the compositor's virtual pointer, a normal desktop follows it,
and a fullscreen game holding a Wayland pointer lock ignores it because a
locked surface wants relative motion from a real device. **Not yet
confirmed** — it needs the running host's `ExecStart` and its log checked
for the input path. The three reporting bugs below are worth fixing whether
or not that diagnosis holds, because they are what made it invisible.

## Goals

- When Dusk defers to a Sunshine it did not install, the device drawer says
  so, and says which version, rather than presenting it as Dusk's own.
- When the deferred-to host is older than the pinned `SUNSHINE_TAG`, that is
  called out as the likely explanation for host-side misbehaviour.
- The input setup step reflects whether the *running* host can actually
  inject input, not whether a udev rule exists somewhere on the machine.
- Where two hosts are installed, Dusk prefers the newer rather than the
  first in a list.

## Non-goals

- Taking over or overwriting someone else's unit. Deferring is correct; the
  silence is the bug.
- Uninstalling, upgrading or reconfiguring a host Dusk did not install.
  Dusk says what is wrong and what would fix it; the person decides. (My
  call — rewriting a stranger's service is how you lose trust in a tool
  that manages services.)
- Fixing portal-vs-uinput input injection upstream. That is Sunshine's.
- Changing `SUNSHINE_TAG`. It is stale (`v2026.914.233613`, and only macOS
  was moved off it) but it is not implicated here, because the pinned build
  is not what runs on this machine.

## Design

### 1. Probe reports a version, and whose host it is

`LinuxHost::probe` currently returns `version: None`
(`host/platform.rs:216`) — Dusk has never read the host's version on any
platform, which is why "older installed host" could not be detected even in
principle. Fill it in, and add provenance beside it:

```rust
pub enum HostProvenance {
    Dusk,        // the unit Dusk wrote, description matches
    Foreign,     // someone else's unit, or a Flatpak Dusk did not install
}
```

Provenance comes from the unit's `Description` compared against the string
`apply.rs:462` writes, which is the one signal that distinguishes them.
Version comes from running the unit's own `ExecStart` program with
`--version` — `exec_start_path` (`platform.rs:289`) already extracts that
path for `set_credentials`, so the plumbing exists. Parse the
`Sunshine version: X` line out of the startup noise.

Two notes for whoever builds this. Sunshine logs its config to stdout before
the version line, so match the line rather than taking the first one. And
the Flatpak's binary is only reachable inside its sandbox — `set_credentials`
already special-cases this with `flatpak run --command=sunshine`
(`platform.rs:256`), and the version read needs the same branch.

### 2. The drawer says it

When provenance is `Foreign`, the device drawer shows the version and that
Dusk did not install it. When it is also older than `SUNSHINE_TAG`, that
reads as a caveat rather than a note, in the same place `HostCapabilities`
caveats already surface (`platform.rs:200`). The precedent is
`client/README.md`'s rule for a missing fork: "the device drawer says so
rather than leaving it to be guessed at." Same principle, other end of the
stream.

### 3. The input step checks the host, not the machine

Replace the rule-presence check with a check of the running host's input
path, and keep the rule check only as the remedy for the one case it
actually fixes. Three outcomes, not two:

- **Works.** The running host opens `/dev/uinput`. Done, as today.
- **Needs the rule.** The host would use uinput but the device is not
  granted to the seat. Offer the existing polkit step (`apply.rs:436`) —
  this is the case that step was written for and it stays as-is.
- **Cannot use uinput.** The host is sandboxed, or configured for portal
  capture, so no udev rule will help. Say that, name it as the likely cause
  of input that works on the desktop and dies in fullscreen games, and give
  the remedy: `flatpak override --device=all dev.lizardbyte.app.Sunshine`
  for the Flatpak case, or an unsandboxed host.

The third outcome is the whole point. A step that cannot distinguish "set
up" from "set up and unusable" is worse than no step, because it actively
argues against looking further — which is exactly what it did.

### 4. Prefer the newer of two hosts

`installed_unit()` should resolve every unit in `LINUX_UNITS` that exists,
read each one's version via (1), and pick the newest, rather than taking
the first that answers. Where versions cannot be read, keep today's order
as the tiebreak so behaviour does not regress on a host that will not
report.

## What this does not settle

Whether portal input injection is the cause of the dead mouse. The
confirming evidence is `ExecStart` for the running `sunshine.service` plus
its log's input lines; if the host turns out to be uinput-backed after all,
the mouse bug goes back to the fork — specifically `setCaptureActive` and
`updatePointerRegionLock` across a mid-stream mode change
(`mouse.cpp:350-380` in the fork), which is the one difference between a
fullscreen game and everything that works. The overlay's hover path is
already ruled out: `overlayPointerUsable` (`mouse.cpp:108`) returns false
while captured in relative mode and `handleMouseMotion` early-returns on it
(`duskoverlay.cpp:434`), so it cannot eat capture inside a captured game.
