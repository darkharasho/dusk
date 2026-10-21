import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

/**
 * Where Dusk's self-update is.
 *
 * An update downloads in the background as soon as it is found, and waits
 * for a restart rather than taking one: Dusk may be hosting or mid-stream,
 * and only the person at the machine knows whether now is a good time.
 */
export type UpdateState =
  /** This copy cannot replace itself: a debug build, or an unknown install. */
  | { kind: "unsupported" }
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "downloading"; version: string; percent: number | null }
  | { kind: "ready"; version: string }
  | { kind: "installing"; version: string }
  | { kind: "error"; message: string };

/** First look a little after launch, so it never competes with discovery. */
const FIRST_CHECK_MS = 10_000;
const RECHECK_MS = 6 * 60 * 60 * 1000;

/**
 * The two ways Dusk updates, behind one shape.
 *
 * Four of the five install formats are the updater plugin's. The fifth is
 * the `.deb`, which the plugin cannot install because the files belong to
 * dpkg — so the backend fetches and installs that one through the package
 * manager. The difference ends here: the states below, and everything that
 * draws them, are the same either way.
 */
type Channel = {
  /** The version waiting, or null if this copy is current. */
  check(): Promise<string | null>;
  /** Fetch it. `onProgress` gets a percentage where one is knowable. */
  download(onProgress: (percent: number | null) => void): Promise<void>;
  /** Put it in place. Returns once Dusk can be restarted into it. */
  install(): Promise<void>;
};

const pluginChannel: Channel = {
  async check() {
    pending = await check();
    return pending?.version ?? null;
  },
  async download(onProgress) {
    if (!pending) throw new Error("nothing to download");
    let total: number | null = null;
    let done = 0;
    await pending.download((event) => {
      if (event.event === "Started") total = event.data.contentLength ?? null;
      if (event.event === "Progress") {
        done += event.data.chunkLength;
        onProgress(total ? Math.min(100, Math.round((done / total) * 100)) : null);
      }
    });
  },
  async install() {
    if (!pending) throw new Error("nothing to install");
    // On Windows the installer takes over from here and ends this process.
    await pending.install();
    await invoke("ensure_self_executable");
  },
};

const debChannel: Channel = {
  async check() {
    const found = await invoke<{ version: string } | null>("check_deb_update");
    return found?.version ?? null;
  },
  async download(onProgress) {
    // No progress to report: the package is fetched and its signature
    // checked in one backend call. Saying nothing is better than inventing
    // a number, and the bar handles null by going indeterminate.
    onProgress(null);
    await invoke("download_deb_update");
  },
  async install() {
    await invoke("install_deb_update");
  },
};

let state: UpdateState = { kind: "idle" };
let pending: Update | null = null;
let channel: Channel | null = null;
let started = false;
const listeners = new Set<() => void>();

function set(next: UpdateState) {
  state = next;
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useUpdateState(): UpdateState {
  return useSyncExternalStore(subscribe, () => state);
}

/** Starts the launch check and the periodic one. Safe to call twice. */
export function startUpdates() {
  if (started) return;
  started = true;
  invoke<"none" | "plugin" | "deb">("update_channel")
    .then((which) => {
      channel = which === "plugin" ? pluginChannel : which === "deb" ? debChannel : null;
      if (!channel) {
        set({ kind: "unsupported" });
        return;
      }
      setTimeout(() => void checkForUpdate(false), FIRST_CHECK_MS);
      setInterval(() => void checkForUpdate(false), RECHECK_MS);
    })
    .catch(() => set({ kind: "unsupported" }));
}

/**
 * Looks for a newer release and downloads it if there is one.
 *
 * A check nobody asked for fails quietly: being offline is not news. One
 * from the button says what went wrong.
 */
export async function checkForUpdate(manual: boolean) {
  // Already found, already fetching, or already on its way in.
  if (["unsupported", "checking", "downloading", "ready", "installing"].includes(state.kind)) return;
  if (!channel) return;
  const before = state;
  set({ kind: "checking" });
  let version: string | null;
  try {
    version = await channel.check();
  } catch (err) {
    set(manual ? { kind: "error", message: String(err) } : before);
    return;
  }
  if (!version) {
    set(manual ? { kind: "upToDate" } : before);
    return;
  }

  set({ kind: "downloading", version, percent: null });
  try {
    await channel.download((percent) => set({ kind: "downloading", version, percent }));
  } catch (err) {
    set(manual ? { kind: "error", message: String(err) } : before);
    return;
  }
  set({ kind: "ready", version });
}

/**
 * Installs the downloaded update and starts the new Dusk in this one's place.
 *
 * On a .deb this is where the administrator prompt appears, which is why it
 * waits for someone to ask rather than happening when the download lands: a
 * password prompt nobody expected is indistinguishable from something nasty.
 */
export async function restartToUpdate() {
  if (state.kind !== "ready" || !channel) return;
  const version = state.version;
  set({ kind: "installing", version });
  try {
    await channel.install();
    await relaunch();
  } catch (err) {
    set({ kind: "error", message: String(err) });
  }
}
