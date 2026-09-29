/**
 * Mirrors the serde representation of `src-tauri/src/model.rs`.
 *
 * These two files are hand-kept in sync. If the shape starts drifting, the fix
 * is to generate this file from the Rust side (ts-rs or specta) rather than to
 * patch it up — but one hand-written file is cheaper than a codegen step while
 * the model is still moving.
 */

export type DeviceId = string;

export type Reachability =
  | { kind: "online"; rttMs: number }
  | { kind: "offline" }
  | { kind: "unknown" };

/**
 * Whether *this* machine is paired with the device.
 *
 * Only knowable over the TLS port with a client certificate presented, which
 * Dusk does not own until M2. Until then this is always `unknown` and the UI
 * says so rather than guessing "not paired".
 */
export type PairingState =
  | { kind: "paired" }
  | { kind: "notPaired" }
  | { kind: "unknown" };

export type Activity =
  | { kind: "idle" }
  | { kind: "hosting"; appId: string | null; appName: string | null }
  | { kind: "unknown" };

export interface DeviceSource {
  mdns: boolean;
  manual: boolean;
}

export interface ServerDetails {
  hostname: string | null;
  mac: string | null;
  appVersion: string | null;
  localIp: string | null;
}

export interface HostApp {
  id: string;
  name: string;
  hdr: boolean;
}

export interface Device {
  id: DeviceId;
  name: string;
  addresses: string[];
  primaryAddress: string | null;
  httpPort: number;
  httpsPort: number;
  source: DeviceSource;
  isSelf: boolean;
  reachability: Reachability;
  pairing: PairingState;
  activity: Activity;
  server: ServerDetails | null;
  /** Only populated once paired — `applist` needs the client certificate. */
  apps: HostApp[];
  lastSeenMs: number | null;
}

export type HostPlatform = "windows" | "linux" | "macos" | "mock";

export interface HostCapabilities {
  /** Sunshine's own support tier for hosting on this platform. */
  supportTier: "supported" | "experimental" | "unsupported";
  virtualDisplay: boolean;
  gamepadInput: boolean;
  systemAudio: boolean;
  /** False where the OS requires a manual permission grant we cannot script. */
  automatedSetup: boolean;
  /** Human-readable caveats surfaced in the host setup flow. */
  caveats: string[];
}

export type HostStatus =
  | { kind: "notInstalled" }
  | { kind: "installed"; version: string | null; running: boolean }
  | { kind: "unknown"; reason: string };

export interface HostState {
  platform: HostPlatform;
  capabilities: HostCapabilities;
  status: HostStatus;
}

/** The single payload the whole UI renders from. */
export interface Snapshot {
  devices: Device[];
  host: HostState;
  discovering: boolean;
  /** False when moonlight-qt is missing; nothing client-side works without it. */
  moonlightAvailable: boolean;
}
