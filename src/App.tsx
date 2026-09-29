import { useCallback, useEffect, useState } from "react";
import { addManualDevice, getSnapshot, onSnapshot, refreshNow } from "./api";
import type { Device, Snapshot } from "./types";
import { cardState } from "./deviceState";
import { DeviceCard } from "./components/DeviceCard";
import { SelfCard } from "./components/SelfCard";
import { AddDeviceDialog } from "./components/AddDeviceDialog";

/** Lit machines first, then alphabetical, so the actionable ones stay on top. */
const TONE_ORDER = { hosting: 0, ready: 1, unpaired: 2, offline: 3 } as const;

function sortDevices(devices: Device[]): Device[] {
  return [...devices].sort((a, b) => {
    const byTone = TONE_ORDER[cardState(a).tone] - TONE_ORDER[cardState(b).tone];
    return byTone !== 0 ? byTone : a.name.localeCompare(b.name);
  });
}

function Masthead({ children }: { children?: React.ReactNode }) {
  return (
    <header className="axi-mast">
      <div className="axi-mast__in">
        <span className="axi-brand">
          <span className="axi-sigil" aria-hidden="true">
            D
          </span>
          <span className="axi-brand__name">
            Dusk
            <small>Sunshine and Moonlight</small>
          </span>
        </span>
        {children}
      </div>
    </header>
  );
}

export function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    getSnapshot().then((s) => !cancelled && setSnapshot(s));
    onSnapshot((s) => !cancelled && setSnapshot(s)).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const handleAdd = useCallback(async (address: string, name?: string) => {
    await addManualDevice(address, name);
  }, []);

  if (!snapshot) return <Masthead />;

  const self = snapshot.devices.find((d) => d.isSelf);
  const remote = sortDevices(snapshot.devices.filter((d) => !d.isSelf));

  return (
    <>
      <Masthead>
        <div className="axi-row dusk-row__end">
          {snapshot.discovering && (
            <>
              {/* Rule 11: an indicator of work animates a composited
                  property, so a stalled main thread cannot freeze it into a
                  lie about the app having crashed. */}
              <span className="axi-spinner" style={{ "--axi-spinner-size": "14px" } as React.CSSProperties} />
              <span className="axi-ink-dim">Looking for machines</span>
            </>
          )}
          <button type="button" className="axi-btn axi-btn--ghost" onClick={() => refreshNow()}>
            Refresh
          </button>
          <button type="button" className="axi-btn" onClick={() => setAdding(true)}>
            Add machine
          </button>
        </div>
      </Masthead>

      <main className="axi-page dusk-page">
        <section className="dusk-section">
          <h2 className="axi-eyebrow">This machine</h2>
          <SelfCard name={self?.name ?? "This machine"} host={snapshot.host} />
        </section>

        <section className="dusk-section">
          <h2 className="axi-eyebrow">Your machines</h2>
          {remote.length === 0 ? (
            <div className="axi-panel dusk-empty axi-stack">
              <strong>No other machines yet</strong>
              <p className="axi-ink-dim dusk-empty__body">
                Dusk watches the local network for machines running Sunshine.
                One reached over a VPN will not announce itself, so add it by
                address.
              </p>
              <button type="button" className="axi-btn" onClick={() => setAdding(true)}>
                Add machine
              </button>
            </div>
          ) : (
            <div className="axi-grid" style={{ "--axi-grid-min": "260px" } as React.CSSProperties}>
              {remote.map((device) => (
                <DeviceCard key={device.id} device={device} />
              ))}
            </div>
          )}
        </section>
      </main>

      {adding && (
        <AddDeviceDialog onAdd={handleAdd} onClose={() => setAdding(false)} />
      )}
    </>
  );
}
