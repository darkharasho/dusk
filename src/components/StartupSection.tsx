import { useEffect, useState } from "react";
import { getStartup, setOpenAtLogin, setStartupPrefs } from "../api";
import type { StartupPrefs, StartupView } from "../types";
import { describe } from "../errors";

/** Where a hidden Dusk waits, in the words this platform uses for it. */
const TRAY = /Mac/.test(navigator.userAgent) ? "menu bar" : "tray";

/**
 * Whether Dusk opens when you sign in, and what closing its window does.
 *
 * Dusk is worth leaving running: it answers your other machines when they
 * ask to pair, and its tray icon turns hosting on and off.
 */
export function StartupSection() {
  const [view, setView] = useState<StartupView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getStartup()
      .then(setView)
      .catch((e) => setError(describe(e)));
  }, []);

  async function run(change: () => Promise<StartupView>) {
    setBusy(true);
    setError(null);
    try {
      setView(await change());
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  function prefs(v: StartupView, patch: Partial<StartupPrefs>): StartupPrefs {
    return {
      startInBackground: v.startInBackground,
      closeToTray: v.closeToTray,
      ...patch,
    };
  }

  return (
    <div className="axi-panel axi-stack">
      {!view && !error && <p className="axi-ink-dim">Checking.</p>}

      {view && (
        <>
          <Toggle
            id="startup-login"
            label="Open Dusk when you sign in"
            detail={
              view.openAtLoginBlocked && !view.openAtLogin
                ? view.openAtLoginBlocked
                : "So your other machines can reach this one without you opening Dusk first."
            }
            on={view.openAtLogin}
            disabled={busy || (!view.openAtLogin && view.openAtLoginBlocked !== null)}
            onChange={(on) => run(() => setOpenAtLogin(on))}
          />
          <Toggle
            id="startup-background"
            label={`Start in the ${TRAY}`}
            detail={`When Dusk opens at sign-in, it waits in the ${TRAY} instead of opening its window.`}
            on={view.startInBackground}
            disabled={busy || !view.openAtLogin}
            onChange={(on) => run(() => setStartupPrefs(prefs(view, { startInBackground: on })))}
          />
          <Toggle
            id="startup-close"
            label={`Keep running in the ${TRAY} when the window is closed`}
            detail={`Quit from the ${TRAY} icon instead. Off, closing the window quits Dusk.`}
            on={view.closeToTray}
            disabled={busy}
            onChange={(on) => run(() => setStartupPrefs(prefs(view, { closeToTray: on })))}
          />
        </>
      )}

      {error && (
        <div className="axi-notice axi-notice--danger">
          <span className="axi-notice__icon" aria-hidden="true">
            !
          </span>
          <p>{error}</p>
        </div>
      )}
    </div>
  );
}

export function Toggle({
  id,
  label,
  detail,
  on,
  disabled,
  onChange,
}: {
  id: string;
  label: string;
  detail: string;
  on: boolean;
  disabled: boolean;
  onChange(on: boolean): void;
}) {
  return (
    <div className="dusk-toggle">
      <div>
        <label htmlFor={id}>{label}</label>
        <p className="axi-ink-dim">{detail}</p>
      </div>
      <button
        type="button"
        id={id}
        className="axi-switch"
        role="switch"
        aria-checked={on}
        disabled={disabled}
        onClick={() => onChange(!on)}
      >
        <span className="axi-switch__knob" />
      </button>
    </div>
  );
}
