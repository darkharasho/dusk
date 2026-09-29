/**
 * How Dusk draws Sunshine's settings.
 *
 * Sunshine has on the order of a hundred settings and no schema endpoint, so
 * this file curates the ones worth a considered screen — a type, a plain
 * label and a sentence of help — and everything else is rendered generically
 * from whatever `GET /api/config` returns.
 *
 * That split is what makes total coverage affordable. A setting nobody has
 * curated still appears, still saves, and still round-trips; it just gets a
 * generic control and its raw key as a label. A new setting in a future
 * Sunshine shows up on its own rather than silently going missing, which is
 * the failure mode of a hand-mirrored form.
 *
 * The copy here is Dusk's own. Sunshine is GPL-3.0 and its help text is
 * creative work; key names and types are interface facts and are not.
 */

export type Control =
  | { kind: "text"; placeholder?: string }
  | { kind: "number"; min?: number; max?: number; unit?: string }
  | { kind: "toggle" }
  | { kind: "select"; options: { value: string; label: string }[] };

export interface Setting {
  key: string;
  label: string;
  help?: string;
  group: string;
  control: Control;
}

export const GROUPS = [
  "This host",
  "Picture",
  "Sound",
  "Network",
  "Input",
  "Everything else",
] as const;

function select(...options: [string, string][]): Control {
  return { kind: "select", options: options.map(([value, label]) => ({ value, label })) };
}

export const SETTINGS: Setting[] = [
  {
    key: "sunshine_name",
    label: "Name other machines see",
    help: "Defaults to this computer's hostname.",
    group: "This host",
    control: { kind: "text", placeholder: "Workshop PC" },
  },
  {
    key: "min_log_level",
    label: "Log detail",
    help: "Raise this only when something needs diagnosing.",
    group: "This host",
    control: select(
      ["0", "Everything"],
      ["1", "Debug"],
      ["2", "Info"],
      ["3", "Warnings"],
      ["4", "Errors"],
      ["5", "Serious errors only"],
      ["6", "Nothing"],
    ),
  },

  {
    key: "max_bitrate",
    label: "Bitrate ceiling",
    help: "0 lets the connecting machine decide.",
    group: "Picture",
    control: { kind: "number", min: 0, unit: "Kbps" },
  },
  {
    key: "minimum_fps_target",
    label: "Lowest frame rate to aim for",
    group: "Picture",
    control: { kind: "number", min: 0, unit: "fps" },
  },
  {
    key: "qp",
    label: "Quality level",
    help: "Lower looks better and costs more bandwidth. Used only by encoders without a bitrate mode.",
    group: "Picture",
    control: { kind: "number", min: 0, max: 51 },
  },
  {
    key: "encoder",
    label: "Encoder",
    help: "Leave empty to let Sunshine pick the best one it can find.",
    group: "Picture",
    control: { kind: "text", placeholder: "Chosen automatically" },
  },
  {
    key: "capture",
    label: "Capture method",
    help: "Leave empty unless capture is failing and you know which method works.",
    group: "Picture",
    control: { kind: "text", placeholder: "Chosen automatically" },
  },
  {
    key: "hevc_mode",
    label: "HEVC",
    group: "Picture",
    control: select(
      ["0", "Let Sunshine decide"],
      ["1", "Off"],
      ["2", "On"],
      ["3", "On, including 10-bit"],
    ),
  },
  {
    key: "av1_mode",
    label: "AV1",
    group: "Picture",
    control: select(
      ["0", "Let Sunshine decide"],
      ["1", "Off"],
      ["2", "On"],
      ["3", "On, including 10-bit"],
    ),
  },
  {
    key: "adapter_name",
    label: "Graphics adapter",
    help: "Only needed on a machine with more than one GPU.",
    group: "Picture",
    control: { kind: "text", placeholder: "Chosen automatically" },
  },
  {
    key: "output_name",
    label: "Display to capture",
    help: "Only needed on a machine with more than one screen.",
    group: "Picture",
    control: { kind: "text", placeholder: "Primary display" },
  },

  {
    key: "audio_sink",
    label: "Audio device to capture",
    group: "Sound",
    control: { kind: "text", placeholder: "System default" },
  },
  {
    key: "virtual_sink",
    label: "Virtual audio device",
    help: "Used to keep sound playing on the host silent while streaming.",
    group: "Sound",
    control: { kind: "text" },
  },

  {
    key: "port",
    label: "Base port",
    help: "Sunshine uses a small range starting here. Change it only if something else has the port.",
    group: "Network",
    control: { kind: "number", min: 1024, max: 65535 },
  },
  {
    key: "address_family",
    label: "Addresses to listen on",
    group: "Network",
    control: select(["ipv4", "IPv4 only"], ["both", "IPv4 and IPv6"]),
  },
  {
    key: "lan_encryption_mode",
    label: "Encryption on your own network",
    group: "Network",
    control: select(
      ["0", "Off"],
      ["1", "On where the client supports it"],
      ["2", "Required"],
    ),
  },
  {
    key: "wan_encryption_mode",
    label: "Encryption from outside your network",
    group: "Network",
    control: select(
      ["0", "Off"],
      ["1", "On where the client supports it"],
      ["2", "Required"],
    ),
  },
  {
    key: "ping_timeout",
    label: "Give up on a silent connection after",
    group: "Network",
    control: { kind: "number", min: 0, unit: "ms" },
  },
  {
    key: "fec_percentage",
    label: "Error correction",
    help: "Higher survives a lossier network and costs bandwidth.",
    group: "Network",
    control: { kind: "number", min: 1, max: 255, unit: "%" },
  },

  {
    key: "gamepad",
    label: "Controller type to emulate",
    group: "Input",
    control: { kind: "text", placeholder: "Chosen automatically" },
  },
  {
    key: "back_button_timeout",
    label: "Hold Back to send Home for",
    help: "-1 turns this off.",
    group: "Input",
    control: { kind: "number", unit: "ms" },
  },
  {
    key: "key_repeat_delay",
    label: "Key repeat delay",
    group: "Input",
    control: { kind: "number", min: 0, unit: "ms" },
  },
  {
    key: "always_send_scancodes",
    label: "Always send scancodes",
    help: "Helps games that read the keyboard directly.",
    group: "Input",
    control: { kind: "toggle" },
  },
];

