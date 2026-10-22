import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { restartToUpdate, useUpdateState } from "../updates";

/**
 * macOS keeps its own traffic lights, drawn over this strip
 * (tauri.macos.conf.json places them on its centre line); everywhere else
 * the window has no frame and these are its only buttons.
 */
const isMac = navigator.userAgent.includes("Mac");

/**
 * The window's chrome: a drag strip, the update pill, and the window buttons.
 *
 * Tauri's drag-region script does the moving and the double-click: with
 * `deep`, anything in the strip that is not a control starts a drag, and
 * two clicks maximize (on mouseup on macOS, where a drag in between cancels
 * it, matching the system). Nothing here answers the double-click as well:
 * a second toggle on top of Tauri's maximized the window and put it back.
 */
export function Titlebar() {
  const win = getCurrentWindow();
  const { maximized, fullscreen, focused } = useWindowState();

  return (
    <div
      className="axi-titlebar dusk-titlebar"
      data-tauri-drag-region="deep"
      data-mac={isMac || undefined}
      data-fullscreen={fullscreen || undefined}
      data-blurred={!focused || undefined}
    >
      <span className="dusk-titlebar__name">Dusk</span>
      <UpdatePill />
      {!isMac && (
        <div className="axi-titlebar__btns">
          <button type="button" aria-label="Minimize" title="Minimize" onClick={() => void win.minimize()}>
            <svg viewBox="0 0 10 10" aria-hidden="true" className="dusk-titlebar__glyph">
              <path d="M0 5.5h10" stroke="currentColor" />
            </svg>
          </button>
          <button
            type="button"
            aria-label={maximized ? "Restore" : "Maximize"}
            title={maximized ? "Restore" : "Maximize"}
            onClick={() => void win.toggleMaximize()}
          >
            <svg viewBox="0 0 10 10" aria-hidden="true" className="dusk-titlebar__glyph">
              {maximized ? (
                <path d="M2.5 2.5V.5h7v7h-2M.5 2.5h7v7h-7z" fill="none" stroke="currentColor" />
              ) : (
                <path d="M.5.5h9v9h-9z" fill="none" stroke="currentColor" />
              )}
            </svg>
          </button>
          {/* Closing follows the Startup setting: to the tray, or quit. */}
          <button type="button" aria-label="Close" title="Close" onClick={() => void win.close()}>
            <svg viewBox="0 0 10 10" aria-hidden="true" className="dusk-titlebar__glyph">
              <path d="m.5.5 9 9m0-9-9 9" stroke="currentColor" />
            </svg>
          </button>
        </div>
      )}
    </div>
  );
}

/** Download progress, then the button that restarts into the new version. */
function UpdatePill() {
  const update = useUpdateState();

  if (update.kind === "downloading") {
    return (
      <span className="axi-row dusk-titlebar__update" aria-live="polite" title={`Downloading Dusk ${update.version}`}>
        <span className="axi-spinner" />
        Updating{update.percent !== null && ` ${update.percent}%`}
      </span>
    );
  }
  if (update.kind === "ready" || update.kind === "installing") {
    return (
      <button
        type="button"
        className="axi-btn axi-btn--primary axi-btn--sm dusk-titlebar__update"
        disabled={update.kind === "installing"}
        title={`Restart Dusk to install version ${update.version}`}
        onClick={() => void restartToUpdate()}
      >
        {update.kind === "installing" ? "Installing" : "Restart to update"}
      </button>
    );
  }
  return null;
}

interface WindowState {
  maximized: boolean;
  /** macOS native full screen, where the traffic lights leave the strip. */
  fullscreen: boolean;
  focused: boolean;
}

/**
 * What the strip has to know about the window it tops. A native titlebar
 * dims when its window is not the active one, on all three platforms, and
 * the restore glyph only means something once the window is maximized.
 */
function useWindowState(): WindowState {
  const [state, setState] = useState<WindowState>({
    maximized: false,
    fullscreen: false,
    focused: true,
  });

  useEffect(() => {
    const win = getCurrentWindow();
    let live = true;
    const read = () =>
      Promise.all([win.isMaximized(), win.isFullscreen()]).then(
        ([maximized, fullscreen]) => live && setState((s) => ({ ...s, maximized, fullscreen })),
        () => {},
      );
    read();
    // There is no maximize event; a resize is what a maximize looks like.
    const stops = Promise.all([
      win.onResized(read),
      win.onFocusChanged(({ payload: focused }) => live && setState((s) => ({ ...s, focused }))),
    ]);
    return () => {
      live = false;
      void stops.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  return state;
}
