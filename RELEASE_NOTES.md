# Release Notes

Version v0.3.46 — October 29, 2026

A stream that failed no longer leaves the next one to fail the same way, and
a failure now says what it was asked for.

## Changes

- **A retry is no longer answered by the first failure's leftovers.** When a
  stream failed, the other machine could be left hosting a session nobody was
  in, and the next stream had to tell it to let go before it could start.
  Whether it needed to was read from the grid's last poll — which, on a retry
  seconds after a failure, could still say the machine was free while it was
  already holding the dead stream's session. The clear was skipped and the
  retry failed for the very reason the clear exists to remove. The machine is
  now asked directly, at the moment it matters. A stream dropped by the
  network is still left resumable, exactly as before.
- **A failed stream records what it asked for.** The log for a session kept
  the machine and the app but not the size, frame rate, bitrate or codec it
  asked for — those live in settings, where the next change overwrites them,
  so a failure read back a day later could not be told from the settings that
  caused it. Each stream now writes the client's arguments to its own log.

Version v0.3.45 — October 28, 2026

Streaming settings can now be set per machine, so a fast host and a modest
one no longer have to share one set.

## Changes

- **Per-machine streaming settings.** A machine's drawer gains a "Streaming
  for this machine" section: Resolution, Bitrate, Frame rate and Video codec,
  each starting at "Use default" — which shows the global value and inherits
  it — or set to override it for that machine alone. So a host with a strong
  GPU can be pinned to 120 fps and HEVC while a weaker one keeps the defaults.
  A machine you set nothing on streams exactly as before.

Version v0.3.44 — October 28, 2026

A stream that failed to connect no longer locks out the next one.

## Changes

- **A stuck client no longer blocks the next stream.** The streaming client
  runs one at a time: when a stream could not reach the other machine, its
  client stayed open on the error, and every stream started after that was
  turned away by it — with nothing on screen to say why. Dusk now clears a
  client still open from a previous stream before it starts the next one, so
  a stream always has the field to itself. Paired with the stale-session
  clearing from v0.3.43, a stream that went wrong on either side no longer
  has to be untangled by hand.

Version v0.3.43 — October 28, 2026

A machine left hosting a session nobody is in no longer turns away the next
stream, and how smooth a stream looks is now yours to set.

## Changes

- **A stale session no longer blocks the next stream.** When a streaming
  client went away without bowing out — a crash, a force quit, a dropped
  network — the host kept the old session running, and the next attempt to
  stream from it failed to connect rather than taking over. Dusk now clears
  a session the host is holding that nothing here is in, just before it
  starts, so the stream connects instead of being turned away. A stream you
  really are in is left alone.
- **Frame rate is a setting.** Streaming settings gains a Frame rate, beside
  Bitrate: leave it Automatic, or ask for up to 240 a second on a fast
  screen. A host that cannot encode that quickly simply sends fewer, so a
  rate higher than it can reach does no harm.

Version v0.3.42 — October 27, 2026

A stream that cannot reach the other machine now says so, instead of quietly
going back to Ready.

## Changes

- **A failed stream gives its reason.** Starting a stream to a machine that
  was asleep, or had gone off the network, put the card back to Ready as
  though nothing had been asked — the reason existed, but nowhere you could
  see it. The failure is now shown, in the client's own words: "Failed to
  connect to 192.168.50.146".
- **A session's log is worth keeping.** When a stream fails, Dusk's log for
  that session now holds the reason and the streaming client's own account of
  it, rather than a pointer to a file that a reboot deletes.

Version v0.3.41 — October 23, 2026

On a Mac you stream from, the mouse now works on whichever screen you switch to.

## Changes

- **The mouse follows a display switch on macOS.** After switching the
  stream to a Mac's second screen, the pointer stayed stuck on the main
  one, so the other screen could not be used. The host bundled into Dusk
  for Mac is updated and now moves the pointer on the screen being shown.
- Linux and Windows hosts are unchanged.

Version v0.3.40 — October 23, 2026

The in-stream display picker now shows only the screens the other machine actually has.

## Changes

- **Switching displays lists the host's real screens.** The overlay used to
  offer a fixed run of screens whether or not they existed. It now lists one
  entry per screen on the machine you are streaming from, and switching
  between them moves the stream to that screen.
