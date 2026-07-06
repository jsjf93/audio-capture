import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { SourceKind } from "./useCaptureLevels";

export type HealthState = "starting" | "healthy" | "stale" | "failed";

interface HealthEvent {
  source: SourceKind;
  state: HealthState;
}

// The supervisor's actual recover cycle (detect staleness, restart the
// helper, see frames resume) can complete in well under two seconds — fast
// enough that the "stale" state flashes and clears before a human glancing
// at the dot ever registers it, even though the recovery genuinely
// happened (confirmed independently via the backend's own logs during
// Phase 4 chaos testing). This isn't a bug in the health signal itself,
// just the display being too literal for a human to usefully perceive —
// so once "stale" is shown, it's held for at least this long before a
// "healthy" event is allowed to clear it. The underlying event stream
// (and anything else that might consume it later) is untouched; this
// delay is purely cosmetic, applied only to what this hook exposes.
const MIN_STALE_DISPLAY_MS = 2000;

/// Subscribes to `capture://health`, emitted by the Rust-side supervisor
/// (see `supervise_mic`/`supervise_system` in src-tauri/src/lib.rs) only
/// while a capture session is active. `stale` means the supervisor noticed
/// a source stop publishing frames and is attempting a restart; `failed`
/// means it gave up after repeated restarts (the circuit breaker tripped).
export function useCaptureHealth(): Record<SourceKind, HealthState | null> {
  const [health, setHealth] = useState<Record<SourceKind, HealthState | null>>({
    microphone: null,
    system_output: null,
  });
  const staleSinceRef = useRef<Record<SourceKind, number | null>>({
    microphone: null,
    system_output: null,
  });
  const pendingTimerRef = useRef<Record<SourceKind, ReturnType<typeof setTimeout> | null>>({
    microphone: null,
    system_output: null,
  });

  useEffect(() => {
    const unlistenPromise = listen<HealthEvent>("capture://health", (event) => {
      const { source, state } = event.payload;
      const existingTimer = pendingTimerRef.current[source];
      if (existingTimer) {
        clearTimeout(existingTimer);
        pendingTimerRef.current[source] = null;
      }

      if (state === "stale" || state === "failed") {
        staleSinceRef.current[source] = Date.now();
        setHealth((prev) => ({ ...prev, [source]: state }));
        return;
      }

      // Recovering to "starting"/"healthy": hold the stale/failed display
      // for at least MIN_STALE_DISPLAY_MS from when it started.
      const staleSince = staleSinceRef.current[source];
      const elapsed = staleSince ? Date.now() - staleSince : MIN_STALE_DISPLAY_MS;
      const remaining = Math.max(0, MIN_STALE_DISPLAY_MS - elapsed);

      if (remaining === 0) {
        setHealth((prev) => ({ ...prev, [source]: state }));
      } else {
        pendingTimerRef.current[source] = setTimeout(() => {
          setHealth((prev) => ({ ...prev, [source]: state }));
          pendingTimerRef.current[source] = null;
        }, remaining);
      }
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
      Object.values(pendingTimerRef.current).forEach((timer) => {
        if (timer) clearTimeout(timer);
      });
    };
  }, []);

  return health;
}
