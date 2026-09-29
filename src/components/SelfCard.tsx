import type { HostState } from "../types";

interface Props {
  name: string;
  host: HostState;
}

function statusLine(host: HostState): string {
  switch (host.status.kind) {
    case "notInstalled":
      return "Sunshine is not installed yet";
    case "installed":
      return host.status.running
        ? `Hosting is on${host.status.version ? ` · Sunshine ${host.status.version}` : ""}`
        : "Hosting is off";
    case "unknown":
      return host.status.reason;
  }
}

/**
 * The machine you are sitting at. It is the only one that can host, so it is
 * a panel rather than one tile among equals in the grid.
 */
export function SelfCard({ name, host }: Props) {
  const { status, capabilities } = host;
  const running = status.kind === "installed" && status.running;

  return (
    <div className="axi-panel">
      <div className="axi-row">
        <div>
          <div className="axi-row">
            <strong>{name}</strong>
            {capabilities.supportTier === "experimental" && (
              <span
                className="axi-chip axi-chip--meta"
                title={capabilities.caveats.join(" ")}
              >
                Experimental host
              </span>
            )}
          </div>
          <p className="axi-ink-dim">{statusLine(host)}</p>
        </div>

        <button
          type="button"
          className="axi-btn dusk-row__end"
          disabled={status.kind !== "installed"}
        >
          {running ? "Turn off hosting" : "Turn on hosting"}
        </button>
      </div>
    </div>
  );
}
