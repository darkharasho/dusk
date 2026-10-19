import { useCallback, useEffect, useState } from "react";
import { addManualDevice, getSnapshot, onSnapshot, refreshNow } from "./api";
import type { Device, Snapshot } from "./types";
import { cardState } from "./deviceState";
import { DeviceCard } from "./components/DeviceCard";
import { SelfCard } from "./components/SelfCard";
import { AddDeviceDialog } from "./components/AddDeviceDialog";
import { DeviceDrawer } from "./components/DeviceDrawer";
import { PairRequestPrompt } from "./components/PairRequestPrompt";
import { SettingsPage, type SettingsSection } from "./components/SettingsPage";

type Tab = "machines" | "settings";

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
          {/* The mark, not the language's lettered sigil: a crescent with
              the sun in its curve — the hour between day and night. */}
          <svg
            className="dusk-mark"
            viewBox="0 0 64 64"
            aria-hidden="true"
            focusable="false"
          >
            <mask id="dusk-crescent">
              <circle cx="30" cy="32" r="23" fill="#fff" />
              <circle cx="40" cy="32" r="20" fill="#000" />
            </mask>
            <circle cx="30" cy="32" r="23" fill="#DCE3EE" mask="url(#dusk-crescent)" />
            <circle cx="40" cy="32" r="10.5" fill="var(--axi-accent)" />
          </svg>
          <span className="axi-brand__name">
            Dusk
            <small>All your machines</small>
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
  const [openId, setOpenId] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("machines");
  const [section, setSection] = useState<SettingsSection>("account");

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
  const open = openId ? snapshot.devices.find((d) => d.id === openId) : undefined;

  function openSettings(next: SettingsSection) {
    setSection(next);
    setTab("settings");
  }

  return (
    <>
      <Masthead>
        <nav className="axi-tabs" aria-label="Sections">
          <button
            type="button"
            aria-current={tab === "machines" ? "page" : undefined}
            onClick={() => setTab("machines")}
          >
            Machines
          </button>
          <button
            type="button"
            aria-current={tab === "settings" ? "page" : undefined}
            onClick={() => setTab("settings")}
          >
            Settings
          </button>
        </nav>
        {tab === "machines" && (
          <div className="axi-row">
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
        )}
      </Masthead>

      {tab === "settings" ? (
        <main className="axi-page dusk-page">
          <SettingsPage
            name={self?.name ?? "This machine"}
            snapshot={snapshot}
            section={section}
            onSection={setSection}
          />
        </main>
      ) : (
        <main className="axi-page dusk-page">
          <section className="dusk-section">
            <h2 className="axi-eyebrow">This machine</h2>
            <SelfCard
              name={self?.name ?? "This machine"}
              snapshot={snapshot}
              onOpenSettings={openSettings}
            />
          </section>

          <section className="dusk-section">
            <h2 className="axi-eyebrow">Your machines</h2>
            {remote.length === 0 ? (
              <div className="axi-panel dusk-empty axi-stack">
                <strong>No other machines yet</strong>
                <p className="axi-ink-dim dusk-empty__body">
                  Dusk watches the local network for machines that can host.
                  One reached over a VPN will not announce itself, so add it
                  by address.
                </p>
                <button type="button" className="axi-btn" onClick={() => setAdding(true)}>
                  Add machine
                </button>
              </div>
            ) : (
              <div className="axi-grid" style={{ "--axi-grid-min": "260px" } as React.CSSProperties}>
                {remote.map((device) => (
                  <DeviceCard
                    key={device.id}
                    device={device}
                    onOpen={() => setOpenId(device.id)}
                  />
                ))}
              </div>
            )}
          </section>
        </main>
      )}

      {adding && (
        <AddDeviceDialog onAdd={handleAdd} onClose={() => setAdding(false)} />
      )}

      {/* Resolved from the live snapshot rather than held in state, so an
          open drawer keeps updating as the machine's state changes under it. */}
      {open && (
        <DeviceDrawer
          device={open}
          moonlightAvailable={snapshot.moonlightAvailable}
          overlayAvailable={snapshot.overlayAvailable}
          onClose={() => setOpenId(null)}
        />
      )}

      <PairRequestPrompt />
    </>
  );
}
