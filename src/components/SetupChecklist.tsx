import { useEffect, useState } from "react";
import {
  getSetup,
  openPrivacySettings,
  previewSunshineDownload,
  startHosting,
} from "../api";
import type { DownloadPreview, Setup, Step } from "../types";

interface Props {
  onClose(): void;
  onSignIn(): void;
}

/** Bytes as something a person reads, not a number with nine digits. */
function megabytes(bytes: number): string {
  return `${Math.round(bytes / 1_000_000)} MB`;
}

function stateChip(step: Step): { className: string; label: string } {
  switch (step.state.kind) {
    case "done":
      return { className: "axi-chip axi-chip--ok", label: "Done" };
    case "todo":
      return { className: "axi-chip axi-chip--warn", label: "To do" };
    case "notNeeded":
      return { className: "axi-chip", label: "Not needed" };
    case "unknown":
      // Genuinely commentary rather than a verdict: Dusk cannot see this
      // one, which is a different thing from it being undone.
      return { className: "axi-chip axi-chip--meta", label: "Check yourself" };
  }
}

/**
 * First-run setup, as a checklist rather than a wizard.
 *
 * A wizard assumes nothing is done, which is wrong for most people here —
 * Sunshine is often already installed and running, and the only outstanding
 * item might be a firewall rule. Every step shows its own state and can be
 * run on its own.
 */
export function SetupChecklist({ onClose, onSignIn }: Props) {
  const [setup, setSetup] = useState<Setup | null>(null);
  const [preview, setPreview] = useState<DownloadPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    getSetup().then(setSetup).catch((e) => setError(String(e)));
  }, []);

  async function refresh() {
    setSetup(await getSetup());
  }

  async function act(step: Step) {
    setBusy(true);
    setError(null);
    try {
      switch (step.id) {
        case "installSunshine": {
          // Deliberately a two-step flow: see what would be downloaded and
          // whether it can be verified, then decide. This ends up running
          // with elevated rights.
          setPreview(await previewSunshineDownload());
          break;
        }
        case "startService":
          await startHosting();
          await refresh();
          break;
        case "signIn":
          onSignIn();
          onClose();
          break;
        case "screenRecording":
        case "accessibility":
          await openPrivacySettings(step.id);
          break;
        default:
          setError("Dusk cannot do this step for you yet.");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="axi-scrim" onClick={onClose} />
      <aside className="axi-drawer" role="dialog" aria-label="Set up hosting">
        <div className="axi-drawer__head">
          <h2>Set up hosting</h2>
          <button
            type="button"
            className="axi-drawer__close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        <div className="axi-drawer__body axi-stack dusk-settings">
          {error && (
            <div className="axi-notice axi-notice--danger">
              <span className="axi-notice__icon" aria-hidden="true">
                !
              </span>
              <p>{error}</p>
            </div>
          )}

          {preview && (
            <div className="axi-notice">
              <span className="axi-notice__icon" aria-hidden="true">
                ↓
              </span>
              <div className="axi-stack">
                <p>
                  Sunshine {preview.version} · {preview.asset} ·{" "}
                  {megabytes(preview.size)}
                </p>
                {preview.verifiable ? (
                  <p>
                    {/* Say exactly what the check is worth. The digest comes
                        from the same service as the file, so it proves the
                        bytes arrived intact — not that the release is
                        genuine. */}
                    Dusk will check the download against the checksum GitHub
                    publishes for it. That confirms the file arrived intact,
                    not who built it.
                  </p>
                ) : (
                  <p className="axi-ink-danger">
                    This release has no checksum, so Dusk will not install it.
                  </p>
                )}
                <p className="axi-ink-faint">
                  Installing is not wired up yet — this is the download Dusk
                  would fetch.
                </p>
              </div>
            </div>
          )}

          {!setup && !error && <p className="axi-ink-dim">Checking this machine.</p>}

          <div className="axi-stack">
            {setup?.steps.map((step) => {
              const chip = stateChip(step);
              const done = step.state.kind === "done";
              const skip = step.state.kind === "notNeeded";
              return (
                <div className="axi-panel axi-panel--tile axi-stack" key={step.id}>
                  <div className="axi-row">
                    <strong>{step.title}</strong>
                    <span className={`${chip.className} dusk-row__end`}>
                      {chip.label}
                    </span>
                  </div>
                  <p className="axi-ink-dim">
                    {step.state.kind === "notNeeded"
                      ? step.state.reason
                      : step.detail}
                  </p>
                  {!done && !skip && (
                    <div className="axi-row">
                      <button
                        type="button"
                        className="axi-btn axi-btn--sm"
                        disabled={busy}
                        onClick={() => act(step)}
                      >
                        {step.automatable ? "Do this" : "Show me"}
                      </button>
                    </div>
                  )}
                </div>
              );
            })}
          </div>

          <div className="axi-row">
            <button type="button" className="axi-btn axi-btn--ghost" onClick={refresh}>
              Check again
            </button>
          </div>
        </div>
      </aside>
    </>
  );
}
