import { useCallback, useEffect, useState } from "react";
import {
  addManualDevice,
  getSnapshot,
  onSessionFailed,
  onSnapshot,
  refreshNow,
  type SessionFailed,
} from "./api";
import type { Device, Snapshot } from "./types";
import { cardState } from "./deviceState";
import { DeviceCard } from "./components/DeviceCard";
import { SelfCard } from "./components/SelfCard";
import { AddDeviceDialog } from "./components/AddDeviceDialog";
import { DeviceDrawer } from "./components/DeviceDrawer";
import { PairRequestPrompt } from "./components/PairRequestPrompt";
import { SettingsPage, type SettingsSection } from "./components/SettingsPage";
import { Titlebar } from "./components/Titlebar";
import { Icon } from "./components/Icon";
import { startUpdates } from "./updates";
import { describe } from "./errors";

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

/**
 * The frame: Dusk's own titlebar, and under it the one region that scrolls,
 * so the titlebar never moves and the masthead sticks below it.
 */
export function App() {
  useEffect(() => startUpdates(), []);
  return (
    <>
      <Titlebar />
      <div className="dusk-body">
        <Body />
      </div>
    </>
  );
}

function Body() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [adding, setAdding] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("machines");
  const [section, setSection] = useState<SettingsSection>("account");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  // A stream that died after its launch call returned. Kept here rather
  // than only in the drawer: the drawer is usually closed by then, and
  // the window coming back with nothing to say was the whole complaint.
  const [failure, setFailure] = useState<SessionFailed | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    const load = () =>
      getSnapshot()
        .then((s) => {
          if (cancelled) return;
          setSnapshot(s);
          setLoadError(null);
        })
        .catch((err) => !cancelled && setLoadError(describe(err)));

    load();
    onSnapshot((s) => !cancelled && setSnapshot(s)).then((fn) => {
      if (cancelled) {
        fn();
        return;
      }
      unlisten = fn;
      // A snapshot emitted between the first read and the listener
      // attaching would be missed; read once more now that it is on.
      load();
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [attempt]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onSessionFailed((f) => !cancelled && setFailure(f)).then((fn) => {
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

  if (!snapshot) {
    return (
      <>
        <Masthead />
        <main className="axi-page dusk-page">
          {loadError ? (
            <div className="axi-notice axi-notice--danger">
              <span className="axi-notice__icon" aria-hidden="true">
                !
              </span>
              <div className="axi-stack">
                <p>Dusk could not read its own state: {loadError}</p>
                <div className="axi-row">
                  <button
                    type="button"
                    className="axi-btn axi-btn--sm"
                    onClick={() => setAttempt((n) => n + 1)}
                  >
                    Try again
                  </button>
                </div>
              </div>
            </div>
          ) : (
            <p className="axi-ink-dim">Starting.</p>
          )}
        </main>
      </>
    );
  }

  const self = snapshot.devices.find((d) => d.isSelf);
  const remote = sortDevices(snapshot.devices.filter((d) => !d.isSelf));
  // Your machines are the ones on your account, not whatever is advertising
  // on this network. Falling back to one list when none are identified is
  // deliberate: signed out, or before any machine has answered, Dusk cannot
  // tell whose is whose, and a "Your machines" heading over an empty list
  // with your machines sitting below it would be a lie.
  const mine = remote.filter((d) => d.mine);
  const others = remote.filter((d) => !d.mine);
  const grouped = mine.length > 0;
  const listed = grouped ? mine : remote;
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
      </Masthead>

      {failure && (
        <div className="axi-page dusk-page dusk-banner">
          <div className="axi-notice axi-notice--danger">
            <span className="axi-notice__icon" aria-hidden="true">
              !
            </span>
            <p>
              {snapshot.devices.find((d) => d.id === failure.deviceId)?.name ??
                "A stream"}
              : {failure.message}
            </p>
            <button
              type="button"
              className="axi-btn axi-btn--ghost axi-btn--sm dusk-row__end"
              onClick={() => setFailure(null)}
              aria-label="Dismiss"
            >
              ×
            </button>
          </div>
        </div>
      )}

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
            <div className="dusk-section__head">
              <h2 className="axi-eyebrow">This machine</h2>
            </div>
            <SelfCard
              name={self?.name ?? "This machine"}
              address={self?.primaryAddress ?? self?.addresses[0] ?? null}
              snapshot={snapshot}
              onOpenSettings={openSettings}
            />
          </section>

          <section className="dusk-section">
            {/* The list's own actions sit with the list, not in the masthead:
                there they came and went with the tab, and the tabs beside
                them jumped every time. */}
            <div className="dusk-section__head">
              <h2 className="axi-eyebrow">
                Your machines
                {listed.length > 0 && <span className="dusk-count">{listed.length}</span>}
              </h2>
              {snapshot.discovering && (
                <span className="axi-row dusk-looking">
                  {/* Rule 11: an indicator of work animates a composited
                      property, so a stalled main thread cannot freeze it
                      into a lie about the app having crashed. */}
                  <span className="axi-spinner" />
                  Looking for machines
                </span>
              )}
              <div className="axi-row dusk-row__end">
                <button
                  type="button"
                  className="axi-btn axi-btn--ghost axi-btn--sm"
                  onClick={() => refreshNow().catch((err) => console.error(err))}
                >
                  <Icon name="refresh" />
                  Refresh
                </button>
                <button
                  type="button"
                  className="axi-btn axi-btn--sm"
                  onClick={() => setAdding(true)}
                >
                  <Icon name="plus" />
                  Add machine
                </button>
              </div>
            </div>
            {listed.length === 0 ? (
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
              <div className="axi-grid dusk-grid">
                {listed.map((device) => (
                  <DeviceCard
                    key={device.id}
                    device={device}
                    onOpen={() => setOpenId(device.id)}
                  />
                ))}
              </div>
            )}
          </section>

          {/* Everything else that can host here. Shown second and plainly,
              because it is the answer to "why is that not in my grid" --
              hiding it would make a housemate's PC look like a bug in
              discovery. Pairing with one still asks for a PIN. */}
          {grouped && others.length > 0 && (
            <section className="dusk-section">
              <div className="dusk-section__head">
                <h2 className="axi-eyebrow">
                  Other machines on this network
                  <span className="dusk-count">{others.length}</span>
                </h2>
              </div>
              <div className="axi-grid dusk-grid">
                {others.map((device) => (
                  <DeviceCard
                    key={device.id}
                    device={device}
                    onOpen={() => setOpenId(device.id)}
                  />
                ))}
              </div>
            </section>
          )}
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
