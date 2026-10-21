import { useEffect, useState } from "react";
import type { Device } from "../types";
import { cardState } from "../deviceState";
import {
  generatePin,
  launchApp,
  onPairProgress,
  onSessionFailed,
  pairDevice,
  quitSession,
  type PairProgress,
} from "../api";
import { StreamStats } from "./StreamStats";

interface Props {
  device: Device;
  moonlightAvailable: boolean;
  overlayAvailable: boolean | null;
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
export function DeviceDrawer({
  device,
  moonlightAvailable,
  overlayAvailable,
  onClose,
}: Props) {
  const [busy, setBusy] = useState<Busy>(null);
  const [pin, setPin] = useState<string | null>(null);
  const [stage, setStage] = useState<PairProgress["stage"] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Whether the other machine's Dusk took the request, or nobody answered
  // and the PIN has to be typed there. Only the backend can know.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onPairProgress((p) => {
      if (!cancelled && p.deviceId === device.id) setStage(p.stage);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [device.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && busy === null) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  // A stream can die well after the launch call resolved, so its reason
  // arrives as an event rather than a rejected promise. Same notice either
  // way: from here it is one failure, however late it turned up.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onSessionFailed((failure) => {
      if (!cancelled && failure.deviceId === device.id) setError(failure.message);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [device.id]);

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
      setStage(null);
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
                Dusk&rsquo;s streaming client is missing, so it cannot pair or
                stream yet. Reinstalling Dusk should fix this.
              </p>
            </div>
          )}

          {!online && <p className="axi-ink-dim">This machine is not answering.</p>}

          {online && moonlightAvailable && !paired && (
            <PairPanel
              name={device.name}
              pin={pin}
              stage={stage}
              pairing={busy === "pairing"}
              onPair={startPairing}
            />
          )}

          {online && paired && (
            <>
              {hosting && (
                <>
                  {/* A host holds its session open after the client goes
                      away, so this is routinely a session nobody is in.
                      Saying so is the difference between a control that
                      looks wrong and one that explains itself. */}
                  {!device.streamingHere && (
                    <p className="axi-ink-dim">
                      This machine is holding a session open with no client
                      attached. Streaming will resume it.
                    </p>
                  )}
                  {device.streamingHere && <StreamStats deviceId={device.id} />}
                  <button
                    type="button"
                    className="axi-btn axi-btn--sm dusk-drawer__end"
                    disabled={busy !== null}
                    onClick={() => run("quitting", () => quitSession(device.id))}
                  >
                    {busy === "quitting"
                      ? "Ending session"
                      : device.streamingHere
                        ? "End the session"
                        : "End it on the host"}
                  </button>
                </>
              )}

              {/* Said here rather than only in the log. A stock Moonlight
                  streams perfectly well and simply has no floating
                  controls, which is indistinguishable from an overlay that
                  is broken unless Dusk names which client it got. */}
              {overlayAvailable === false && (
                <p className="axi-ink-dim">
                  The in-stream controls are unavailable with this streaming
                  client, so ending a session means coming back here.
                </p>
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
  name,
  pin,
  stage,
  pairing,
  onPair,
}: {
  name: string;
  pin: string | null;
  stage: PairProgress["stage"] | null;
  pairing: boolean;
  onPair(): void;
}) {
  if (!pairing) {
    return (
      <div className="axi-stack">
        <p className="axi-ink-dim">
          Dusk is not paired with this machine yet. When both are signed in
          to the same GitHub account, it pairs on its own.
        </p>
        <button type="button" className="axi-btn axi-btn--primary" onClick={onPair}>
          Pair with this machine
        </button>
      </div>
    );
  }

  if (stage === "manual") {
    return (
      <div className="axi-stack">
        <p>Type this PIN on {name} to finish pairing.</p>
        <p className="dusk-pin">{pin}</p>
        <p className="axi-ink-dim">
          {name} did not answer as Dusk, so it cannot be asked directly. In
          Dusk, that is Settings › Hosting › Accept a PIN, once Let other
          devices pair is on.
        </p>
      </div>
    );
  }

  if (stage === "clientOpen") {
    return (
      <div className="axi-stack">
        <p>Close the stream window to finish pairing.</p>
        <p className="axi-ink-dim">
          {name} has already said yes. Dusk finishes as soon as the window is
          closed.
        </p>
      </div>
    );
  }

  if (stage === "waitingForHost") {
    return (
      <div className="axi-stack">
        <p>{name} is in use. Pairing will finish when it&rsquo;s free.</p>
        <p className="axi-ink-dim">
          {name} has already said yes. It just can&rsquo;t take a new machine
          while it&rsquo;s streaming.
        </p>
      </div>
    );
  }

  return (
    <div className="axi-stack">
      <p>
        {stage === "waitingForApproval"
          ? `Waiting for someone at ${name} to press Allow.`
          : `Asking ${name}.`}
      </p>
      {stage === "waitingForApproval" && (
        <p className="axi-ink-dim">
          {name} lets other devices pair, and this machine is not on its
          GitHub account, so someone there has to allow it.
        </p>
      )}
    </div>
  );
}