- **No display switch on a one-screen host.** When the other machine has a
  single screen, "Switch display" no longer appears.
- **End the session from the overlay.** A new row ends the session on the
  other machine, not just this stream.
- **A dropped session says it can be resumed.** The device drawer now says
  so when a stream ended but the session is still running on the host.
- The other machine needs v0.3.40 too to report its screens; with an older
  one, the picker behaves as before.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.39 — October 23, 2026

Updating on Linux no longer leaves the old copy's files mounted.

## Changes

- **A restart into an update cleans up after itself on Linux.** Each update
  of the AppImage used to leave the previous copy's files mounted under
  `/var/tmp`, with a helper process to serve them, until the next sign-out.
  A machine updated several times in a day had several of each. The
  restart now lets go of them properly. The copy you update *from* still
  runs the old code, so this one update leaves one behind; after that,
  none.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.38 — October 23, 2026

A machine whose hosting has stopped answering securely now fixes itself, and the other machine says what is wrong.

## Changes

- **Hosting restarts itself when it gets stuck.** A stream that failed to
  start could leave hosting in a state where the machine looked up but no
  other machine could pair with it, stream from it, or end its session.
  Dusk on the host now notices within about ninety seconds and restarts
  hosting on its own, at most once every ten minutes.
- **The other machine says so instead of checking forever.** A card for
  such a machine used to sit on "Checking whether this machine is paired".
  It now explains that the machine is up but its hosting is not answering
  securely, and what to do about it.

The host has to run this version to restart itself; if it runs v0.3.37 or
earlier, turn hosting off and on again there, or update it from here.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.37 — October 22, 2026

Reach a Mac whose screen is asleep or locked.

## Changes

- **Connecting wakes a Mac's screen.** When you start a stream to a Mac,
  its screen now turns on first, so a Mac whose display had gone to sleep
  no longer refuses the stream. A locked Mac shows its lock screen in the
  stream, and you can type your password there.

A Mac that has gone fully to sleep still can't be reached. To help it wake
when you connect, turn on **Wake for network access** in its System
Settings › Energy (or Battery › Options on a laptop). The Mac has to run
this version for its screen to wake; if it runs v0.3.36 you can update it
from here.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.36 — October 22, 2026

Update your other machines from the one you're sitting at.

## Changes

- **Update a machine from here.** Each of your machines now shows which
  version of Dusk it runs. When one is behind, its panel offers **Update Dusk
  on this machine**: it fetches the release itself, installs it only if the
  signature checks out, and comes back the way it was, hidden in the tray if
  it was hidden. It says no if someone would lose a stream, apart from your
  own stream from it, which the panel warns will end.
- **Machines keep themselves up to date.** With **Update automatically when
  idle** on in Settings, which it is by default, Dusk checks once an hour
  and installs a new release only while nobody is streaming to or from that
  machine.
- **Restarting into an update brings the window back.** Pressing Restart
  after an update now reopens Dusk instead of leaving it closed.

This version has to be installed by hand once on each machine; from then on
it can be updated from your others. Linux `.deb` installs and Windows `.msi`
installs need someone at the machine to approve an update, so they can't be
updated remotely; the panel says so. A Windows machine shows an installer
progress window briefly while it updates.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.35 — October 22, 2026

Copy and paste now work across a stream.

## Changes

- **Your clipboard follows you through a stream.** Text you copy on either
  machine can be pasted on the other while a stream is running, on Windows,
  macOS and Linux in any mix. It is on by default and can be turned off with
  **Share clipboard while streaming** in Settings. It only shares with the
  machine streaming from this one, over an encrypted connection, and stops
  when the stream ends.
- **It picks back up on its own.** Turning sharing back on mid-stream, or a
  host that was briefly unavailable, reconnects without restarting the
  stream.
- **Very large copies stay put.** A copy over 1 MB is not sent, and the
  stream says so.
- **A GNOME Wayland host says why it can't share.** GNOME doesn't let a
  background app follow the clipboard, so sharing is off there and the
  device panel says so instead of looking broken.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.34 — October 22, 2026

