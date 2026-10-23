import { useEffect, useState } from "react";
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
  const [note, setNote] = useState<string | null>(null);
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
      setNote(`${device.name} is now on version ${theirs}.`);
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
        setNote(`${device.name} says it already has the latest version.`);
      }
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="axi-stack">
      {dusk && (
        <p className="axi-ink-dim">
          Dusk {dusk.version}
          {behind && ` · this machine has ${mine}`}
        </p>
      )}
      {pending && !overdue && (
        <p className="axi-ink-dim">
          {device.name} is installing version {pending.version} and will restart by itself in a minute or two.
        </p>
      )}
      {pending && overdue && (
        <p className="axi-ink-dim">
          {device.name} hasn&rsquo;t come back on version {pending.version} yet. You can ask it again, or it may
          need someone at the machine.
        </p>
      )}
      {(!pending || overdue) &&
        behind &&
        (dusk.remoteUpdate ? (
          <>
            {device.streamingHere && (
              <p className="axi-ink-dim">Your stream will drop while {device.name} restarts.</p>
            )}
            <button
              type="button"
              className="axi-btn axi-btn--sm"
              disabled={busy}
              onClick={() => void update()}
            >
              {busy ? "Asking it to update" : "Update Dusk on this machine"}
            </button>
          </>
        ) : (
          <p className="axi-ink-dim">
            Dusk on {device.name} can&rsquo;t update itself from here. Someone at {device.name} has to install it.
          </p>
        ))}
      {note && <p className="axi-ink-dim">{note}</p>}
      {error && <p className="axi-ink-danger">{error}</p>}
    </div>
  );
}
