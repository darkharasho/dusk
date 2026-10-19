import { useEffect, useState } from "react";
import { onSessionStats, type StreamStats as Stats } from "../api";

/**
 * Live numbers for the stream running from one machine.
 *
 * Only Dusk's own client sends these, over the session channel, so with a
 * stock Moonlight this renders nothing rather than a panel of dashes that
 * looks broken. It also starts empty for each stream: the last session's
 * figures are not this one's.
 */
export function StreamStats({ deviceId }: { deviceId: string }) {
  const [stats, setStats] = useState<Stats | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    setStats(null);
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
    </>
  );
}

function fixed(value: number | null | undefined, digits: number, unit: string): string | null {
  return value == null ? null : `${value.toFixed(digits)}${unit}`;
}