Fewer sync, sign-in and pairing errors, and a title bar that behaves the same on every platform.

## Changes

- **Your machines show up without a restart.** A machine signed in after
  Dusk opened was not counted as yours until Dusk was started again. The
  list is now re-read every few minutes and whenever you press Refresh.
- **A sign-in GitHub no longer accepts says so.** The Account screen showed
  signed in while every check quietly failed. It now shows a notice with a
  **Sign in again** button.
- **Pairing says what actually went wrong.** A host that could not check
  your account, because its own sign-in lapsed or GitHub was unreachable,
  used to tell you to sign in on the other machine. It now says it could
  not check, and what to do there.
- **A locked keystore no longer resets hosting.** Turning hosting on while
  the system keystore would not answer rotated the host's password and
  restarted it. Dusk now refuses and says the keystore could not be opened.
- **The device grid stops flickering.** A machine could appear twice for a
  moment on every refresh, and one missed check sent it Offline. Cards keep
  their identity, keep a settled pairing state, and go Offline only after
  two misses in a row.
- **A stream that stalled on a bad log line no longer freezes.** Unusual
  text in the stream's output could block it with no error shown.
- **Hosting tells you when it is waiting on a restart.** If a restart keeps
  not happening, the Hosting panel says so and why, instead of every
  pairing machine being told to try again in a minute.
- **Quitting Dusk ends any running stream** instead of leaving it with no
  way to disconnect.
- **The typed PIN retries** like the automatic path, rather than failing
  once with "no device is waiting yet".
- **Errors read as sentences.** Some failures showed as "[object Object]".
- **A corrupt settings file is kept aside** rather than overwritten.
- **The title bar is the same on every platform.** A double-click on
  Windows and Linux maximized the window and put it straight back; the
  update badge sat in a different place on macOS; the bar did not dim when
  the window was not the active one. All three are fixed, and on macOS the
  bar no longer leaves space for the traffic lights in full screen.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.33 — October 22, 2026

Each stream now leaves a log behind, even if the computer freezes.

## Changes

- **Every stream keeps a log you can read after a reboot.** On Linux, what
  happened during a stream used to be kept only in memory, so a freeze took
  it with it. Each stream is now written to
  `~/.local/state/dev.dusk.app/streams/` as it runs and saved to disk every
  second. The last ten streams are kept.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.32 — October 21, 2026

A Linux host matches your screen when you pick up a stream where you left it.

## Changes

- **Picking up a stream on a Linux host now sizes its screen to yours.** If
  your last stream dropped instead of being quit, the next one carried on
  where it left off, but at the host's own screen size. The host now resizes
  its screen to yours as the stream resumes, and puts it back when you quit.
- **A Linux host puts its screen back by itself once a dropped stream is
  gone.** Before, it stayed at the other device's size until Dusk next
  started. Now it goes back after about two minutes with nobody connected.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.31 — October 21, 2026

Pairing without a PIN finishes after a stream that dropped.

## Changes

- **Pairing with a machine whose last stream dropped, instead of being
  quit, now finishes.** That machine went on counting itself as mid-stream
  for as long as the app the stream had started kept running, so it waited
  forever to finish pairing. Dusk now checks whether anyone is still
  connected, and finishes once nobody is. A stream that is really going on
  is still left alone.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.30 — October 21, 2026

Pairing without a PIN now finishes on Linux.

## Changes

- **A Linux machine pairing with one of your own machines no longer stops at
  "This machine couldn't save the pairing."** The other machine had already
  trusted it, but this one failed to record the pairing on its side. It now
  saves it, and pressing Pair again finishes without a PIN.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.29 — October 21, 2026

Your own machines pair without a PIN.

## Changes

- **Machines signed in to the same GitHub account now pair on their own.**
  Press Pair and the two exchange certificates, each vouched for by your
  account, with no code to read off one screen and type into the other. A
  machine that is in the middle of a stream finishes it first and pairs
  after. If anything gets in the way — an older Dusk on the other end, a
  machine that can't be reached, a host that says no — Dusk asks for the
  PIN exactly as before.
  - A brand-new install that has never paired anything still needs the PIN
    once.
- **A machine you take off your account stops being trusted.** Dusk
  removes it only once it is certain; a locked keychain or a sign-in it
  can't use leaves every pairing where it was.
