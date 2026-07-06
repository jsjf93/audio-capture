import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

// Reports the bounding rects of every `[data-interactive]` element to
// Rust, which hit-tests the global cursor against them to flip the
// window's click-through state (see spawn_cursor_poller in
// src-tauri/src/overlay.rs). The slow re-report interval keeps the rects
// honest through layout changes we don't explicitly track — including the
// design's slow age-out scale animation, which drifts card geometry
// continuously.
export function useInteractiveRegions(
  rootRef: React.RefObject<HTMLElement | null>,
  deps: unknown[],
) {
  useEffect(() => {
    function report() {
      const nodes = rootRef.current?.querySelectorAll("[data-interactive]") ?? [];
      const regions = Array.from(nodes).map((node) => {
        const r = (node as HTMLElement).getBoundingClientRect();
        return { x: r.x, y: r.y, width: r.width, height: r.height };
      });
      invoke("set_overlay_interactive_regions", { regions }).catch(() => {});
    }
    report();
    const id = setInterval(report, 250);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
