import { useEffect, useMemo, useState } from "react";
import { getHostConfig, saveHostConfig, type HostConfig } from "../api";
import {
  GROUPS,
  METADATA_KEYS,
  settingFor,
  type Control,
  type Setting,
} from "../sunshineSchema";

interface Props {
  onClose(): void;
}

/** Sunshine reports almost everything as a string, so that is what we send. */
function toWire(control: Control, raw: string | boolean): string {
  if (control.kind === "toggle") return raw ? "enabled" : "disabled";
  return String(raw);
}

function isOn(value: unknown): boolean {
  const v = String(value ?? "").trim().toLowerCase();
  return v === "true" || v === "enabled" || v === "1";
}

/**
 * The host's settings, replacing Sunshine's web UI.
 *
 * Curated settings get a considered control; everything else is rendered
 * from its own value into "Everything else". That is what makes coverage
 * total without hand-mirroring a hundred fields — and it means a setting
 * added by a future Sunshine appears here on its own.
 */
export function HostSettings({ onClose }: Props) {
  const [config, setConfig] = useState<HostConfig | null>(null);
  const [edits, setEdits] = useState<HostConfig>({});
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    getHostConfig()
      .then(setConfig)
      .catch((err) => setError(String(err)));
  }, []);

  const groups = useMemo(() => {
    if (!config) return [];
    const byGroup = new Map<string, { setting: Setting; value: unknown }[]>();

    for (const [key, value] of Object.entries(config)) {
      if (METADATA_KEYS.has(key)) continue;
      const setting = settingFor(key, value);
      const list = byGroup.get(setting.group) ?? [];
      list.push({ setting, value });
      byGroup.set(setting.group, list);
    }

    // Curated groups keep their authored order; the catch-all sorts by key
    // so a long list of unfamiliar names is at least findable.
    return GROUPS.flatMap((name) => {
      const items = byGroup.get(name);
      if (!items) return [];
      if (name === "Everything else") {
        items.sort((a, b) => a.setting.key.localeCompare(b.setting.key));
      }
      return [{ name, items }];
    });
  }, [config]);

  const dirty = Object.keys(edits).length;

  function set(key: string, value: unknown) {
    setEdits((prev) => ({ ...prev, [key]: value }));
    setSaved(false);
  }

  async function save(restart: boolean) {
    setSaving(true);
    setError(null);
    try {
      await saveHostConfig(edits, restart);
      setConfig((prev) => (prev ? { ...prev, ...edits } : prev));
      setEdits({});
      setSaved(true);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
      <div className="axi-scrim" onClick={onClose} />
      <aside
        className="axi-drawer axi-drawer--full"
        role="dialog"
        aria-label="Host settings"
      >
        <div className="axi-drawer__head">
          <h2>Host settings</h2>
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

          {!config && !error && <p className="axi-ink-dim">Reading settings.</p>}

          {groups.map((group) => (
            <section key={group.name}>
              <h3 className="axi-eyebrow">{group.name}</h3>
              <div className="axi-stack">
                {group.items.map(({ setting, value }) => (
                  <Field
                    key={setting.key}
                    setting={setting}
                    value={edits[setting.key] ?? value}
                    changed={setting.key in edits}
                    onChange={(v) => set(setting.key, v)}
                  />
                ))}
              </div>
            </section>
          ))}
        </div>

        {config && (
          <div className="axi-modal__foot">
            {saved && <span className="axi-ink-ok dusk-row__end">Saved.</span>}
            <span className={saved ? "" : "dusk-row__end"} />
            <button
              type="button"
              className="axi-btn"
              disabled={!dirty || saving}
              onClick={() => save(false)}
            >
              {saving ? "Saving" : `Save${dirty ? ` ${dirty}` : ""}`}
            </button>
            <button
              type="button"
              className="axi-btn axi-btn--primary"
              disabled={!dirty || saving}
              onClick={() => save(true)}
            >
              Save and restart
            </button>
          </div>
        )}
      </aside>
    </>
  );
}

function Field({
  setting,
  value,
  changed,
  onChange,
}: {
  setting: Setting;
  value: unknown;
  changed: boolean;
  onChange(value: unknown): void;
}) {
  const { control } = setting;
  const id = `cfg-${setting.key}`;

  return (
    <div className="dusk-field">
      <label htmlFor={id}>
        {setting.label}
        {/* Unsaved edits are marked, because a long form gives no other clue
            about what the pending Save actually covers. */}
        {changed && <span className="axi-chip axi-chip--meta dusk-field__flag">Changed</span>}
      </label>

      {control.kind === "toggle" ? (
        <button
          type="button"
          id={id}
          className="axi-switch"
          role="switch"
          aria-checked={isOn(value)}
          onClick={() => onChange(toWire(control, !isOn(value)))}
        >
          <span className="axi-switch__knob" />
        </button>
      ) : control.kind === "select" ? (
        <select
          id={id}
          className="axi-select"
          value={String(value ?? "")}
          onChange={(e) => onChange(e.target.value)}
        >
          {/* A host may hold a value this build does not know about; showing
              it beats silently switching the setting to the first option. */}
          {!control.options.some((o) => o.value === String(value ?? "")) && (
            <option value={String(value ?? "")}>{String(value ?? "")}</option>
          )}
          {control.options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      ) : (
        <input
          id={id}
          className="axi-input"
          type={control.kind === "number" ? "number" : "text"}
          value={String(value ?? "")}
          placeholder={control.kind === "text" ? control.placeholder : undefined}
          min={control.kind === "number" ? control.min : undefined}
          max={control.kind === "number" ? control.max : undefined}
          onChange={(e) => onChange(e.target.value)}
        />
      )}

      {(setting.help || (control.kind === "number" && control.unit)) && (
        <p className="axi-ink-faint">
          {setting.help}
          {control.kind === "number" && control.unit
            ? `${setting.help ? " " : ""}Measured in ${control.unit}.`
            : ""}
        </p>
      )}
    </div>
  );
}
