import type { Device } from "./types";

/** The four verdicts a card can carry. */
export type Tone = "hosting" | "ready" | "unpaired" | "offline";

export interface CardState {
  tone: Tone;
  label: string;
  /**
   * The ink for the card's status cap and its chip.
   *
   * Offline gets none of either: rule 2 forbids a muted ink, so the absence
   * of a status is drawn as the absence of colour rather than as a faded one.
   */
  strip: string | null;
  chipClass: string;
}

const TONES: Record<Tone, Pick<CardState, "strip" | "chipClass">> = {
  // Mid-session is the app's own ink — the sun in sunshine/moonlight.
  hosting: { strip: "var(--axi-accent)", chipClass: "axi-chip axi-chip--accent" },
  ready: { strip: "var(--axi-ok)", chipClass: "axi-chip axi-chip--ok" },
  unpaired: { strip: "var(--axi-warn)", chipClass: "axi-chip axi-chip--warn" },
  offline: { strip: null, chipClass: "axi-chip" },
};

/**
 * Collapse the three independent state axes (reachable / paired / busy) into
 * the one verdict the card shows. Order matters: a machine mid-session reads
 * as hosting regardless of how we are paired with it.
 */
export function cardState(device: Device): CardState {
  const of = (tone: Tone, label: string): CardState => ({
    tone,
    label,
    ...TONES[tone],
  });

  if (device.reachability.kind === "offline") return of("offline", "Offline");
  if (device.reachability.kind === "unknown") return of("offline", "Checking");

  if (device.activity.kind === "hosting") {
    const { appName } = device.activity;
    return of("hosting", appName ? `Streaming ${appName}` : "In a session");
  }

  switch (device.pairing.kind) {
    case "paired":
      return of("ready", "Ready");
    case "notPaired":
      return of("unpaired", "Not paired");
    // Pairing is only knowable with a client certificate, which Dusk does not
    // own until M2. "Online" is the weaker claim we can actually support; the
    // cap still reads as ok because reachable is a real status.
    case "unknown":
      return of("ready", "Online");
  }
}

export function originLabel(device: Device): string | null {
  const { mdns, manual } = device.source;
  if (mdns && manual) return "Added, discovered";
  if (manual) return "Added";
  return null;
}

/** Two letters for the card's glyph tile. */
export function initials(name: string): string {
  const words = name.split(/[\s._-]+/).filter(Boolean);
  if (words.length === 0) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}
