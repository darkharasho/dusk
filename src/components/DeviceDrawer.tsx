import { useEffect, useRef, useState } from "react";
import type { Codec, Device, DeviceStream, Resolution } from "../types";
import { DeviceStreamSection } from "./DeviceStreamSection";
import { cardState, pairingOf } from "../deviceState";
import {
  generatePin,
  launchApp,
  onPairProgress,
  onClipboardUnavailable,
  onSessionFailed,
  pairDevice,
  quitSession,
  type PairProgress,
} from "../api";
import { StreamStats } from "./StreamStats";
import { RemoteUpdate } from "./RemoteUpdate";
import { describe } from "../errors";
import { OVERLAY_HOTKEY } from "../hotkeys";

interface Props {
  device: Device;
  moonlightAvailable: boolean;
  overlayAvailable: boolean | null;
  /** This machine's streaming overrides, and the globals they fall back to. */
  deviceStream: DeviceStream | undefined;
  streamDefaults: {
    resolution: Resolution;
    bitrateKbps: number | null;
    fps: number | null;
    codec: Codec;
    freeCursor: boolean;
  };
  onClose(): void;
}

type Busy = null | "pairing" | "launching" | "quitting";

/**
 * How to reach the in-stream controls of the stream that is up, given the
 * pointer mode it launched with.
 *
 * The hotkey leads in every case because it is the one way in that works in
 * both modes. Whether the floating handle answers a click does not: a stream
 * holding the pointer locks the cursor, so there is nothing to aim with and
 * the client hit-tests nothing. `null` is a staged session with no client
 * behind it, where the handle is not ours to promise either way.
 */
export function overlayReach(freeCursor: boolean | null): string {
  const press = `Press ${OVERLAY_HOTKEY} in the stream for the in-stream controls.`;
  switch (freeCursor) {
    case true:
      return `${press} Or click the floating handle — this stream is letting the pointer leave.`;
    case false:
      return `${press} This stream is holding the pointer, so the floating handle cannot be clicked; Streaming settings can let it leave.`;
    default:
      return press;
  }
}

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
  deviceStream,
  streamDefaults,
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

  // Once the other machine has said yes and the pairing is saved on both
  // sides, what is left is waiting for its stream to end, which can be
  // hours. Nothing here is needed for that, so the drawer may close; the
  // card shows Ready when the host is free. Anything else that is busy
  // keeps the drawer open, the same from every way of closing it.
  const canClose = busy === null || (busy === "pairing" && stage === "waitingForHost");

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && canClose) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, canClose]);

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

  // Why the clipboard is not being shared, said by the stream's clipboard
  // connection when the machine streamed from cannot do it. It belongs to one
  // session, so it goes when that session does. The listener is registered
  // once per machine and reads whether it is streaming here through a ref:
  // registering again whenever that changed left a gap a quick decline,
  // which comes right as the stream starts, could fall into.
  const [clipboardNote, setClipboardNote] = useState<string | null>(null);
  const streamingHere = useRef(device.streamingHere);
  streamingHere.current = device.streamingHere;
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onClipboardUnavailable((note) => {
      if (!cancelled && note.deviceId === device.id && streamingHere.current)
        setClipboardNote(note.message);
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
    // A note belongs to one session: gone when it ends, absent when the next starts.
    setClipboardNote(null);
  }, [device.streamingHere]);

  const { label, chipClass } = cardState(device);
  const online = device.reachability.kind === "online";
  const pairing = pairingOf(device);
  const paired = pairing === "paired";
  const hosting = device.activity.kind === "hosting";

  async function run(kind: Exclude<Busy, null>, action: () => Promise<void>) {
    setBusy(kind);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(describe(err));
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
      <div className="axi-scrim" onClick={() => canClose && onClose()} />
      <aside className="axi-drawer" role="dialog" aria-label={device.name}>
        <div className="axi-drawer__head">
          <h2>{device.name}</h2>
          <button
            type="button"
            className="axi-drawer__close"
            disabled={!canClose}
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        <div className="axi-drawer__body axi-stack">
          <div className="axi-row">
            <span className={chipClass}>{label}</span>
            {/* Meta ink, and meta ink whatever the version turns out to be:
                the chip answers what the machine is on, and whether that is
                worth doing something about is the notice's job below. Two
                filled chips here would compete, and the state chip is the
                one that should win. */}
            {device.dusk && (
              <span className="axi-chip axi-chip--meta">Dusk {device.dusk.version}</span>
            )}
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

          {online && <RemoteUpdate key={device.id} device={device} />}

          {/* The port everything else here goes through is dead while the
              machine is plainly up. Said before any of it is offered: a
              Stream or End button would only time out, and "Checking"
              would never finish. */}
          {online && device.securePortDown && (
            <div className="axi-notice axi-notice--warn">
              <span className="axi-notice__icon" aria-hidden="true">
                !
              </span>
              <p>
                {device.name} is up, but its hosting has stopped answering
                securely, so Dusk cannot pair with it, stream from it, or end
                its session. This usually means a session got stuck starting
                there. Dusk on {device.name} restarts hosting on its own once
                it notices; if this stays, turn hosting off and on again
                there.
              </p>
            </div>
          )}

          {/* Not knowing is not "not paired": a host that answered only on
              its plain port this tick says nothing either way, and offering
              to pair with one already paired ends in an error. */}
          {online &&
            moonlightAvailable &&
            pairing === "unknown" &&
            !device.securePortDown &&
            busy !== "pairing" && (
              <p className="axi-ink-dim">Checking whether this machine is paired.</p>
            )}

          {online && moonlightAvailable && (pairing === "notPaired" || busy === "pairing") && (
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
              {/* A host holds its session open after the client goes
                  away, so this is routinely a session nobody is in.
                  Saying so is the difference between a control that
                  looks wrong and one that explains itself. */}
              {hosting && !device.streamingHere && (
                <p className="axi-ink-dim">
                  Disconnected. The session is still running on this machine,
                  and connecting from any of your devices picks it up.
                </p>
              )}

              {/* Keyed off our own session rather than the host's, which is
                  the grid's last poll and so up to five seconds old. Nested
                  under that flag, a stream we are in lost its stats and its
                  End control for the moment the host had not caught up —
                  the one moment they are most wanted. What we are streaming
                  is not something the host has to confirm. */}
              {device.streamingHere && <StreamStats deviceId={device.id} />}
              {device.streamingHere && clipboardNote && (
                <p className="axi-ink-dim">{clipboardNote}</p>
              )}

              {(hosting || device.streamingHere) && (
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

              {/* The other half of the same thing, and the more useful half:
                  when the controls do exist, how to reach them. The floating
                  handle looks like the way in and is not always clickable —
                  a stream that holds the pointer locks the cursor, so there
                  is nothing to aim at it with and the client hit-tests
                  nothing. The hotkey works in either mode, which is why it
                  leads. Keyed to what this stream launched with rather than
                  to the setting, which can have moved under it. */}
              {device.streamingHere && overlayAvailable !== false && (
                <p className="axi-ink-dim">{overlayReach(device.streamFreeCursor)}</p>
              )}

              <h3 className="axi-eyebrow">Apps</h3>
              {device.apps.length === 0 ? (
                <p className="axi-ink-dim">Reading the app list.</p>
              ) : (
                <div className="axi-stack dusk-list">
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

              <DeviceStreamSection
                deviceId={device.id}
                override={deviceStream}
                defaults={streamDefaults}
              />
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
          while it&rsquo;s streaming. You can close this; the card will show
          Ready when it&rsquo;s done.
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
