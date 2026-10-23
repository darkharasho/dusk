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
  /** Remembered by moonlight-qt. Known, not necessarily reachable. */
  moonlight: boolean;
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
  /** Its id on its owner's account, when it told us. */
  machineId?: string;
  /** True when this machine is on *your* account's list. */
  mine: boolean;
  /** True when this Dusk is streaming from the machine right now. */
  streamingHere: boolean;
  /**
   * The machine answers on its plain port, but its pairing port has not
   * completed a handshake for a while. Pairing, streaming and ending a
   * session all go through that port, so nothing works until its hosting
   * is restarted.
   */
  securePortDown: boolean;
  isSelf: boolean;
  reachability: Reachability;
  pairing: PairingState;
  activity: Activity;
  server: ServerDetails | null;
  /** Only populated once paired — `applist` needs the client certificate. */
  apps: HostApp[];
  lastSeenMs: number | null;
  /** What the machine's own Dusk said about itself; absent until it does. */
  dusk?: DuskInfo;
}

export interface DuskInfo {
  version: string;
  /** It can update itself when another of your machines asks. */
  remoteUpdate: boolean;
}

export type HostPlatform = "windows" | "linux" | "macos" | "mock";

export interface HostCapabilities {
  /** Sunshine's own support tier for hosting on this platform. */
  supportTier: "supported" | "experimental" | "unsupported";
  /**
   * This platform can give a stream a screen the size of the device
   * watching. How it does that differs; see `virtualScreenMechanism`.
   */
  sizedScreen: boolean;
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

export type StepId =
  | "installSunshine"
  | "startService"
  | "provision"
  | "firewall"
  | "input"
  | "screenRecording"
  | "accessibility"
  | "systemAudio"
  | "virtualDisplay";

export type StepState =
  | { kind: "done" }
  | { kind: "todo" }
  /** Dusk cannot tell from here; the person confirms it themselves. */
  | { kind: "unknown" }
  | { kind: "notNeeded"; reason: string };

export interface Step {
  id: StepId;
  title: string;
  detail: string;
  state: StepState;
  /** False means the person has to do it, and `detail` says what. */
  automatable: boolean;
  /** Not every machine needs it, so "Set up this machine" leaves it alone. */
  optional: boolean;
}

export interface Setup {
  steps: Step[];
}

export interface DownloadPreview {
  version: string;
  asset: string;
  size: number;
  /** False when no checksum exists, in which case Dusk refuses to install. */
  verifiable: boolean;
  /** True when the installer ships inside Dusk and nothing is downloaded. */
  bundled: boolean;
  /** True on macOS, where hosting is inside Dusk and nothing is copied. */
  embedded: boolean;
}

/** The single payload the whole UI renders from. */
export interface Snapshot {
  devices: Device[];
  host: HostState;
  discovering: boolean;
  /** False when the streaming client is missing; nothing client-side works without it. */
  moonlightAvailable: boolean;
  /**
   * Whether the client Dusk found carries the in-stream overlay. Only Dusk's
   * fork has it; a stock Moonlight streams fine and simply has no floating
   * controls, which looks like a bug unless we say so. Null when Dusk was
   * pointed at a binary by hand and cannot tell.
   */
  overlayAvailable: boolean | null;
  /** Whether Dusk holds this machine's hosting sign-in. */
  /**
   * Null until the backend has read the keystore, which it does in the
   * background: on macOS that read can put a system password prompt on
   * screen, and the grid has to draw either way. Treat null as "not yet
   * known" rather than as false, or someone already signed in is shown a
   * sign-in form every launch.
   */
  hostSignedIn: boolean | null;
  /** False when the sign-in is only held for this run (no OS keystore). */
  hostCredentialsPersistent: boolean;
  /**
   * Whether machines outside this one's GitHub account may pair with it, by
   * an Allow press or a PIN. Your own machines pair without either.
   */
  allowOtherDevices: boolean;
  /** How big a stream from this machine asks to be. */
  resolution: Resolution;
  /** Kbps, or null to let the stream pick from its size and frame rate. */
  bitrateKbps: number | null;
  /** Which video format a stream asks the host for. */
  codec: Codec;
  /**
   * Whether streams from this machine are given a screen the size of the
   * connecting machine's.
   */
  virtualScreen: boolean;
  /** How this machine could do that, and `"none"` if it cannot. */
  virtualScreenMechanism: ScreenMechanism;
  /** Whether text copied during a stream is shared with the other machine. */
  clipboardSharing: boolean;
  /** Whether Dusk updates itself while nobody is streaming either way. */
  autoUpdate: boolean;
  /** Whether this copy can update with nobody at it at all. */
  autoUpdatePossible: boolean;
  /**
   * Hosting owes a restart to load a change to its paired machines, and why
   * it has not happened yet. Null when nothing is owed. Until it happens a
   * machine that paired with this one by account is told to try again in a
   * minute, so this is the one place that says what is going on.
   */
  hostRestart: string | null;
}

/**
 * How a machine sizes a stream's screen to the device watching: a virtual
 * display of its own (Windows, with the driver installed), the mode of the
 * output it is really using (Linux, on X11), or not at all.
 */
export type ScreenMechanism = "none" | "virtualDisplay" | "hostOutput";

export type Codec = "auto" | "h264" | "hevc" | "av1";

/**
 * How big a stream asks to be. Matching the host is the default: a stream
 * shaped differently from the host's screen gets bars the pointer cannot
 * reach into.
 */
export type Resolution =
  | { kind: "matchHost" }
  | { kind: "matchThisDisplay" }
  | { kind: "fixed"; width: number; height: number };

export interface GithubUser {
  id: number;
  login: string;
  avatarUrl: string;
}

/** The GitHub sign-in and the machines on its account. */
export interface AccountView {
  /** False when this build has no GitHub sign-in at all. */
  available: boolean;
  user: GithubUser | null;
  machines: { name: string; thisMachine: boolean }[];
  /** False when the sign-in is only held for this run (no OS keystore). */
  persistent: boolean;
  /**
   * Set when GitHub has stopped accepting the stored sign-in. Shown loudly:
   * the only other symptom is your own machines appearing not to be yours.
   */
  expired: string | null;
  /**
   * Set when the keystore would not say whether this machine is signed in.
   * Shown instead of the sign-in form: signing in again on a machine that
   * already is would mint it a second identity.
   */
  unreadable: string | null;
}

export interface SignInCode {
  userCode: string;
  verificationUrl: string;
  expiresIn: number;
}

/** Whether Dusk opens at sign-in, and how it opens and closes. */
export interface StartupPrefs {
  startInBackground: boolean;
  closeToTray: boolean;
}

export interface StartupView extends StartupPrefs {
  openAtLogin: boolean;
  /** Why opening at sign-in cannot be turned on from where Dusk is now. */
  openAtLoginBlocked: string | null;
}
