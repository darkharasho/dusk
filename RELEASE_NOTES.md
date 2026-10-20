# Release Notes

Version v0.3.0 — October 19, 2026

Dusk sets up a machine to host by itself, pairs your own machines without a
PIN, and keeps every setting in one place.

## Hosting without the setup

- **Hosting comes with Dusk** on Windows and Linux. Installing it no longer
  means a separate download, and Dusk registers it to start in the background.
- **No password to make up.** Dusk creates the host's sign-in itself and keeps
  it in the system keystore, so the sign-in form is gone. **Finish setting up
  hosting** on a host that was set up before Dusk replaces the password it
  had.
- **Set up this machine** runs every step that can be automated, in order:
  the background service, the firewall (one prompt on Windows, not two) and,
  on Linux, the input permission controllers need.
- **A virtual display on Windows**, for hosts with no monitor attached or
  for streaming at a resolution the monitor lacks. It is an optional step with
  its own confirmation, never part of **Set up this machine**. It installs the
  user-mode Virtual Display Driver (MIT, signed), pinned to release 25.7.23
  and checked against a hash written into Dusk.

## Pairing

- **Pair without typing a PIN** when the other machine runs Dusk. It shows
  **Allow** with your machine's name, and one press finishes the pairing.
  Dusk listens on TCP port 47850 for this; Windows asks to allow it once.
- **Sign in with GitHub** under Settings › Account, and machines signed in to
  the same account pair with each other with no prompt at all. It is
  optional. Dusk asks GitHub only for permission to keep one private gist
  listing your machines, which holds each one's name and public key. There
  is still no server of Dusk's own.

## Streaming

- Dusk steps aside once a stream starts and comes back when it ends.
- **Leave the stream** in the device drawer ends the stream and leaves the
  game running on the host.
- When a stream fails, the drawer says why.

## Settings

Everything that changes this machine is now under a **Settings** tab, split
into Account, Hosting, Setup, Host settings and About. The Machines tab
shows a summary of this machine with a link to the right section.

## Known limits

- None of this has run end to end between two real machines yet: hosting
  setup, PIN-free pairing, GitHub pairing, Leave the stream and the Windows
  display driver are unit-tested and compile-checked only. Expect rough
  edges and please report them.
- macOS still downloads hosting rather than bundling it, and its dock icon
  does not step aside during a stream.
- Windows builds remain unsigned (SmartScreen: *More info* → *Run anyway*).
- Networking is LAN or your own VPN only, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.2.0 — October 19, 2026

Windows and Linux builds, and the in-stream overlay actually ships.

## Windows and Linux

Every release now builds all three platforms. The Rust side has been
cross-platform since the host backends were written and CI has compiled and
tested it on all three ever since; only the packaging was macOS-only, so none
of that work could reach anyone not on a Mac.

- Windows: an `.msi` for managed or scripted installs, and a per-user
  `-setup.exe` that installs without administrator rights.
- Linux: a `.deb` and an `.AppImage`, x86_64.

Both are **unsigned**. On Windows that means SmartScreen shows "Windows
protected your PC" on first run, and getting past it is *More info* → *Run
anyway*. Signing needs an Authenticode certificate, and every route to one
requires a verified organisation, so it waits until there is demand to
justify the cost.

## The in-stream overlay

Dusk streams through its own fork of Moonlight, which carries a floating
overlay — a draggable handle that opens a menu without leaving the stream.
No packaged build had ever actually contained that fork: a release build
looked for it only in a developer's sibling checkout, so every install fell
through to whatever Moonlight was on the machine. That streams perfectly well
and has no overlay, which is why the feature looked broken rather than
absent.

Builds now carry the client, and the device drawer says so when it is running
through a stock Moonlight instead of guessing.

The client also presents as Dusk Stream now, with Dusk's icon, rather than
announcing itself as Moonlight in the dock, the taskbar and window titles.

## Known limits

- The client path has still not been tested against a second machine.
- The Windows and Linux host paths are compile-checked and have not run on a
  real machine, so hosting from them is untested.
- A Mac cannot usefully run Sunshine, so `DUSK_MOCK=1` remains the way to
  work on the UI there.
- Networking is LAN or your own VPN only — no accounts, no relay, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.1.0 — October 16, 2026

First tagged build, and the first one that is signed and notarized, so macOS
opens it without the unidentified-developer warning.

This is an early preview. The device grid is real and working; most of what
sits behind it is built but has not been exercised against a second machine.

## What works

- Device grid — every machine on the network appears as a card showing online,
  paired, and hosting state, fed by real mDNS discovery and `serverinfo`
  polling rather than fixtures.
- A manual address book alongside discovery, plus Moonlight's own host list as
  a source, for machines mDNS does not reach.
- Sunshine host control and the schema-driven config screens that replace
  Sunshine's web UI.
- Tray icon and first-run install paths for each platform.

## Known limits

- The client path (pair, list apps, launch via `moonlight-qt`) is built but has
  not been tested against a second machine.
- The config screens need a signed-in Sunshine host to exercise properly.
- A Mac cannot usefully run Sunshine, so `DUSK_MOCK=1` remains the way to work
  on the UI there.
- Networking is LAN or your own VPN only — no accounts, no relay, by design.

## Install

Download the `.dmg`, drag Dusk to Applications. The build is a universal binary
and runs natively on both Apple Silicon and Intel.
