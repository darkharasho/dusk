import { useState } from "react";
import type { HostState, Snapshot } from "../types";
import {
  acceptPin,
  provisionHost,
  setAllowOtherDevices,
  setVirtualScreen,
  startHosting,
  stopHosting,
} from "../api";
import { Toggle } from "./StartupSection";
import { describe } from "../errors";

interface Props {
  name: string;
  snapshot: Snapshot;
  /** Where installing hosting actually happens, for a machine without it. */
  onSetup(): void;
}

type Panel = null | "pin";

function statusLine(host: HostState): string {
  switch (host.status.kind) {
    case "notInstalled":
      return "Hosting is not installed yet";
    case "installed":
      return host.status.running
        ? `Hosting is on${host.status.version ? ` · version ${host.status.version}` : ""}`
        : "Hosting is off";
    case "unknown":
      return host.status.reason;
  }
}

function virtualScreenDetail(snapshot: Snapshot): string {
  const mechanism = snapshot.virtualScreenMechanism;
  if (!snapshot.virtualScreen && mechanism === "none") {
    // The two platforms are short of different things, and "install the
    // driver" is no use to someone on Linux.
    return snapshot.host.platform === "linux"
      ? "Needs a screen Dusk can resize. A Wayland session cannot be, and nor can one with several screens and no primary."
      : "Needs the virtual display. Install it from Setup first.";
  }
  // On Linux there is only the one screen, and the person at the host sees
  // it change. Saying the monitors switch off would be plainly wrong there.
  if (mechanism === "hostOutput") {
    return snapshot.virtualScreen
      ? "Each stream gets a screen the size of the device watching it, with no bars. This machine's own screen changes to that size while it streams, and changes back afterwards."
      : "Give each stream a screen the size of the device watching it, so an ultrawide or a laptop fills edge to edge. This machine's own screen changes with it, to the nearest size it supports, and changes back afterwards.";
  }
  if (snapshot.virtualScreen) {
    return "Each stream gets a screen the size of the device watching it, with no bars. This machine's own monitors switch off while it streams.";
  }
  return "Give each stream a screen the size of the device watching it, so an ultrawide or a laptop fills edge to edge. This machine's own monitors switch off while it streams. Windows asks for permission once.";
}

/**
 * Hosting on the machine you are sitting at: whether it is on, and the
 * controls that act on it. Lives in Settings; the grid shows a summary.
 */
