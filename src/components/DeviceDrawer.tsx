import { useEffect, useState } from "react";
import type { Device } from "../types";
import { cardState } from "../deviceState";
import { generatePin, launchApp, pairDevice, quitSession } from "../api";

interface Props {
  device: Device;
  moonlightAvailable: boolean;
  onClose(): void;
}

type Busy = null | "pairing" | "launching" | "quitting";

/**
 * One machine, opened.
 *
 * Everything you can do to a device lives here rather than on the card: the
 * grid's job is to let you find the machine you came for, and a card with
 * four buttons on it stops being scannable.
 */
export function DeviceDrawer({ device, moonlightAvailable, onClose }: Props) {
  const [busy, setBusy] = useState<Busy>(null);
  const [pin, setPin] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && busy === null) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  const { label, chipClass } = cardState(device);
  const online = device.reachability.kind === "online";
  const paired = device.pairing.kind === "paired";
  const hosting = device.activity.kind === "hosting";

  async function run(kind: Exclude<Busy, null>, action: () => Promise<void>) {
    setBusy(kind);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
      setPin(null);
    }
  }

  function startPairing() {
    const generated = generatePin();
    setPin(generated);
    void run("pairing", () => pairDevice(device.id, generated));
  }

  return (
    <>
      <div className="axi-scrim" onClick={() => busy === null && onClose()} />
      <aside className="axi-drawer" role="dialog" aria-label={device.name}>
        <div className="axi-drawer__head">
          <h2>{device.name}</h2>
          <button
            type="button"
            className="axi-drawer__close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        <div className="axi-drawer__body axi-stack">
          <div className="axi-row">
            <span className={chipClass}>{label}</span>
            <span className="axi-ink-faint dusk-card__address">
              {device.primaryAddress ?? device.addresses[0] ?? "No address"}
            </span>
          </div>

          {!moonlightAvailable && (
            <div className="axi-notice axi-notice--warn">
              {/* The variant colours this element, so a notice without one
                  renders as a plain box and loses its status. */}
              <span className="axi-notice__icon" aria-hidden="true">
                !
              </span>
              <p>
                Moonlight is not installed, so Dusk cannot pair or stream yet.
                Install it, or point Dusk at it with{" "}
                <code className="axi-code">DUSK_MOONLIGHT_BIN</code>.
              </p>
            </div>
          )}

          {!online && <p className="axi-ink-dim">This machine is not answering.</p>}

          {online && moonlightAvailable && !paired && (
            <PairPanel pin={pin} pairing={busy === "pairing"} onPair={startPairing} />
          )}

          {online && paired && (
            <>
              {hosting && (
                <button
                  type="button"
                  className="axi-btn"
                  disabled={busy !== null}
                  onClick={() => run("quitting", () => quitSession(device.id))}
                >
                  {busy === "quitting" ? "Ending session" : "End the session"}
                </button>
              )}

              <h3 className="axi-eyebrow">Apps</h3>
              {device.apps.length === 0 ? (
                <p className="axi-ink-dim">Reading the app list.</p>
              ) : (
                <div className="axi-stack" style={{ "--axi-stack-gap": "8px" } as React.CSSProperties}>
                  {device.apps.map((app) => (
                    <div className="axi-panel axi-panel--tile axi-row" key={app.id}>
                      <span>{app.name}</span>
                      <button
                        type="button"
                        className="axi-btn axi-btn--sm dusk-row__end"
                        disabled={busy !== null}
                        onClick={() => run("launching", () => launchApp(device.id, app.id))}
                      >
                        Stream
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}

          {error && (
            <div className="axi-notice axi-notice--danger">
              <span className="axi-notice__icon" aria-hidden="true">
                !
              </span>
              <p>{error}</p>
            </div>
          )}
        </div>
      </aside>
    </>
  );
}

function PairPanel({
  pin,
  pairing,
  onPair,
}: {
  pin: string | null;
  pairing: boolean;
  onPair(): void;
}) {
  if (!pairing) {
    return (
      <div className="axi-stack">
        <p className="axi-ink-dim">
          Dusk is not paired with this machine yet. Pairing shows a PIN you
          confirm on the other machine.
        </p>
        <button type="button" className="axi-btn axi-btn--primary" onClick={onPair}>
          Pair with this machine
        </button>
      </div>
    );
  }

  return (
    <div className="axi-stack">
      <p>Type this PIN on the other machine to finish pairing.</p>
      <p className="dusk-pin">{pin}</p>
      <p className="axi-ink-dim">
        {/* Honest about the seam: accepting the PIN natively is M3's job,
            and until then this is still two programs. */}
        For now that means Sunshine&rsquo;s own web page on the host. Dusk will
        accept the PIN itself once it manages the host side.
      </p>
    </div>
  );
}
