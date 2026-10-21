# Machines that are reachable from where you are

A design for the half of "your machines" that is still missing: the grid knows
*which* machines are yours, and does not know *where* they are.

`architecture.md` ends its scope section with an invitation:

> If the reach ever matters more than the non-goal, the honest way to get it
> without operating servers is to lean on something like Tailscale rather than
> to build a relay — but that is a different conversation, and it should be had
> explicitly.

This is that conversation.

## What is already true

Since v0.3.19 the account's machine list seeds the grid, so every machine you
have signed in has a card whether or not it is advertising. The join is
`Device::machine_id`, answered by `peer::identify` and checked against the
account's list — not a name, because names differ: this Mac is `Michaels-Mini`
on the account and `Michaels-Mac-mini.local.` over mDNS.

What a card lacks, when nothing has found the machine, is an address. So the
grid can say "this is yours and nothing has found it" and nothing more. From
another network that is every card.

## The gap, concretely

`discovery.rs` says it plainly: mDNS "will not find a machine across a VPN —
multicast rarely survives the hop — which is why the manual address book is a
peer of this, not a fallback."

So today, reaching your own machine from elsewhere means typing its address in
by hand, on every machine you want to reach it from, and again whenever it
changes. That is the part of Parsec that Dusk cannot currently imitate, and —
unlike NAT traversal — it is not ruled out by anything.

The machines in this household already run overlay networks. Observed on the
Linux host while debugging pairing, it woke the Mac on each of:

```
10.147.19.255   ZeroTier
100.120.26.79   Tailscale
192.168.50.255  LAN
```

Three networks, two of which work from anywhere and neither of which mDNS
crosses. The reach already exists. Dusk simply has no idea about it.

## The design

**Each machine publishes the addresses it answers on, into the account entry
it already writes.**

Signing in already creates `dusk-machine-<id>.json` in a secret gist, holding
a name and a public key. It gains an address list:

```json
{
  "name": "Venus",
  "key": "41c02f6d…",
  "addresses": ["192.168.50.149", "100.101.8.12"],
  "updated": 1792550000
}
```

A machine writes its own entry and reads everyone else's — the same file, the
same gist, the same token. The grid then has somewhere to try for a machine
mDNS cannot see, and `probe_order` gets a VPN address to fall back to after the
LAN one fails.

Nothing new is operated. No rendezvous, no relay, no NAT traversal. The overlay
network is the person's own and is doing what it already does; Dusk is only
learning what it already could have asked.

### Which addresses

Every address the machine answers on, minus the ones that cannot help:

- loopback, which is never useful to another machine
- link-local (`169.254.0.0/16`, `fe80::/10`), which does not route
- the ULA addresses the client already ignores (`Ignoring ULA:` in its log)

That leaves LAN addresses and overlay-network addresses, which is the set
worth having. Dusk should not try to classify them beyond that — a Tailscale
address and a LAN address are both just addresses to try, and guessing which
is "the VPN one" is how this acquires a per-provider special case for every
network someone runs.

### Ordering

`primary_address` already means "the one that last answered, and the one we try
first", and probes already walk addresses in order. A published address is
simply another candidate, tried after anything discovery found — the LAN path
is faster when both work, and should stay first.

## The two decisions worth making deliberately

### Should the addresses be encrypted?

A secret gist is unlisted, not private: anyone with the URL can read it, and
the URL is in every machine's keystore. Today it holds names and public keys,
which is unremarkable. Addresses are a map of where your machines are.

**Recommendation: encrypt them, and do it with what is already there.** Every
machine has an Ed25519 keypair for pairing proofs and publishes its public
half. Addresses can be sealed to those keys, so the gist holds ciphertext and
only a machine on the account can read it.

The cost is honest: a machine that cannot read the account cannot read
addresses either — but that is already true of everything the account does,
and v0.3.15 exists because that failure used to be silent. The other cost is
that Ed25519 is a signing key, not an encryption key, so this needs either a
second keypair or conversion to X25519. A second keypair published alongside
the first is simpler to reason about and does not weaken the signing key by
using it for two purposes.

Against: a Tailscale address is not much of a secret, and unencrypted is
several hundred lines less. The case for encrypting anyway is that this is the
first thing Dusk would publish *about* a person's network rather than about
their machines, and the mechanism is sitting there.

### What about stale addresses?

A machine that moves networks leaves a dead address in its entry. The grid
would try it, wait for a timeout, and look slow for a machine that is fine.

**Recommendation: carry `updated` and let a machine rewrite its own entry
whenever its addresses change, not on a timer.** Reading side: prefer
discovery, then published addresses newest-first, and let the existing probe
timeout do the rest. Expiring entries outright is worse — a machine switched
off for a fortnight is still yours and its last known address is still the
best guess.

What this must not become is a heartbeat. Writing to GitHub every few minutes
to prove liveness would be a rendezvous service with extra steps, and the
scope document rules that out for good reasons.

## What this does not do

- **It does not make Dusk work from anywhere.** It makes Dusk work anywhere
  *your overlay network already reaches*. Without Tailscale, ZeroTier or
  similar, this changes nothing, and that is the ceiling the project chose.
- **It does not do NAT traversal**, hole punching or relaying.
- **It does not help someone else reach your machine.** Guest access is a
  different feature and should not be smuggled in here.

## Order of work

1. **Publish and read addresses, unencrypted, behind the existing sign-in.**
   Smallest thing that proves the reach: open Dusk on the tailnet and have
   your machines be there. One gist field, one probe-order change.
2. **Encrypt.** The X25519 key, sealing, and the migration for entries written
   before it.
3. **Wake on LAN**, which is unrelated but adjacent: the client already sends
   WoL to every address it knows, and a published address list is exactly what
   makes waking a machine from another network possible at all.

Doing (1) first is deliberate: it is the step that tells you whether the
feature is worth having. If opening Dusk away from home does not feel like
the thing you wanted, (2) was not worth building.

## Open questions

- Should a machine publish addresses when the person has not asked it to?
  Signing in currently publishes a name and a key; adding addresses is a
  meaningful widening of that and may deserve its own consent, or at least its
  own line in the Account screen.
- What happens when two machines on the same account share an address — behind
  the same NAT, seen from outside? The machine id disambiguates them once
  `identify` answers, but the first probe cannot know.
- Does the Steam Deck case change this? It is the one target likely to move
  between networks regularly, and it is the one where a stale address is the
  normal state rather than the exception.