export function HostingPanel({ name, snapshot, onSetup }: Props) {
  const host = snapshot.host;
  const { status, capabilities } = host;
  const installed = status.kind === "installed";
  const running = status.kind === "installed" && status.running;

  // Three states, not two. Offering to take hosting over while the keystore
  // is still being read would replace a password Dusk already holds, so
  // until that read lands nothing is offered at all.
  const managed = snapshot.hostSignedIn === true;
  const managedKnown = snapshot.hostSignedIn !== null;

  const [panel, setPanel] = useState<Panel>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  async function run(action: () => Promise<void>, done?: string) {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await action();
      if (done) setNote(done);
      setPanel(null);
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="axi-panel axi-stack">
      <div className="axi-row">
        <div>
          <div className="axi-row">
            <strong>{name}</strong>
            {capabilities.supportTier === "experimental" && (
              <span
                className="axi-chip axi-chip--meta"
                title={capabilities.caveats.join(" ")}
              >
                Experimental host
              </span>
            )}
          </div>
          <p className="axi-ink-dim">{statusLine(host)}</p>
        </div>

        <div className="axi-row dusk-row__end">
          {installed && managed && snapshot.allowOtherDevices && (
            <button
              type="button"
              className="axi-btn"
              disabled={busy}
              onClick={() => setPanel(panel === "pin" ? null : "pin")}
            >
              Accept a PIN
            </button>
          )}
          {/* Dusk sets a host up on its own: on the way up when hosting is
              turned on, and at launch for one already running (except on
              Windows, where it would mean an unasked-for elevation prompt).
              This is what is left: a running host it has not reached yet. */}
          {running && managedKnown && !managed && (
            <button
              type="button"
              className="axi-btn"
              disabled={busy}
              onClick={() => run(provisionHost, "Hosting is set up.")}
            >
              {busy ? "Setting up" : "Finish setting up hosting"}
            </button>
          )}
          {/* Without this the panel is a dead end: it says hosting is not
              installed and every control on it is disabled, with the one
              screen that can install it unnamed. */}
          {status.kind === "notInstalled" ? (
            <button type="button" className="axi-btn" onClick={onSetup}>
              Go to setup
            </button>
          ) : (
            <button
              type="button"
              className="axi-btn"
              disabled={!installed || busy}
              onClick={() =>
                run(running ? stopHosting : startHosting)
              }
            >
              {running ? "Turn off hosting" : "Turn on hosting"}
            </button>
          )}
        </div>
      </div>

      {snapshot.hostRestart && (
        <div className="axi-notice">
          <span className="axi-notice__icon" aria-hidden="true">
            !
          </span>
          <p>{snapshot.hostRestart}</p>
        </div>
      )}

      {managed && !snapshot.hostCredentialsPersistent && (
        <p className="axi-ink-dim">
          {/* Never let someone believe a password was saved when it was not. */}
          No system keystore was available, so Dusk will have to set this host
          up again the next time it starts.
        </p>
      )}

      <Toggle
        id="allow-other-devices"
        label="Let other devices pair"
        detail={
          snapshot.allowOtherDevices
            ? "Someone else's Dusk can ask, and someone here presses Allow. Phones, TVs and other streaming apps pair with a PIN typed here."
            : "Only machines signed in to your GitHub account pair with this one, on their own. Turn this on for phones, TVs, or someone else's machine; each then needs someone here to allow it."
        }
        on={snapshot.allowOtherDevices}
        disabled={busy}
        onChange={(on) => run(() => setAllowOtherDevices(on))}
      />

      {capabilities.sizedScreen && installed && managed && (
        <Toggle
          id="virtual-screen"
          label="Stream a screen sized to the other device"
          detail={virtualScreenDetail(snapshot)}
          on={snapshot.virtualScreen}
          disabled={
            busy ||
            !running ||
            (!snapshot.virtualScreen && snapshot.virtualScreenMechanism === "none")
          }
          onChange={(on) =>
            run(
              () => setVirtualScreen(on),
              on
                ? "Streams now show a screen the other device's size."
                : "Streams show this machine's screen again.",
            )
          }
        />
      )}

      {panel === "pin" && snapshot.allowOtherDevices && (
        <AcceptPinPanel
          busy={busy}
          onSubmit={(pin, deviceName) =>
            run(() => acceptPin(pin, deviceName), "Machine paired.")
          }
        />
      )}

      {note && <p className="axi-ink-ok">{note}</p>}
      {error && (
        <div className="axi-notice axi-notice--danger">
          <span className="axi-notice__icon" aria-hidden="true">
            !
          </span>
          <p>{error}</p>
        </div>
      )}
    </div>
  );
}

function AcceptPinPanel({
  busy,
  onSubmit,
}: {
  busy: boolean;
  onSubmit(pin: string, deviceName?: string): void;
}) {
  const [pin, setPin] = useState("");
  const [deviceName, setDeviceName] = useState("");

  return (
    <form
      className="axi-well axi-stack"
      onSubmit={(e) => {
        e.preventDefault();
        onSubmit(pin, deviceName || undefined);
      }}
    >
      <p className="axi-ink-dim">
        When another machine asks to pair with this one, type the PIN it shows
        here.
      </p>
      <div className="dusk-field">
        <label htmlFor="accept-pin">PIN</label>
        <input
          id="accept-pin"
          className="axi-input"
          value={pin}
          inputMode="numeric"
          maxLength={4}
          placeholder="0000"
          onChange={(e) => setPin(e.target.value.replace(/\D/g, ""))}
        />
      </div>
      <div className="dusk-field">
        <label htmlFor="accept-name">Name this machine (optional)</label>
        <input
          id="accept-name"
          className="axi-input"
          value={deviceName}
          placeholder="Living room PC"
          onChange={(e) => setDeviceName(e.target.value)}
        />
      </div>
      <div className="axi-row">
        <button
          type="submit"
          className="axi-btn axi-btn--primary"
          disabled={busy || pin.length !== 4}
        >
          {busy ? "Sending" : "Accept PIN"}
        </button>
      </div>
    </form>
  );
}
