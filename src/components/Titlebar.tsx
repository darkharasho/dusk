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
 * `data-tauri-drag-region` only applies to the element that carries it, not
 * to its children, so the label repeats it. Buttons never drag.
 */
export function Titlebar() {
  const win = getCurrentWindow();
  const maximized = useMaximized();

  return (
    <div
      className="axi-titlebar dusk-titlebar"
      data-tauri-drag-region
      data-mac={isMac || undefined}
      onDoubleClick={(e) => {
        // macOS answers the double-click itself.
        if (!isMac && e.target === e.currentTarget) void win.toggleMaximize();
      }}
    >
      <span data-tauri-drag-region>Dusk</span>
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
        <span className="axi-spinner" style={{ "--axi-spinner-size": "11px" } as React.CSSProperties} />
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

function useMaximized(): boolean {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    if (isMac) return;
    const win = getCurrentWindow();
    let live = true;
    const read = () => win.isMaximized().then((m) => live && setMaximized(m), () => {});
    read();
    // There is no maximize event; a resize is what a maximize looks like.
    const off = win.onResized(read);
    return () => {
      live = false;
      void off.then((fn) => fn());
    };
  }, []);
  return maximized;
}