- **A host whose certificate changes is paired again by itself**, and a
  card reads paired only when both machines trust each other.
- **Dusk is now free software under the GPL, version 3 or later.**

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.28 — October 21, 2026

Dusk installed from a `.deb` can now update itself.

## Changes

- **A Dusk installed from the `.deb` updates like every other one.** It
  finds the new version on its own, fetches it in the background, and
  offers **Restart to update** — asking once for permission to install,
  because the package manager needs it. Until now this was the one way of
  installing Dusk that meant downloading a file and typing a command, on
  the machines least likely to have anyone sitting at them.
  - Installing this release is the last manual one. A copy has to be
    carrying this change before it can use it, so update to v0.3.28 by
    hand and the ones after it look after themselves.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.27 — October 21, 2026

Streams from a Linux machine fit the screen watching them.

## Changes

- **A Linux machine can now give each stream a screen the size of the
  device watching it.** Turn it on in Settings › Hosting. An ultrawide
  gets an ultrawide picture from a host whose own monitor is nothing of
  the sort, instead of a smaller one with black bars down the sides. The
  host's own screen changes with it for the length of the stream and
  changes back afterwards — and now changes back too if a stream ends
  badly and never gets the chance.
- **Your own machines are no longer sometimes filed as other people's.**
  A machine that said who it was before Dusk had finished reading your
  account list was put under "Other machines on this network" and left
  there, which lasted until Dusk happened to be restarted at a better
  moment. Your account decides it now, every time it looks.
- **A machine with no screen of its own can be set up to do this.**
  `dusk screen on` from a terminal, alongside the existing `dusk setup`,
  `dusk signin` and `dusk status`, for a host in a cupboard.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.26 — October 21, 2026

Pairing works again with an up-to-date host.

## Changes

- **Connecting to a machine running a recent host no longer hangs on
  pairing.** Newer hosting software asks which pairing request is being
  approved, and Dusk was not saying — so the request sat waiting, timed
  out, and reported itself as a PIN that had been turned down. Dusk now
  looks the request up and approves it. Hosts that do not ask are
  unaffected, and nothing about how you connect has changed.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.25 — October 21, 2026

Signing in somewhere else now reaches a window that is already open.

## Changes

- **A window no longer keeps using a sign-in that has been replaced.** Dusk
  reads your sign-in when it starts and keeps it for as long as it runs. If
  you signed in again from somewhere else — a terminal, or another copy of
  Dusk — the open window never noticed, kept using the old one, and refused
  your own machines as belonging to a different account. It now checks again
  when GitHub turns the old one down.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.24 — October 21, 2026

One machine, one card.

## Changes

- **Your machines no longer appear twice.** Since the grid was split into
  your machines and everything else, a machine running an older Dusk
  showed up in both lists — once from your account with no address, once
  from the network with one. Dusk now recognises them as the same machine
  by name when the other end is too old to say who it is, and shows a
  single card with the address, as yours.
- **A machine signed in before address sharing existed can now receive
  it.** It was given a new key to receive with on every launch instead of
  keeping one, so addresses from your other machines would never have
  reached it.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.23 — October 21, 2026

No change you can see. The last release's fix is now proven rather than
assumed: the case it guards against — a keystore that will not answer —
could not be reached by a test before, and now is.

Version v0.3.22 — October 21, 2026

Signing in again keeps the machine you already had.

## Changes

- **A machine no longer becomes a different machine when the keystore is
  briefly unavailable.** Signing in again checks whether this machine is
  already signed in, and could not tell the difference between "it is not"
  and "the keystore would not say" — so a locked keyring or a dismissed
  password prompt gave the machine a brand new identity, left its old one
  stranded on your account as a duplicate, and cut it off from the
  addresses your other machines share. It now stops and says so instead of
  guessing.

If you have a machine listed twice on your account, the older entry can be
removed; it was this.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.21 — October 21, 2026

Only your own machines can read where your machines are.

## Changes

- **The addresses your machines share are now encrypted.** They are kept
  in a private list on your GitHub account, which v0.3.20 wrote in plain
  text. That list is unlisted rather than secret — anyone holding its
  address could read it — and where your machines answer is a map of your
  network. Each machine now seals its addresses so that only the other
  machines signed in to your account can open them.

