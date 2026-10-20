import type { CSSProperties } from "react";
import type { HostState, Snapshot } from "../types";
import type { SettingsSection } from "./SettingsPage";
import { initials } from "../deviceState";

interface Props {
  name: string;
  address: string | null;
  snapshot: Snapshot;
  onOpenSettings(section: SettingsSection): void;
}

function summary(host: HostState, managed: boolean): string {
  switch (host.status.kind) {
    case "notInstalled":
      return "Not set up to host yet";
    case "installed":
      if (!managed) return "Hosting needs finishing";
      return host.status.running ? "Hosting is on" : "Hosting is off";
    case "unknown":
      return host.status.reason;
  }
}

/**
 * The machine you are sitting at, drawn as the same kind of card as the
 * others so it reads as one of your machines rather than a settings banner.
 * Everything that changes it lives in Settings, so the card is the way there.
 */
export function SelfCard({ name, address, snapshot, onOpenSettings }: Props) {
  const { status } = snapshot.host;
  // Null is "not read yet", which is not the same as needing setup.
  const managed = snapshot.hostSignedIn !== false;
  const needsSetup = status.kind === "notInstalled" || !managed;
  const running = status.kind === "installed" && status.running;

  // The same inks the grid uses: on is ready to be streamed from, needing
  // setup is the warning, off is the absence of a status.
  const strip = needsSetup ? "var(--axi-warn)" : running ? "var(--axi-ok)" : null;

  return (
    <button
      type="button"
      className={`axi-card dusk-self${strip ? " axi-card--strip" : ""}`}
      style={strip ? ({ "--axi-card-strip": strip } as CSSProperties) : undefined}
      onClick={() => onOpenSettings(needsSetup ? "setup" : "hosting")}
    >
      <div className="axi-card__head">
        <span className="axi-card__glyph" aria-hidden="true">
          {initials(name)}
        </span>
        <span className="axi-card__title">
          <span className="axi-card__name">{name}</span>
          <p>
            {summary(snapshot.host, managed)}
            {address && (
              <>
                {" · "}
                <span className="dusk-card__address">{address}</span>
              </>
            )}
          </p>
        </span>
      </div>

      <div className="axi-card__meta">
        {running && !needsSetup && <span className="axi-chip axi-chip--ok">Hosting</span>}
        {needsSetup && <span className="axi-chip axi-chip--warn">Needs setup</span>}
        {!running && !needsSetup && <span className="axi-chip">Off</span>}
        <span className="axi-card__go">{needsSetup ? "Set up hosting" : "Hosting settings"}</span>
      </div>
    </button>
  );
}
