import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Device, DownloadPreview, Setup, Snapshot } from "./types";

export const SNAPSHOT_EVENT = "dusk://snapshot";
export const SESSION_FAILED_EVENT = "dusk://session-failed";
export const SESSION_STATS_EVENT = "dusk://session-stats";

/** A stream that died on its own, after the launch call had already returned. */
export interface SessionFailed {
  deviceId: string;
  message: string;
}

/**
 * How a running stream is doing, about once a second. Any field can be
 * missing: a client sends what it measured rather than a misleading zero.
 */
export interface StreamStats {
  fps?: number | null;
  bitrateKbps?: number | null;
  rttMs?: number | null;
  decodeMs?: number | null;
  /** Percentage of frames the network dropped, 0 to 100. */
  packetLoss?: number | null;
}

export interface SessionStats {
  deviceId: string;
  stats: StreamStats;
}

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

/**
 * Give this machine's Sunshine a sign-in Dusk made up and keeps. Replaces
 * whatever password Sunshine had, and restarts it.
 */
export function provisionHost(): Promise<void> {
  return invoke("provision_host");
}

export function getSetup(): Promise<Setup> {
  return invoke<Setup>("get_setup");
}

/** What Dusk would download, without downloading it. */
export function previewSunshineDownload(): Promise<DownloadPreview> {
  return invoke<DownloadPreview>("preview_sunshine_download");
}

export const DOWNLOAD_EVENT = "dusk://download";

export interface DownloadProgress {
  received: number;
  total: number;
}

export function onDownloadProgress(
  handler: (p: DownloadProgress) => void,
): Promise<() => void> {
  return listen<DownloadProgress>(DOWNLOAD_EVENT, (e) => handler(e.payload));
}

/** Downloads and installs Sunshine. A real change to the machine. */
export function installSunshine(): Promise<void> {
  return invoke("install_sunshine");
}

/** Windows only; prompts for administrator rights when it runs. */
export function openFirewall(): Promise<void> {
  return invoke("open_firewall");
}

/** Linux: install the udev rule that lets Sunshine create input devices. */
export function allowInput(): Promise<void> {
  return invoke("allow_input");
}

/** Put a macOS privacy pane in front of someone; Dusk cannot grant it. */
export function openPrivacySettings(
  pane: "screenRecording" | "accessibility",
): Promise<void> {
  return invoke("open_privacy_settings", { pane });
}

export type HostConfig = Record<string, unknown>;

export function getHostConfig(): Promise<HostConfig> {
  return invoke<HostConfig>("get_host_config");
}

/**
 * Save changed settings. Only the changed keys are sent; the backend merges
 * them into the current config, because Sunshine's endpoint replaces rather
 * than patches.
 */
export function saveHostConfig(changes: HostConfig, restart: boolean): Promise<void> {
  return invoke("save_host_config", { changes, restart });
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

export function onSessionFailed(
  handler: (failure: SessionFailed) => void,
): Promise<() => void> {
  return listen<SessionFailed>(SESSION_FAILED_EVENT, (event) => handler(event.payload));
}

export function onSessionStats(
  handler: (update: SessionStats) => void,
): Promise<() => void> {
  return listen<SessionStats>(SESSION_STATS_EVENT, (event) => handler(event.payload));
}
