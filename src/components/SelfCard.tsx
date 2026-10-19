import { useState } from "react";
import type { HostState, Snapshot } from "../types";
import { acceptPin, provisionHost, startHosting, stopHosting } from "../api";

interface Props {
  name: string;
  snapshot: Snapshot;
  onOpenSettings(): void;
  onOpenSetup(): void;
}

type Panel = null | "pin";

function statusLine(host: HostState): string {
  switch (host.status.kind) {
    case "notInstalled":
      return "Sunshine is not installed yet";
    case "installed":
      return host.status.running
        ? `Hosting is on${host.status.version ? ` · Sunshine ${host.status.version}` : ""}`
        : "Hosting is off";
    case "unknown":
      return host.status.reason;
  }
}

/**
 * The machine you are sitting at. It is the only one that can host, so it is
 * a panel rather than one tile among equals in the grid.
 */
export function SelfCard({
  name,
  snapshot,
  onOpenSettings,
  onOpenSetup,
}: Props) {
  const host = snapshot.host;
  const { status, capabilities } = host;
  const installed = status.kind === "installed";
  const running = status.kind === "installed" && status.running;

  // Three states, not two. Offering to take Sunshine over while the keystore
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
      setError(String(err));
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
          <button type="button" className="axi-btn" onClick={onOpenSetup}>
            Set up hosting
          </button>
          {installed && managed && (
            <button type="button" className="axi-btn" onClick={onOpenSettings}>
              Settings
            </button>
          )}
          {installed && managed && (
            <button
              type="button"
              className="axi-btn"
              disabled={busy}
              onClick={() => setPanel(panel === "pin" ? null : "pin")}
            >
              Accept a PIN
            </button>
          )}
          {installed && managedKnown && !managed && (
            <button
              type="button"
              className="axi-btn"
              disabled={busy}
              title="Dusk gives Sunshine a sign-in of its own. Any password set in Sunshine's web page stops working."
              onClick={() => run(provisionHost, "Dusk is managing this host now.")}
            >
              {busy ? "Setting up" : "Let Dusk manage this host"}
            </button>
          )}
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
        </div>
      </div>

      {managed && !snapshot.hostCredentialsPersistent && (
        <p className="axi-ink-dim">
          {/* Never let someone believe a password was saved when it was not. */}
          No system keystore was available, so Dusk will have to set this host
          up again the next time it starts.
        </p>
      )}

      {panel === "pin" && (
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
        here. This used to mean opening Sunshine&rsquo;s web page.
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
