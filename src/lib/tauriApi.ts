import { invoke } from "@tauri-apps/api/core";

// Thin wrapper around the Tauri commands, kept in one place so the rest of
// the frontend doesn't need to know the exact command names or that
// `invoke` is how Tauri IPC works.

export async function startMicCapture(): Promise<void> {
  await invoke("start_mic_capture");
}

export async function stopMicCapture(): Promise<void> {
  await invoke("stop_mic_capture");
}

export async function startSystemCapture(): Promise<void> {
  await invoke("start_system_capture");
}

// Opens the suggestion overlay window (see src-tauri/src/overlay.rs).
export async function openOverlay(): Promise<void> {
  await invoke("open_overlay");
}

export async function stopSystemCapture(): Promise<void> {
  await invoke("stop_system_capture");
}
