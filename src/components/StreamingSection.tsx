import { useState } from "react";
import { setBitrate, setCodec, setResolution } from "../api";
import type { Codec, Resolution, Snapshot } from "../types";
import { describe } from "../errors";

/** The choices, keyed by what the select holds. */
const CHOICES: { value: string; label: string; resolution: Resolution }[] = [
  { value: "host", label: "Match the host's screen", resolution: { kind: "matchHost" } },
  { value: "here", label: "Match this screen", resolution: { kind: "matchThisDisplay" } },
  { value: "720", label: "720p · 1280 × 720", resolution: { kind: "fixed", width: 1280, height: 720 } },
  { value: "1080", label: "1080p · 1920 × 1080", resolution: { kind: "fixed", width: 1920, height: 1080 } },
  { value: "1440", label: "1440p · 2560 × 1440", resolution: { kind: "fixed", width: 2560, height: 1440 } },
  { value: "2160", label: "4K · 3840 × 2160", resolution: { kind: "fixed", width: 3840, height: 2160 } },
];

function valueOf(resolution: Resolution): string {
  const match = CHOICES.find(
    (c) =>
      c.resolution.kind === resolution.kind &&
      (resolution.kind !== "fixed" ||
        (c.resolution.kind === "fixed" &&
          c.resolution.width === resolution.width &&
          c.resolution.height === resolution.height)),
  );
  return match?.value ?? "custom";
}

function detail(resolution: Resolution): string {
  switch (resolution.kind) {
    case "matchHost":
      return "Streams come at the size of the screen you are streaming, so the picture fills it edge to edge. A machine without Dusk streams at 1080p.";
    case "matchThisDisplay":
      return "Streams come at the size of the screen Dusk is on here. A host with a differently shaped screen gets bars down its sides.";
    case "fixed":
      return "Every stream comes at this size, whatever the screens at either end.";
  }
}

/** Fixed rates, in Mbps. Automatic is offered alongside. */
const BITRATES = [5, 10, 20, 30, 50, 80, 120, 150];

function bitrateDetail(kbps: number | null): string {
  if (kbps === null) {
    return "Each stream picks a rate to suit its size and frame rate. Set one yourself to go easier on a busy network, or to sharpen the picture on a fast one.";
  }
  return `Streams use up to ${kbps / 1000} Mbps. Lower it if the picture stutters or freezes; raise it if it looks blocky and your network has room.`;
}

const CODECS: { value: Codec; label: string; detail: string }[] = [
  { value: "auto", label: "Automatic", detail: "The two machines settle on the best format both can handle in hardware." },
  { value: "h264", label: "H.264", detail: "Every machine can play it, but it needs the most data for a sharp picture." },
  {
    value: "hevc",
    label: "HEVC (H.265)",
    detail: "A sharper picture than H.264 for the same data. Both machines need graphics hardware that supports it.",
  },
  {
    value: "av1",
    label: "AV1",
    detail: "The sharpest for the data it uses, on recent graphics hardware only. Both machines need to support it.",
  },
];

/** How streams from this machine look. */
export function StreamingSection({ snapshot }: { snapshot: Snapshot }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = snapshot.resolution;
  const value = valueOf(current);
  const bitrate = snapshot.bitrateKbps;
  // A rate set some other way still shows as itself.
  const bitrateCustom = bitrate !== null && !BITRATES.includes(bitrate / 1000);
  const codec = CODECS.find((c) => c.value === snapshot.codec) ?? CODECS[0];

  async function save(change: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await change();
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  function choose(next: string) {
    const choice = CHOICES.find((c) => c.value === next);
    if (choice) void save(() => setResolution(choice.resolution));
  }

  return (
    <div className="axi-panel axi-stack">
      <div className="dusk-setting">
        <div className="dusk-field">
          <label htmlFor="stream-resolution">Resolution</label>
          <select
            id="stream-resolution"
            className="axi-select"
            value={value}
            disabled={busy}
            onChange={(e) => choose(e.target.value)}
          >
            {/* A size set some other way still shows as itself, rather than
                as whichever option happens to come first. */}
            {value === "custom" && current.kind === "fixed" && (
              <option value="custom">
                {current.width} × {current.height}
              </option>
            )}
            {CHOICES.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        <p className="axi-ink-dim">{detail(current)}</p>
      </div>

      <div className="dusk-setting">
        <div className="dusk-field">
          <label htmlFor="stream-bitrate">Bitrate</label>
          <select
            id="stream-bitrate"
            className="axi-select"
            value={bitrate === null ? "auto" : String(bitrate)}
            disabled={busy}
            onChange={(e) => {
              const next = e.target.value === "auto" ? null : Number(e.target.value);
              void save(() => setBitrate(next));
            }}
          >
            <option value="auto">Automatic</option>
            {bitrateCustom && <option value={String(bitrate)}>{bitrate / 1000} Mbps</option>}
            {BITRATES.map((mbps) => (
              <option key={mbps} value={String(mbps * 1000)}>
                {mbps} Mbps
              </option>
            ))}
          </select>
        </div>
        <p className="axi-ink-dim">{bitrateDetail(bitrate)}</p>
      </div>

      <div className="dusk-setting">
        <div className="dusk-field">
          <label htmlFor="stream-codec">Video codec</label>
          <select
            id="stream-codec"
            className="axi-select"
            value={codec.value}
            disabled={busy}
            onChange={(e) => {
              const next = e.target.value as Codec;
              void save(() => setCodec(next));
            }}
          >
            {CODECS.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        <p className="axi-ink-dim">{codec.detail}</p>
      </div>
      {error && <p className="axi-ink-danger">{error}</p>}
    </div>
  );
}