Nothing to do: machines re-share their addresses, sealed, the next time
they notice a change. A machine that has not been signed in since this
release keeps working as before and simply shares nothing until it is.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.20 — October 21, 2026

Your machines are there when you are somewhere else.

## Changes

- **Machines reached over a VPN no longer have to be typed in.** Dusk finds
  machines by listening to the local network, and that does not reach
  across a VPN — so away from home your own machines were present in the
  grid but unreachable, unless you had added their address by hand on every
  machine you wanted to reach them from. Each machine now tells your
  account where it answers, and the others try that when the local network
  turns up nothing.

This uses a VPN you already run — Tailscale, ZeroTier or similar. Dusk
does not run a server of its own and still cannot reach a machine that is
not on one. Addresses are shared through the same private list that
already holds your machines, visible only to your account.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.19 — October 21, 2026

Your machines stay in the grid when they are switched off.

## Changes

- **A machine of yours no longer disappears when it stops hosting.** The
  grid showed what was announcing itself on the network, so a machine you
  own that was turned off — or that had its hosting uninstalled — simply
  vanished, which looks like Dusk losing it rather than like the machine
  being off. Every machine signed in to your account now keeps its card,
  and says it has not been found rather than claiming to be offline.
  Its card gives way to the real one the moment the machine turns up.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.18 — October 21, 2026

The grid says which machines are actually yours.

## Changes

- **"Your machines" now means your machines.** The grid listed everything
  on the network that can host, under a heading that claimed they were all
  yours — so someone else's computer looked exactly like one of your own.
  Machines signed in to your account now come first, and anything else
  that can host here is listed below as "Other machines on this network".
  Nothing is hidden: a machine missing entirely would just look like Dusk
  failing to find it. Pairing with one of those still asks for a PIN, as
  it always did.

When you are not signed in, Dusk cannot tell whose machines are whose, so
it shows one list exactly as before.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.17 — October 21, 2026

Pairing works the first time, not the second.

## Changes

- **First-time pairing no longer needs a second go.** The machine being
  connected to waited thirty seconds and then gave up — but a streaming
  client that has just started spends longer than that looking for the
  machine before it can pair at all, especially on a computer with several
  network connections. The second attempt always worked because by then it
  had warmed up. The host now waits as long as the client is allowed to
  take, so the first attempt is the one that works.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.16 — October 21, 2026

Pairing your own machines works again.

## Changes

- **"That PIN was not accepted" when nothing was wrong.** Connecting one
  of your machines to another could fail with that message even when both
  were signed in to the same account and everything else was right. The
  machine being connected to gave the other one five seconds to get ready,
  then gave up — and giving up stopped the machine that was still starting.
  It now waits long enough for a client that takes a moment to start, which
  is most of them.

If a PIN really is wrong, you will now wait half a minute to be told so.
That is the cost of not cutting off every pairing that was merely slow.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.15 — October 21, 2026

Dusk now says when a GitHub sign-in has run out.

## Changes

- **"You're not signed in to the same GitHub" when you were.** Signing in
  lasted eight hours. After that a machine could no longer check which
  machines are yours, so your own machines looked like strangers' and
  pairing asked for a PIN — while the app still showed you as signed in.
  Dusk now notices and says so, on the Account screen and in `dusk status`,
  instead of leaving you to guess.
- **`dusk status` shows the sign-in**, so a machine set up over SSH says
  whether it will actually pair without a PIN.

Signing in again fixes it. If you run your own GitHub app for Dusk, turn
off "Expire user authorization tokens" in its settings and sign-ins stop
running out at all — Dusk cannot renew them for you, because renewing
needs a client secret that an app on your own computer has nowhere safe to
keep.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.14 — October 20, 2026

Sign a machine in without a screen in front of it.

## Changes

- **`dusk signin`.** Signing in to GitHub is what lets your machines pair
  with each other without anyone typing a PIN, and until now it needed the
  window — so a machine in a cupboard could host, but could never be one
  your others recognised. This prints a short code, you enter it on any
  device you already have, and that machine joins the rest. It says so if
  the sign-in could not be kept, rather than leaving a machine that looks
  signed in and is forgotten at the next restart.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.13 — October 20, 2026