const BY_KEY = new Map(SETTINGS.map((s) => [s.key, s]));

/** Reported by the config endpoint but not a setting; never shown or saved. */
export const METADATA_KEYS = new Set([
  "platform",
  "version",
  "status",
  "restart_supported",
]);

/**
 * Build a control for a key nobody has curated, from the value itself.
 *
 * Sunshine returns almost everything as a string, so `"true"` and `"28"` are
 * the shapes worth recognising — a toggle and a number are both far easier
 * to get right than a free-text box.
 */
export function inferControl(value: unknown): Control {
  if (typeof value === "boolean") return { kind: "toggle" };
  if (typeof value === "number") return { kind: "number" };

  if (typeof value === "string") {
    const v = value.trim().toLowerCase();
    if (v === "true" || v === "false" || v === "enabled" || v === "disabled") {
      return { kind: "toggle" };
    }
    // Only a plain integer; a version string or an address must stay text.
    if (/^-?\d+$/.test(v)) return { kind: "number" };
  }
  return { kind: "text" };
}

/** A key's curated entry, or a generic one derived from its value. */
export function settingFor(key: string, value: unknown): Setting {
  const curated = BY_KEY.get(key);
  if (curated) return curated;
  return {
    key,
    // The raw key is the honest label here: inventing a prettier name for a
    // setting we have not curated would make it harder to match against
    // Sunshine's own documentation.
    label: key,
    group: "Everything else",
    control: inferControl(value),
  };
}
