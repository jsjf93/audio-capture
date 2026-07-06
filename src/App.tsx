import { useState } from "react";
import { LevelMeter } from "./components/LevelMeter";
import { CaptureControls } from "./components/CaptureControls";
import { useCaptureLevels } from "./hooks/useCaptureLevels";
import { useCaptureHealth } from "./hooks/useCaptureHealth";
import * as tauriApi from "./lib/tauriApi";
import "./App.css";

// Phase 3 UI (levels) + Phase 4 (health): two independent level meters
// driven by Tauri events from a single shared bus subscriber, plus a
// status dot per source driven by the Rust-side supervisor's health
// events. One Start/Stop control pair drives both sources together (the
// realistic way this will be used), but each source's status is tracked
// independently, since one can fail or go stale without the other being
// affected.

type CommandStatus = "idle" | "running" | "error";
type DisplayStatus = "idle" | "running" | "stale" | "error";

function resolveDisplayStatus(command: CommandStatus, health: string | null): DisplayStatus {
  if (command === "idle") return "idle";
  if (command === "error") return "error";
  // command === "running": defer to the live health state once one has
  // arrived; "starting"/"healthy" both just mean "working normally" from
  // the UI's point of view, "stale" means the supervisor is mid-restart,
  // and "failed" means it gave up (circuit breaker tripped).
  switch (health) {
    case "stale":
      return "stale";
    case "failed":
      return "error";
    default:
      return "running";
  }
}

function App() {
  const levels = useCaptureLevels();
  const health = useCaptureHealth();
  const [micCommandStatus, setMicCommandStatus] = useState<CommandStatus>("idle");
  const [systemCommandStatus, setSystemCommandStatus] = useState<CommandStatus>("idle");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const micStatus = resolveDisplayStatus(micCommandStatus, health.microphone);
  const systemStatus = resolveDisplayStatus(systemCommandStatus, health.system_output);
  const isRunning = micCommandStatus === "running" || systemCommandStatus === "running";

  function collectErrors(results: PromiseSettledResult<void>[]): string[] {
    return results
      .filter((r): r is PromiseRejectedResult => r.status === "rejected")
      .map((r) => String(r.reason));
  }

  async function handleStart() {
    setBusy(true);
    setError(null);

    const results = await Promise.allSettled([tauriApi.startMicCapture(), tauriApi.startSystemCapture()]);
    setMicCommandStatus(results[0].status === "fulfilled" ? "running" : "error");
    setSystemCommandStatus(results[1].status === "fulfilled" ? "running" : "error");

    const errors = collectErrors(results);
    if (errors.length > 0) setError(errors.join(" | "));
    setBusy(false);
  }

  async function handleStop() {
    setBusy(true);
    setError(null);

    const results = await Promise.allSettled([tauriApi.stopMicCapture(), tauriApi.stopSystemCapture()]);
    setMicCommandStatus("idle");
    setSystemCommandStatus("idle");

    const errors = collectErrors(results);
    if (errors.length > 0) setError(errors.join(" | "));
    setBusy(false);
  }

  return (
    <main className="container">
      <h1>Audio Capture</h1>
      <p>
        Microphone and system output are captured independently and never merged — that
        separation is what lets downstream features tell "you" apart from "everyone else"
        without speaker diarization.
      </p>

      <CaptureControls isRunning={isRunning} isBusy={busy} onStart={handleStart} onStop={handleStop} />

      <div className="meters">
        <LevelMeter label="Microphone (you)" rms={levels.microphone} status={micStatus} />
        <LevelMeter label="System output (everyone else)" rms={levels.system_output} status={systemStatus} />
      </div>

      {error && <p className="error-text">Error: {error}</p>}

      <p>
        <button onClick={() => tauriApi.openOverlay().catch((e) => setError(String(e)))}>
          Open overlay
        </button>
      </p>
    </main>
  );
}

export default App;
