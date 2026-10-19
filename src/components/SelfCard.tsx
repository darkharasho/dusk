import type { HostState, Snapshot } from "../types";
import type { SettingsSection } from "./SettingsPage";

interface Props {
  name: string;
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
 * The machine you are sitting at, as a summary. Everything that changes it
 * lives in Settings, so this only says where things stand and points there
 * when something needs doing.
 */
export function SelfCard({ name, snapshot, onOpenSettings }: Props) {
  const { status } = snapshot.host;
  // Null is "not read yet", which is not the same as needing setup.
  const managed = snapshot.hostSignedIn !== false;
  const needsSetup = status.kind === "notInstalled" || !managed;
  const running = status.kind === "installed" && status.running;

  return (
    <div className="axi-panel axi-row">
      <div>
        <div className="axi-row">
          <strong>{name}</strong>
          {running && <span className="axi-chip axi-chip--ok">Hosting</span>}
        </div>
        <p className="axi-ink-dim">{summary(snapshot.host, managed)}</p>
      </div>
      <button
        type="button"
        className={`axi-btn dusk-row__end${needsSetup ? " axi-btn--primary" : ""}`}
        onClick={() => onOpenSettings(needsSetup ? "setup" : "hosting")}
      >
        {needsSetup ? "Set up hosting" : "Hosting settings"}
      </button>
    </div>
  );
}
