import { useEffect, useState } from "react";
import { leaveStream, onSessionStats, type StreamStats as Stats } from "../api";
import { describe } from "../errors";

/**
 * Live numbers for the stream running from one machine.
 *
 * Only Dusk's own client sends these, over the session channel, so with a
 * stock Moonlight this renders nothing rather than a panel of dashes that
 * looks broken. It also starts empty for each stream: the last session's
 * figures are not this one's.
 *
 * The same goes for leaving the stream from here: it is asked of the client
 * over that channel, so it is only offered once the client has spoken on it.
 */
export function StreamStats({ deviceId }: { deviceId: string }) {
  const [stats, setStats] = useState<Stats | null>(null);
  const [leaving, setLeaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    setStats(null);
    setLeaving(false);
    setError(null);
    onSessionStats((update) => {
      if (!cancelled && update.deviceId === deviceId) setStats(update.stats);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [deviceId]);

  if (stats === null) return null;

  async function leave() {
    setLeaving(true);
    setError(null);
    try {
      await leaveStream(deviceId);
    } catch (err) {
      setError(describe(err));
      setLeaving(false);
    }
  }

  const rows: [string, string | null][] = [
    ["Frame rate", fixed(stats.fps, 0, " fps")],
    ["Bitrate", stats.bitrateKbps == null ? null : `${(stats.bitrateKbps / 1000).toFixed(1)} Mbps`],
    ["Network latency", fixed(stats.rttMs, 0, " ms")],
    ["Decode time", fixed(stats.decodeMs, 1, " ms")],
    ["Frames lost", fixed(stats.packetLoss, 1, "%")],
  ];

  return (
    <>
      <h3 className="axi-eyebrow">This stream</h3>
      <dl className="axi-readout">
        {rows.map(([label, value]) =>
          value === null ? null : (
            <div className="axi-readout__row" key={label}>
              <dt className="axi-readout__k">{label}</dt>
              <dd className="axi-readout__v">{value}</dd>
            </div>
          ),
        )}
      </dl>
      {/* Not the same as ending the session below: this drops the stream
          and leaves the game running on the host, to pick up again later. */}
      <div className="axi-row">
        <button type="button" className="axi-btn" disabled={leaving} onClick={leave}>
          {leaving ? "Leaving the stream" : "Leave the stream"}
        </button>
        <span className="axi-ink-faint">The host keeps running what you were playing.</span>
      </div>
      {error && <p className="axi-ink-dim">{error}</p>}
    </>
  );
}

function fixed(value: number | null | undefined, digits: number, unit: string): string | null {
  return value == null ? null : `${value.toFixed(digits)}${unit}`;
}
