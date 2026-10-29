import { useEffect, useState } from "react";
import {
  allowInput,
  getSetup,
  installDisplayDriver,
  installSunshine,
  onDownloadProgress,
  openFirewall,
  openPrivacySettings,
  previewSunshineDownload,
  provisionHost,
  startHosting,
  type DownloadProgress,
} from "../api";
import type { DownloadPreview, Setup, Step } from "../types";
import { describe } from "../errors";

/** Bytes as something a person reads, not a number with nine digits. */
function megabytes(bytes: number): string {
  return `${Math.round(bytes / 1_000_000)} MB`;
}

function stateChip(step: Step): { className: string; label: string } {
  switch (step.state.kind) {
    case "done":
      return { className: "axi-chip axi-chip--ok", label: "Done" };
    case "todo":
      return step.optional
        ? { className: "axi-chip axi-chip--meta", label: "Optional" }
        : { className: "axi-chip axi-chip--warn", label: "To do" };
    case "notNeeded":
      return { className: "axi-chip", label: "Not needed" };
    case "blocked":
      // Not "To do": nothing anyone presses here will help until the reason
      // is dealt with, and a button that cannot work is worse than none.
      return { className: "axi-chip axi-chip--warn", label: "Blocked" };
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
 * hosting is often already installed and running, and the only outstanding
 * item might be a firewall rule. Every step shows its own state and can be
 * run on its own.
 */
export function SetupChecklist() {
  const [setup, setSetup] = useState<Setup | null>(null);
  const [preview, setPreview] = useState<DownloadPreview | null>(null);
  const [confirmDriver, setConfirmDriver] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);

  useEffect(() => {
    getSetup().then(setSetup).catch((e) => setError(describe(e)));
  }, []);

  useEffect(() => {
    let stop: (() => void) | undefined;
    let cancelled = false;
    onDownloadProgress(setProgress).then((fn) => {
      if (cancelled) fn();
      else stop = fn;
    });
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);

  async function runInstall() {
    setBusy(true);
    setError(null);
    setProgress(null);
    try {
      await installSunshine();
      setPreview(null);
      await refresh();
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  }

  async function refresh() {
    setSetup(await getSetup());
  }

  /** Carry out one step Dusk can do itself. */
  async function perform(step: Step) {
    switch (step.id) {
      case "installSunshine":
        await installSunshine();
        setPreview(null);
        break;
      case "startService":
        await startHosting();
        break;
      case "provision":
        await provisionHost();
        break;
      case "firewall":
        await openFirewall();
        break;
      case "input":
        await allowInput();
        break;
      case "virtualDisplay":
        await installDisplayDriver();
        break;
      default:
        throw new Error("Dusk cannot do this step for you yet.");
    }
  }

  async function act(step: Step) {
    setBusy(true);
    setError(null);
    try {
      switch (step.id) {
        case "installSunshine": {
          // Deliberately a two-step flow: see what would be installed and
          // whether it can be verified, then decide. This ends up running
          // with elevated rights.
          setPreview(await previewSunshineDownload());
          break;
        }
        case "virtualDisplay":
          // A driver gets its own yes, with what it is spelled out first.
          setConfirmDriver(true);
          break;
        case "screenRecording":
        case "accessibility":
          await openPrivacySettings(step.id);
          break;
        default:
          await perform(step);
          await refresh();
      }
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Every step Dusk can do, in order, from one press. The list of what it
   * will do is on screen beside the button, so the press is the consent.
   * Re-reads the checklist after each step, because one step often finishes
   * another: installing also sets up the sign-in, which also starts hosting.
   */
  async function setUpEverything() {
    setBusy(true);
    setError(null);
    const tried = new Set<string>();
    try {
      for (;;) {
        const current = await getSetup();
        setSetup(current);
        const next = current.steps.find(
          (s) =>
            s.automatable && !s.optional && s.state.kind === "todo" && !tried.has(s.id),
        );
        if (!next) break;
        tried.add(next.id);
        await perform(next);
      }
    } catch (err) {
      setError(describe(err));
      await refresh().catch(() => {});
    } finally {
      setBusy(false);
      setProgress(null);
    }
  }

  const automatic =
    setup?.steps.filter((s) => s.automatable && !s.optional && s.state.kind === "todo") ?? [];

  async function runDriverInstall() {
    setBusy(true);
    setError(null);
    try {
      await installDisplayDriver();
      setConfirmDriver(false);
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
      await refresh().catch(() => {});
    }
  }

  return (
    <div className="axi-stack">
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
              Hosting {preview.version}
              {!preview.embedded && <> · {megabytes(preview.size)}</>}
            </p>
            {preview.embedded ? (
              <p>
                Hosting is built into Dusk, so nothing is downloaded or
                copied. Turning it on makes it start in the background
                when you sign in.
              </p>
            ) : preview.bundled ? (
              <p>
                This comes with Dusk, so nothing is downloaded.
                Dusk checks it against the checksum recorded when this
                release was built before installing it.
              </p>
            ) : preview.verifiable ? (
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
            {progress && progress.total > 0 ? (
              <>
                {/* Rule 9: a proportion is drawn as length at full
                    strength, never as a faded fill. */}
                <div className="axi-meter">
                  <span
                    className="axi-meter__fill"
                    style={
                      {
                        "--axi-meter-v": `${Math.round(
                          (progress.received / progress.total) * 100,
                        )}%`,
                      } as React.CSSProperties
                    }
                  />
                </div>
                <p className="axi-ink-faint">
                  {megabytes(progress.received)} of{" "}
                  {megabytes(progress.total)}
                </p>
              </>
            ) : (
              preview.verifiable && (
                <div className="axi-row">
                  <button
                    type="button"
                    className="axi-btn axi-btn--primary"
                    disabled={busy}
                    onClick={runInstall}
                  >
                    {busy
                      ? preview.embedded
                        ? "Turning on"
                        : "Installing"
                      : preview.embedded
                        ? "Turn on hosting"
                        : preview.bundled
                          ? "Install"
                          : "Download and install"}
                  </button>
                  <button
                    type="button"
                    className="axi-btn axi-btn--ghost"
                    disabled={busy}
                    onClick={() => setPreview(null)}
                  >
                    Not now
                  </button>
                </div>
              )
            )}
          </div>
        </div>
      )}

      {confirmDriver && (
        <div className="axi-notice">
          <span className="axi-notice__icon" aria-hidden="true">
            ↓
          </span>
          <div className="axi-stack">
            <p>
              This installs the Virtual Display Driver, an open-source display
              driver from another project. Windows will treat it as a monitor
              that is always connected, which hosting can stream from.
            </p>
            <p className="axi-ink-dim">
              Dusk downloads the version it was built to expect and refuses it
              if it does not match. Windows will ask for administrator rights.
              You can remove it later in Device Manager under Display
              adapters.
            </p>
            <div className="axi-row">
              <button
                type="button"
                className="axi-btn axi-btn--primary"
                disabled={busy}
                onClick={runDriverInstall}
              >
                {busy ? "Installing" : "Install the driver"}
              </button>
              <button
                type="button"
                className="axi-btn axi-btn--ghost"
                disabled={busy}
                onClick={() => setConfirmDriver(false)}
              >
                Not now
              </button>
            </div>
          </div>
        </div>
      )}

      {!setup && !error && <p className="axi-ink-dim">Checking this machine.</p>}

      {automatic.length > 0 && !preview && (
        <div className="axi-well axi-stack">
          <p>
            Dusk can do {automatic.length === 1 ? "this" : "these"} for
            you: {automatic.map((s) => s.title.toLowerCase()).join(", ")}.
            Your system may ask for your password.
          </p>
          <div className="axi-row">
            <button
              type="button"
              className="axi-btn axi-btn--primary"
              disabled={busy}
              onClick={setUpEverything}
            >
              {busy ? "Setting up" : "Set up this machine"}
            </button>
          </div>
        </div>
      )}

      <div className="axi-stack">
        {setup?.steps.map((step) => {
          const chip = stateChip(step);
          const done = step.state.kind === "done";
          const skip = step.state.kind === "notNeeded";
          const blocked = step.state.kind === "blocked";
          return (
            <div className="axi-panel axi-panel--tile axi-stack" key={step.id}>
              <div className="axi-row">
                <strong>{step.title}</strong>
                <span className={`${chip.className} dusk-row__end`}>
                  {chip.label}
                </span>
              </div>
              <p className="axi-ink-dim">
                {step.state.kind === "notNeeded" ||
                step.state.kind === "blocked"
                  ? step.state.reason
                  : step.detail}
              </p>
              {!done && !skip && !blocked && (
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
  );
}