A machine that logs in on its own can now keep its sign-in.

## Changes

- **Hosting set up over SSH keeps its sign-in.** On Linux the place Dusk
  keeps secrets has to be unlocked, normally by the password you type at
  the login screen. A machine set to log in automatically never types one,
  so Dusk could not save anything: it looked set up, then forgot which
  machine it was at the next restart and quietly stopped being one your
  other machines recognise. `dusk setup --unlock-keyring` opens it for
  this restart, reading the password from standard input and leaving the
  keyring's own password untouched.
- **`dusk setup --store=tpm`, for a machine nobody logs in to.** Keeps
  Dusk's secrets sealed to that machine instead, so they survive a restart
  with nobody there. They can only be read back on the machine that wrote
  them — move the disk elsewhere and they are unreadable. If there is no
  TPM to seal to, the command refuses rather than storing anything in the
  clear.
- **`dusk status` says where secrets are kept**, and says so plainly when
  a locked keyring means nothing can be saved.
- **No more "systemctl did not answer" when it did.** Turning hosting on or
  off could take a few seconds longer than Dusk was willing to wait, so
  work that had succeeded was reported as having failed.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.12 — October 20, 2026

Set a machine up to host without sitting in front of it.

## Changes

- **`dusk setup` and `dusk status`.** A machine you only ever stream from
  often has nobody at it — a box in a cupboard, a headless Linux host, one
  you reach over SSH. Until now setting one up needed a desktop session and
  a mouse, because every step was a button. `dusk setup` carries out the
  same steps the Setup screen does, in the same order, and prints what it
  did and what is left. `dusk status` says whether hosting is installed,
  running, and set up. Running `dusk` with no arguments opens the window as
  before. Steps only you can finish are listed, with what to do, and do not
  count as a failure. A virtual display is never installed this way: that
  stays a thing you ask for.
- **Hosting comes back after a reboot on Linux.** Its background service was
  tied to a target that Ubuntu's GNOME never switches on, so it was set to
  start at sign-in and then quietly didn't. Only affects a host set up from
  this version onwards; one already installed keeps the service it has.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.11 — October 20, 2026

Setting up hosting works on Windows. It never did before this release.

## Changes

- **Install hosting on Windows.** Setup › Install hosting failed every
  time with "this installation package could not be opened", on every
  Windows machine, in every version that had the step. Dusk handed the
  installer's path to Windows in a way that cut it at the first space,
  and the path always has one. A Windows machine that already hosts was
  never affected: Dusk adopts what is there instead of installing.
- **No more flashing window.** Dusk checks whether hosting is on every
  five seconds, and on Windows each check opened and shut a console
  window. The checks now run out of sight.
- **A way to set hosting up from Settings › Hosting.** It said hosting
  was not installed and then offered nothing that could install it.
  There is now a button to Setup, where the step lives.
- **The window buttons sit straight on macOS.** They were a few points
  above the middle of Dusk's title strip.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.10 — October 20, 2026

Windows hosts can stream a screen the size of the device you're
connecting from.

## Changes

- **Stream a screen sized to the other device.** On a Windows host with
  the virtual display installed (Setup › Virtual display), turn on
  Settings › Hosting › **Stream a screen sized to the other device**.
  Windows asks for permission once. After that, connecting with
  Settings › Streaming › Resolution on **Match the host** streams at your
  own screen's size, for example 3440×1440 on an ultrawide, with no black
  bars. The host's own monitors switch off while a stream runs and come
  back when it ends. A screen size the host doesn't have streams at the
  closest one of the same shape.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.9 — October 20, 2026

Bitrate and video format settings, and the in-stream Dusk icon within
reach on NVIDIA Linux machines.

## Changes

- **Choose a bitrate.** Settings › Streaming › Bitrate is Automatic, which
  sizes it to the stream's resolution and frame rate, or a fixed 5 to
  150 Mbps. Lower it if the picture stutters; raise it if it looks blocky.
- **Choose a video format.** Settings › Streaming › Video codec is
  Automatic, H.264, HEVC (H.265) or AV1. Automatic picks the best one both
  machines can handle in hardware.

