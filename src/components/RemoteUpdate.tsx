import { useEffect, useState, type ReactNode } from "react";
import { getVersion } from "@tauri-apps/api/app";
import type { Device } from "../types";
import { updateDevice } from "../api";
import { describe } from "../errors";
import { isOlder } from "../versions";

/** How long a machine may take to come back on its new version. */
const COMEBACK_MS = 10 * 60 * 1000;

/**
 * Machines asked to update, by device id, and when. Outside the component
 * so closing the drawer and opening it again still shows the update going
 * on rather than offering the button a second time.
 */
const asked = new Map<string, Pending>();

interface Pending {
  /** The version it was on when asked. */
  from: string;
  /** The version it said it was installing. */
  version: string;
  at: number;
}

let ours: Promise<string> | null = null;
function ourVersion(): Promise<string> {
  ours ??= getVersion();
  return ours;
}

/**
 * Which Dusk another of your machines runs, and a way to bring it up to
 * this one's without walking over to it.
 */
export function RemoteUpdate({ device }: { device: Device }) {
  const [mine, setMine] = useState<string | null>(null);
  const [pending, setPending] = useState(() => asked.get(device.id) ?? null);
  const [busy, setBusy] = useState(false);
  // A title and a detail rather than one sentence: the notice draws them
  // differently, and the title alone has to be readable.
  const [note, setNote] = useState<{ title: string; detail: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    ourVersion().then(setMine).catch(() => {});
  }, []);

  const theirs = device.dusk?.version ?? null;

  // Back on a different version from the one it was on: done, whether it
  // is the one it was installing or one released since.
  useEffect(() => {
    if (pending && theirs && theirs !== pending.from) {
      asked.delete(device.id);
      setPending(null);
      setNote({
        title: `Updated to ${theirs}`,
        detail: `${device.name} restarted and came back on the new version.`,
      });
    }
  }, [pending, theirs, device.id, device.name]);

  // Ticks only while waiting, so the "taking longer" line can appear.
  useEffect(() => {
    if (!pending) return;
    const timer = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(timer);
  }, [pending]);

  // A machine restarting into its update says nothing for a moment, and
  // the line saying it is on its way should not vanish while it does.
  const dusk = device.dusk;
  if (!device.mine || !mine || (!dusk && !pending)) return null;
  const behind = dusk !== undefined && isOlder(dusk.version, mine);
  const overdue = pending !== null && now - pending.at >= COMEBACK_MS;

  async function update() {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      const outcome = await updateDevice(device.id);
      if (outcome.kind === "updating") {
        const entry = { from: device.dusk?.version ?? "", version: outcome.version, at: Date.now() };
        asked.set(device.id, entry);
        setPending(entry);
        setNow(entry.at);
      } else {
        setNote({
          title: 'Already up to date',
          detail: `${device.name} says it already has the latest version.`,
        });
      }
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="axi-stack">
      {pending && !overdue && (
        <Notice icon="↻" title={`Installing ${pending.version}`}>
          {device.name} will restart by itself in a minute or two.
        </Notice>
      )}

      {pending && overdue && (
        <Notice variant="danger" icon="!" title={`${device.name} hasn’t come back`}>
          It never returned on {pending.version}. You can ask it again, or someone may need to
          be at the machine.
        </Notice>
      )}

      {(!pending || overdue) &&
        behind &&
        (dusk.remoteUpdate ? (
          <Notice variant="warn" icon="↑" title={`${mine} is available`}>
            {device.name} is on {dusk.version}.
            {device.streamingHere && " Your stream will drop while it restarts."}
            <div className="axi-row" style={{ marginTop: 10 }}>
              <button
                type="button"
                className="axi-btn axi-btn--sm axi-btn--primary"
                disabled={busy}
                onClick={() => void update()}
              >
                {busy ? "Asking it to update" : `Update ${device.name}`}
              </button>
            </div>
          </Notice>
        ) : (
          <Notice variant="warn" icon="↑" title={`${mine} is available`}>
            Dusk on {device.name} can&rsquo;t update itself from here. Someone at the machine has
            to install it.
          </Notice>
        ))}

      {note && (
        <Notice variant="ok" icon="✓" title={note.title}>
          {note.detail}
        </Notice>
      )}

      {error && (
        <Notice variant="danger" icon="!" title="That didn’t work">
          {error}
        </Notice>
      )}
    </div>
  );
}

/**
 * One outcome, drawn as the language draws outcomes.
 *
 * The variant is what colours an `axi-notice`, so one without a variant is a
 * plain box rather than a quieter status — which is why the installing case
 * passes none on purpose and the rest always pass one.
 *
 * The icon is a character rather than a drawn glyph, matching the notice
 * already in the drawer. Rule 12 asks for the language's own angles at
 * control weight, so these are a stand-in until the language offers them.
 */
function Notice({
  variant,
  icon,
  title,
  children,
}: {
  variant?: "ok" | "warn" | "danger";
  icon: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <div className={variant ? `axi-notice axi-notice--${variant}` : "axi-notice"}>
      <span className="axi-notice__icon" aria-hidden="true">
        {icon}
      </span>
      <div>
        <p style={{ margin: 0 }}>{title}</p>
        <div className="axi-ink-dim" style={{ marginTop: 4 }}>
          {children}
        </div>
      </div>
    </div>
  );
}
