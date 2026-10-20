import { useState } from "react";
import { setResolution } from "../api";
import type { Resolution, Snapshot } from "../types";

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

/** How streams from this machine look. */
export function StreamingSection({ snapshot }: { snapshot: Snapshot }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = snapshot.resolution;
  const value = valueOf(current);

  async function choose(next: string) {
    const choice = CHOICES.find((c) => c.value === next);
    if (!choice) return;
    setBusy(true);
    setError(null);
    try {
      await setResolution(choice.resolution);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="axi-panel axi-stack">
      <div className="dusk-field">
        <label htmlFor="stream-resolution">Resolution</label>
        <select
          id="stream-resolution"
          className="axi-select"
          value={value}
          disabled={busy}
          onChange={(e) => void choose(e.target.value)}
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
      {error && <p className="axi-ink-danger">{error}</p>}
    </div>
  );
}
