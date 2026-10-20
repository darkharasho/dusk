import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import type { Snapshot } from "../types";
import { AccountSection } from "./AccountSection";
import { HostingPanel } from "./HostingPanel";
import { HostSettings } from "./HostSettings";
import { SetupChecklist } from "./SetupChecklist";
import { StartupSection } from "./StartupSection";
import { checkForUpdate, restartToUpdate, useUpdateState, type UpdateState } from "../updates";

export type SettingsSection = "account" | "startup" | "hosting" | "setup" | "host" | "about";

const SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: "account", label: "Account" },
  { id: "startup", label: "Startup" },
  { id: "hosting", label: "Hosting" },
  { id: "setup", label: "Setup" },
  { id: "host", label: "Host settings" },
  { id: "about", label: "About" },
];

interface Props {
  name: string;
  snapshot: Snapshot;
  section: SettingsSection;
  onSection(section: SettingsSection): void;
}

/** Every setting Dusk has, in one place, one section at a time. */
export function SettingsPage({ name, snapshot, section, onSection }: Props) {
  const status = snapshot.host.status;
  const hostReady = status.kind === "installed" && snapshot.hostSignedIn === true;

  return (
    <div className="dusk-settings-page">
      <nav className="dusk-settings-nav" aria-label="Settings">
        {SECTIONS.map((s) => (
          <button
            key={s.id}
            type="button"
            className="dusk-settings-nav__item"
            aria-current={section === s.id ? "page" : undefined}
            onClick={() => onSection(s.id)}
          >
            {s.label}
          </button>
        ))}
      </nav>

      <section className="axi-stack dusk-settings-body">
        <h2>{SECTIONS.find((s) => s.id === section)?.label}</h2>

        {section === "account" && <AccountSection />}

        {section === "startup" && <StartupSection />}

        {section === "hosting" && <HostingPanel name={name} snapshot={snapshot} />}

        {section === "setup" && <SetupChecklist />}

        {section === "host" &&
          (hostReady ? (
            <HostSettings />
          ) : (
            <div className="axi-panel axi-stack">
              <p>Set up hosting on this machine first.</p>
              <div className="axi-row">
                <button type="button" className="axi-btn" onClick={() => onSection("setup")}>
                  Go to setup
                </button>
              </div>
            </div>
          ))}

        {section === "about" && <About />}
      </section>
    </div>
  );
}

function About() {
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    getVersion().then(setVersion).catch(() => {});
  }, []);

  return (
    <div className="axi-panel axi-stack">
      <p>
        <strong>Dusk</strong>
        {version && <span className="axi-ink-dim"> · version {version}</span>}
      </p>
      <p className="axi-ink-dim">
        Every machine you own in one app. Stream from any of them, and let any
        of them stream from this one.
      </p>
      <Updates />
      {/* The notices the bundled open-source components require. Tucked away,
          not absent: their licences ask for them to be reachable. */}
      <details>
        <summary className="axi-ink-dim">Open-source licences</summary>
        <p className="axi-ink-faint">
          Dusk&rsquo;s hosting and streaming are built on Sunshine and on a
          fork of Moonlight, both free software under the GNU General Public
          License, version 3. They run as separate programs alongside Dusk.
          Their licence and an offer of their source code are in the
          <code className="axi-code"> licenses </code> folder inside Dusk&rsquo;s
          installation.
        </p>
      </details>
    </div>
  );
}

function updateLine(update: UpdateState): string {
  switch (update.kind) {
    case "unsupported":
      return "This copy of Dusk updates through your package manager.";
    case "idle":
      return "Dusk looks for a new version on launch and every few hours.";
    case "checking":
      return "Looking for a new version…";
    case "upToDate":
      return "This is the latest version.";
    case "downloading":
      return `Downloading version ${update.version}` + (update.percent !== null ? ` · ${update.percent}%` : "");
    case "ready":
      return `Version ${update.version} is ready. Restart Dusk to finish.`;
    case "installing":
      return `Installing version ${update.version}…`;
    case "error":
      return `Could not update: ${update.message}`;
  }
}

/** Where the self-update is, and the buttons that move it along. */
function Updates() {
  const update = useUpdateState();
  if (update.kind === "unsupported") {
    return <p className="axi-ink-faint">{updateLine(update)}</p>;
  }
  const busy = ["checking", "downloading", "installing"].includes(update.kind);
  return (
    <div className="axi-row">
      <span className={update.kind === "error" ? "axi-ink-danger" : "axi-ink-dim"}>{updateLine(update)}</span>
      {update.kind === "ready" ? (
        <button type="button" className="axi-btn axi-btn--primary" onClick={() => void restartToUpdate()}>
          Restart to update
        </button>
      ) : (
        <button
          type="button"
          className="axi-btn axi-btn--ghost"
          disabled={busy}
          onClick={() => void checkForUpdate(true)}
        >
          Check for updates
        </button>
      )}
    </div>
  );
}
