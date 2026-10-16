# Release Notes

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
