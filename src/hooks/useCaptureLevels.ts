import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

export type SourceKind = "microphone" | "system_output";

interface LevelEvent {
  source: SourceKind;
  rms: number;
}

/// Subscribes to the `capture://level` events emitted by the Rust backend
/// (one per source, throttled to ~10Hz each — see `level_event_emitter` in
/// src-tauri/src/lib.rs) and keeps the latest reading per source. This
/// hook never sees raw audio: the backend deliberately only ever crosses
/// the IPC boundary with small, low-frequency derived numbers.
export function useCaptureLevels(): Record<SourceKind, number> {
  const [levels, setLevels] = useState<Record<SourceKind, number>>({
    microphone: 0,
    system_output: 0,
  });

  useEffect(() => {
    const unlistenPromise = listen<LevelEvent>("capture://level", (event) => {
      setLevels((prev) => ({ ...prev, [event.payload.source]: event.payload.rms }));
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  return levels;
}