## Fixes

- **The Dusk icon stays inside the picture on NVIDIA Linux machines.**
  There the stream is drawn a different way, which put the icon in the
  window's corner. When the stream is narrower than the screen, that
  corner is in a black bar the mouse can't reach.
- **Matching the host's screen works with Windows hosts.** Windows'
  firewall blocked Dusk from asking a host its screen size, so streams
  fell back to 1080p. On each Windows host, update Dusk and run **Open the
  firewall for hosting** in Setup again; it now lets Dusk through as well
  as hosting, and clears any rule that was blocking Dusk.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.8 — October 20, 2026

A tidier window that holds still.

## Changes

- **The header stays put.** Switching between Machines and Settings no
  longer shifts the top of the window. Refresh and Add machine now sit
  beside the "Your machines" heading, with a count of your machines.
- **Machine cards say something useful.** An online machine shows how
  quickly it answers; an offline one shows when it was last seen.
- **This machine matches the rest.** It is a slim card showing whether
  hosting is on and at what address. Click it to open Hosting settings,
  or Setup if hosting isn't set up yet.
- **Settings is easier to scan.** The sidebar has icons, each switch sits
  on the same line as its label, and stacked settings have a divider
  between them.
- **Ending a stream from the drawer** uses a smaller button sized to its
  label.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.7 — October 19, 2026

Fixes v0.3.6 opening to an empty window.

## Fixes

- **Dusk opens to your machines again.** v0.3.6 waited on itself while
  gathering what the window shows, so it never finished: the window
  stopped at the Dusk header with nothing beneath it, every time. An
  install stuck on v0.3.6 still checks for updates and offers this one
  in the titlebar.

Everything from v0.3.6 is unchanged: streams at the host's screen size,
Settings › Streaming › Resolution, and the Dusk button staying inside the
picture.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.6 — October 19, 2026

Streams match the host's screen, and the Dusk button stays within reach.

## Streaming

- **Streams run at the host's screen size.** Until now every stream was
  1280×720. Dusk now asks the host's Dusk how big its screen is before a
  stream starts. A host that doesn't run Dusk, or runs an older version,
  streams at 1080p.
- **Settings › Streaming › Resolution** chooses something else: the size
  of the screen you're sitting at, or 720p, 1080p, 1440p or 4K. The
  bitrate follows the size.

## Fixes

- **The Dusk button stays inside the picture.** When the window's shape
  didn't match the stream's, as on an ultrawide, the picture had black
  bars and the Dusk button sat out in a bar where the mouse couldn't
  reach it. Only Linux with Vulkan got this right; Windows, Mac and
  Linux's other drawing paths now do too. On Windows the menu's buttons
  also missed clicks.

## Known limits

- While a game holds the mouse, the button can't be pointed at;
  Ctrl+Alt+Shift+O opens the menu.
- The titlebar has been checked on Linux, not yet on Windows or a Mac.
- On a Mac, hosting keeps running the previous version until it next
  restarts.
- Windows builds remain unsigned (SmartScreen: *More info* → *Run anyway*).
- Networking is LAN or your own VPN only, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.4 — October 19, 2026

Your GitHub account is the only sign-in, and streaming works from the
Linux AppImage again.

## Pairing

- **Machines on your GitHub account pair with each other on their own.**
  No PIN, and nobody has to be at the other machine.
- **Dusk looks after the host's password itself.** There is nothing to
  sign in to apart from GitHub.
- **Let other devices pair** (Settings › Hosting) is off to begin with.
  Turn it on for phones, TVs or someone else's machine; each one then
  needs someone at this machine to allow it.

## Fixes

- **Streaming works from the Linux AppImage.** In v0.3.3 the streaming
  client inside the AppImage was damaged while it was being built, so every
  stream failed straight away. Hosting from the AppImage was broken the
  same way. The `.deb` was not affected.
- **A failed stream says why.** Dusk shows the reason the stream gave, or
  says that the streaming client crashed.

## Known limits

- This is the first update self-update delivers. A v0.3.3 Dusk offers it on
  its own; older versions need installing by hand.
