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

// Starts/stops the assistant pipeline (transcription + cue agent +
// suggestion forwarding). Requires capture to be running to hear anything,
// a Whisper model on disk, and ANTHROPIC_API_KEY in the environment.
// Modes are defined in crates/cue-agent/src/modes.rs.
export type AssistantMode = "sales" | "meeting" | "general";

export async function startAssistant(mode: AssistantMode): Promise<void> {
  await invoke("start_assistant", { mode });
}

export async function stopAssistant(): Promise<void> {
  await invoke("stop_assistant");
}

export async function stopSystemCapture(): Promise<void> {
  await invoke("stop_system_capture");
}
