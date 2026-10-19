# Release Notes

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