- The titlebar has been checked on Linux, not yet on Windows or a Mac.
- On a Mac, hosting keeps running the previous version until it next
  restarts.
- Windows builds remain unsigned (SmartScreen: *More info* → *Run anyway*).
- Networking is LAN or your own VPN only, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.3 — October 19, 2026

Dusk has its own titlebar and now keeps itself up to date.

## Updates

- **Dusk updates itself.** It looks for a new version shortly after it
  opens and every few hours after, and downloads it in the background.
- **It never restarts on its own.** When an update is ready, **Restart to
  update** appears in the titlebar, so a stream or a hosted session is
  never cut off. **Settings › About** has **Check for updates**.
- **This version has to be installed by hand.** From here on, Dusk
  updates itself.
- Updates come to the macOS app, the Windows installers and the Linux
  AppImage. A copy installed from the `.deb` updates through your package
  manager.

## Titlebar

- **On Windows and Linux, Dusk draws its own titlebar** with minimise,
  maximise and close. Close still follows the Startup setting: to the
  tray, or quit.
- **On a Mac, the window buttons sit over Dusk's own bar.**

## Fixes

- **Opening Dusk from the tray looks for machines again**, so a Dusk that
  started in the tray no longer shows the list from when it started.
- **Release builds pick up their installers reliably.**

## Known limits

- Self-update has not yet carried an install from one release to the next;
  v0.3.4 will be the first.
- The titlebar has been checked on Linux, not yet on Windows or a Mac.
- On a Mac, hosting keeps running the previous version until it next
  restarts.
- Windows builds remain unsigned (SmartScreen: *More info* → *Run anyway*).
- Networking is LAN or your own VPN only, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.2 — October 19, 2026

Dusk can start when you sign in and wait in the tray.

## Startup

- **Open Dusk when you sign in**, in the new **Startup** section of
  Settings. On a Mac, Dusk has to be in Applications first.
- **Start in the tray** (the menu bar on a Mac) when opened at sign-in, so
  Dusk is ready to accept pairing and switch hosting without a window in
  the way.
- **Closing the window keeps Dusk running** in the tray by default. Turn
  this off to have closing the window quit Dusk.
- **Opening Dusk again shows the window** that is already running rather
  than starting a second copy.
- **On a Mac, Dusk's dock icon goes with its window.** It leaves the dock
  while Dusk waits in the menu bar or steps aside for a stream, and comes
  back with the window.

## Fixes

- **"Looking for machines" no longer stays on forever.** It shows for a few
  seconds when Dusk starts and after **Refresh**, while Dusk is actually
  looking. Dusk still notices a machine that appears later.

## Known limits

- The startup settings have been built for every platform but not yet run
  on each. Please report what you find.
- During a stream the stream's own window keeps its dock icon on a Mac.
- Hosting on a Mac has not been run end to end yet.
- Windows builds remain unsigned (SmartScreen: *More info* → *Run anyway*).
- Networking is LAN or your own VPN only, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

Version v0.3.1 — October 19, 2026

Hosting is now built into Dusk on macOS too.

## Hosting on macOS

- **Nothing to download.** Hosting ships inside Dusk as **Dusk Host**, a
  universal app for Apple Silicon and Intel, signed and notarized with Dusk.
  **Turn on hosting** registers it to start in the background when you sign
  in. Nothing is copied into Applications and there is no administrator
  prompt.
- **It shows as Dusk Host** in Screen Recording, Microphone and
  Accessibility, so the privacy steps name Dusk rather than another program.
- **Keep Dusk in Applications.** Hosting runs from inside Dusk, so Dusk asks
  to be moved out of the disk image first. If you move Dusk later, press
  **Turn on hosting** again to point hosting at the new place.

## Known limits

- Hosting on a Mac has not been run end to end yet; the build, signing and
  notarization are proven, the running service is not. Please report what
  you find.
- The macOS dock icon still does not step aside during a stream.
- Windows builds remain unsigned (SmartScreen: *More info* → *Run anyway*).
- Networking is LAN or your own VPN only, by design.

## Install

macOS: the `.dmg`, signed and notarized, universal. Windows: the `.msi` or
the `setup.exe`. Linux: the `.deb` where it fits, otherwise the `.AppImage`.

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
