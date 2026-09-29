import type { CSSProperties } from "react";
import type { Device } from "../types";
import { cardState, initials, originLabel } from "../deviceState";

interface Props {
  device: Device;
  onOpen(device: Device): void;
}

/**
 * One machine.
 *
 * Status is a cap across the head of the card, not a stripe down its edge —
 * a full-height stripe reads as the card's border, and a grid of them becomes
 * a grid of coloured frames that say nothing about any one machine.
 */
export function DeviceCard({ device, onOpen }: Props) {
  const { label, strip, chipClass } = cardState(device);
  const origin = originLabel(device);
  const address = device.primaryAddress ?? device.addresses[0] ?? "No address";

  return (
    <button
      type="button"
      className={`axi-card${strip ? " axi-card--strip" : ""}`}
      style={strip ? ({ "--axi-card-strip": strip } as CSSProperties) : undefined}
      onClick={() => onOpen(device)}
    >
      <div className="axi-card__head">
        <span className="axi-card__glyph" aria-hidden="true">
          {initials(device.name)}
        </span>
        <span className="axi-card__title">
          <span className="axi-card__name">{device.name}</span>
          {origin && <span className="axi-card__kind">{origin}</span>}
        </span>
      </div>

      <p className="dusk-card__address">{address}</p>

      <div className="axi-card__meta">
        <span className={chipClass}>{label}</span>
        {device.apps.length > 0 && (
          <span>{device.apps.length} apps</span>
        )}
      </div>
    </button>
  );
}
