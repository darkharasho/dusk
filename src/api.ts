import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Device, Snapshot } from "./types";

export const SNAPSHOT_EVENT = "dusk://snapshot";

export function getSnapshot(): Promise<Snapshot> {
  return invoke<Snapshot>("get_snapshot");
}

export function addManualDevice(address: string, name?: string): Promise<Device> {
  return invoke<Device>("add_manual_device", { address, name: name ?? null });
}

export function removeManualDevice(id: string): Promise<void> {
  return invoke("remove_manual_device", { id });
}

export function refreshNow(): Promise<void> {
  return invoke("refresh_now");
}

/** Backend pushes a full snapshot whenever discovery or polling changes state. */
export function onSnapshot(handler: (s: Snapshot) => void): Promise<() => void> {
  return listen<Snapshot>(SNAPSHOT_EVENT, (event) => handler(event.payload));
}
