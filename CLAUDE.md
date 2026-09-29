## Project Context

A single polished desktop app that wraps Sunshine (host) and Moonlight (client) behind one device-centric UI, so hosting and connecting stop feeling like two separate programs. Sunshine runs as a managed background service configured through the app's own screens via its local API; Moonlight handles the stream, shelled out to moonlight-qt at first with an embedded moonlight-common-c renderer as a later swap. Networking is accepted as-is — LAN or the user's own VPN, with Sunshine's existing pairing and mDNS discovery — so there are no accounts, rendezvous servers, or relay infrastructure to run. The core payoff is a device grid where every machine appears as a card showing online, paired, and hosting/available state.

## Goals

- One app that presents hosting and streaming as a single 'my machines' experience rather than two tools
- Device grid UI: each machine as a card with online state, pairing state, and hosting/available state
- Manage Sunshine as a background service and replace its web config UI with native app screens driven by its local API
- Launch and manage streaming sessions via moonlight-qt initially, with a path to an embedded moonlight-common-c renderer
- Installer that handles the messy host setup (service registration, virtual display driver) so first-run feels effortless
- Accept Sunshine's existing pairing and mDNS discovery rather than inventing new connection flows

## Out of scope

- No account system, rendezvous service, or user identity layer
- No UDP hole punching, NAT traversal, or TURN-style relay fallback
- No reimplementation of the streaming protocol, encoders, or input handling
- Not a true Parsec replacement for arbitrary internet connections — LAN or user-provided VPN only
- No hosted backend or infrastructure the project has to operate

## Suggested stack

- **Sunshine (managed as a separate process)** — Proven self-hosted host with a local web API that can be driven programmatically; keeping it a separate process also avoids GPL-3.0 copyleft entanglement with the shell
- **Moonlight (moonlight-qt shelled out, moonlight-common-c later)** — Shelling out gets a working app fast; the streaming logic lives in a reusable C library so an embedded window can replace it later without UI changes
- **Desktop shell framework (undecided)** — Needs native process/service control and a polished custom UI; choice deferred to the first dev session
