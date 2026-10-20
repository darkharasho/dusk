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
  /** This copy cannot replace itself: a .deb, or a debug build. */
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

let state: UpdateState = { kind: "idle" };
let pending: Update | null = null;
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
  invoke<boolean>("updates_supported")
    .then((supported) => {
      if (!supported) {
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
  const before = state;
  set({ kind: "checking" });
  let update: Update | null;
  try {
    update = await check();
  } catch (err) {
    set(manual ? { kind: "error", message: String(err) } : before);
    return;
  }
  if (!update) {
    set(manual ? { kind: "upToDate" } : before);
    return;
  }

  const version = update.version;
  let total: number | null = null;
  let done = 0;
  set({ kind: "downloading", version, percent: null });
  try {
    await update.download((event) => {
      if (event.event === "Started") total = event.data.contentLength ?? null;
      if (event.event === "Progress") {
        done += event.data.chunkLength;
        set({
          kind: "downloading",
          version,
          percent: total ? Math.min(100, Math.round((done / total) * 100)) : null,
        });
      }
    });
  } catch (err) {
    set(manual ? { kind: "error", message: String(err) } : before);
    return;
  }
  pending = update;
  set({ kind: "ready", version });
}

/** Installs the downloaded update and starts the new Dusk in this one's place. */
export async function restartToUpdate() {
  if (state.kind !== "ready" || !pending) return;
  const version = state.version;
  set({ kind: "installing", version });
  try {
    // On Windows the installer takes over from here and ends this process.
    await pending.install();
    await invoke("ensure_self_executable");
    await relaunch();
  } catch (err) {
    set({ kind: "error", message: String(err) });
  }
}
