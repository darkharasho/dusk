import { useState } from "react";
import { setDeviceStream } from "../api";
import type { Codec, DeviceStream, Resolution } from "../types";
import { describe } from "../errors";
import { BITRATES, CHOICES, CODECS, FRAME_RATES, valueOf } from "./StreamingSection";

/** The global default, spelled the way its "Use default" option reads. */
function resolutionDefault(resolution: Resolution): string {
  const v = valueOf(resolution);
  return CHOICES.find((c) => c.value === v)?.label ?? "a custom size";
}

function bitrateDefault(kbps: number | null): string {
  return kbps === null ? "Automatic" : `${kbps / 1000} Mbps`;
}

function fpsDefault(fps: number | null): string {
  return fps === null ? "Automatic" : `${fps} fps`;
}

function codecDefault(codec: Codec): string {
  return CODECS.find((c) => c.value === codec)?.label ?? "Automatic";
}

/**
 * What "Use default" means for the pointer, which is the one control here
 * whose default is not simply the global value.
 *
 * Spelled out because the surprising half is silent otherwise: a game holds
 * the pointer even with the global switch on, and the only visible sign is a
 * camera that turns. Naming both halves is what stops that reading as a bug.
 */
export function cursorOverrideDetail(global: boolean): string {
  return global
    ? "The desktop lets the pointer leave, as the global setting says; everything else holds it, because a game that turns the camera with the mouse cannot use a free pointer."
    : "Nothing lets the pointer leave while the global setting is off.";
}

interface Props {
  deviceId: string;
  /** This machine's current overrides, or undefined when it has none. */
  override: DeviceStream | undefined;
  /** The global Streaming settings each field falls back to. */
  defaults: {
    resolution: Resolution;
    bitrateKbps: number | null;
    fps: number | null;
    codec: Codec;
    freeCursor: boolean;
  };
}

/**
 * Streaming settings for one machine, over the global ones.
 *
 * Every control starts at "Use default", which shows the global value in
 * parentheses, and inherits it until set. Setting one overrides it for this
 * machine alone — so a fast host can be pinned to 120 fps and HEVC while the
 * rest stay on the defaults.
 */
export function DeviceStreamSection({ deviceId, override, defaults }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const current: DeviceStream = {
    resolution: override?.resolution ?? null,
    bitrateKbps: override?.bitrateKbps ?? null,
    fps: override?.fps ?? null,
    codec: override?.codec ?? null,
    freeCursor: override?.freeCursor ?? null,
  };

  function change(patch: Partial<DeviceStream>) {
    setBusy(true);
    setError(null);
    setDeviceStream(deviceId, { ...current, ...patch })
      .catch((err) => setError(describe(err)))
      .finally(() => setBusy(false));
  }

  const resValue = current.resolution ? valueOf(current.resolution) : "";

  return (
    <>
      <h3 className="axi-eyebrow">Streaming for this machine</h3>
      <div className="axi-panel axi-stack">
        <div className="dusk-setting">
          <div className="dusk-field">
            <label htmlFor="dev-resolution">Resolution</label>
            <select
              id="dev-resolution"
              className="axi-select"
              value={resValue}
              disabled={busy}
              onChange={(e) => {
                const v = e.target.value;
                const choice = CHOICES.find((c) => c.value === v);
                change({ resolution: choice ? choice.resolution : null });
              }}
            >
              <option value="">Use default ({resolutionDefault(defaults.resolution)})</option>
              {resValue === "custom" && current.resolution?.kind === "fixed" && (
                <option value="custom">
                  {current.resolution.width} × {current.resolution.height}
                </option>
              )}
              {CHOICES.map((c) => (
                <option key={c.value} value={c.value}>
                  {c.label}
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="dusk-setting">
          <div className="dusk-field">
            <label htmlFor="dev-bitrate">Bitrate</label>
            <select
              id="dev-bitrate"
              className="axi-select"
              value={current.bitrateKbps === null ? "" : String(current.bitrateKbps)}
              disabled={busy}
              onChange={(e) =>
                change({ bitrateKbps: e.target.value === "" ? null : Number(e.target.value) })
              }
            >
              <option value="">Use default ({bitrateDefault(defaults.bitrateKbps)})</option>
              {current.bitrateKbps !== null && !BITRATES.includes(current.bitrateKbps / 1000) && (
                <option value={String(current.bitrateKbps)}>
                  {current.bitrateKbps / 1000} Mbps
                </option>
              )}
              {BITRATES.map((mbps) => (
                <option key={mbps} value={String(mbps * 1000)}>
                  {mbps} Mbps
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="dusk-setting">
          <div className="dusk-field">
            <label htmlFor="dev-fps">Frame rate</label>
            <select
              id="dev-fps"
              className="axi-select"
              value={current.fps === null ? "" : String(current.fps)}
              disabled={busy}
              onChange={(e) => change({ fps: e.target.value === "" ? null : Number(e.target.value) })}
            >
              <option value="">Use default ({fpsDefault(defaults.fps)})</option>
              {current.fps !== null && !FRAME_RATES.includes(current.fps) && (
                <option value={String(current.fps)}>{current.fps} fps</option>
              )}
              {FRAME_RATES.map((rate) => (
                <option key={rate} value={String(rate)}>
                  {rate} fps
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="dusk-setting">
          <div className="dusk-field">
            <label htmlFor="dev-codec">Video codec</label>
            <select
              id="dev-codec"
              className="axi-select"
              value={current.codec ?? ""}
              disabled={busy}
              onChange={(e) =>
                change({ codec: e.target.value === "" ? null : (e.target.value as Codec) })
              }
            >
              <option value="">Use default ({codecDefault(defaults.codec)})</option>
              {CODECS.map((c) => (
                <option key={c.value} value={c.value}>
                  {c.label}
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="dusk-setting">
          <div className="dusk-field">
            <label htmlFor="dev-free-cursor">Pointer leaves the stream</label>
            <select
              id="dev-free-cursor"
              className="axi-select"
              value={current.freeCursor === null ? "" : current.freeCursor ? "yes" : "no"}
              disabled={busy}
              onChange={(e) =>
                change({ freeCursor: e.target.value === "" ? null : e.target.value === "yes" })
              }
            >
              <option value="">Use default (only for the desktop)</option>
              <option value="yes">Always</option>
              <option value="no">Never</option>
            </select>
          </div>
          <p className="axi-ink-dim">{cursorOverrideDetail(defaults.freeCursor)}</p>
        </div>

        {error && <p className="axi-ink-dim">{error}</p>}
      </div>
    </>
  );
}
