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

/**
 * Pair with a host. Resolves once moonlight-qt has finished, which is only
 * after the PIN is accepted or the attempt times out — so callers should
 * keep the PIN on screen until this settles.
 */
export function pairDevice(id: string, pin: string): Promise<void> {
  return invoke("pair_device", { id, pin });
}

export function launchApp(id: string, appId: string): Promise<void> {
  return invoke("launch_app", { id, appId });
}

export function quitSession(id: string): Promise<void> {
  return invoke("quit_session", { id });
}

export function startHosting(): Promise<void> {
  return invoke("start_hosting");
}

export function stopHosting(): Promise<void> {
  return invoke("stop_hosting");
}

/** Verified against Sunshine before it is stored, so a typo surfaces here. */
export function signInHost(username: string, password: string): Promise<void> {
  return invoke("sign_in_host", { username, password });
}

export function signOutHost(): Promise<void> {
  return invoke("sign_out_host");
}

/** Accept an incoming pairing PIN on this machine. */
export function acceptPin(pin: string, deviceName?: string): Promise<void> {
  return invoke("accept_pin", { pin, deviceName: deviceName ?? null });
}

/**
 * A four-digit pairing PIN.
 *
 * Generated from the platform CSPRNG rather than Math.random: the PIN is the
 * only thing standing between a stranger on the network and a paired client,
 * so it should not be predictable from the clock.
 */
export function generatePin(): string {
  const bytes = new Uint32Array(1);
  crypto.getRandomValues(bytes);
  return String(bytes[0] % 10000).padStart(4, "0");
}

/** Backend pushes a full snapshot whenever discovery or polling changes state. */
export function onSnapshot(handler: (s: Snapshot) => void): Promise<() => void> {
  return listen<Snapshot>(SNAPSHOT_EVENT, (event) => handler(event.payload));
}
