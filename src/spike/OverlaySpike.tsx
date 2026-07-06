import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./OverlaySpike.css";

// THROWAWAY SPIKE — see src-tauri/src/overlay_spike.rs for the three
// questions this exists to answer. The suggestion cards are hardcoded
// fakes; nothing here is connected to the audio pipeline.
//
// Every element marked `data-interactive` gets its bounding rect reported
// to Rust, which hit-tests the global cursor against those rects to decide
// when the window should accept mouse events vs. pass clicks through to
// whatever is beneath it.

type Suggestion = { id: number; cue: string; hint: string; detail: string };

const FAKE_SUGGESTIONS: Suggestion[] = [
  {
    id: 1,
    cue: "“budget approval”",
    hint: "Ask who signs off on this purchase",
    detail:
      "They implied the decision isn't theirs alone. Ask: “Who else is involved in approving this, and what do they care about most?”",
  },
  {
    id: 2,
    cue: "“we tried something similar before”",
    hint: "Dig into why the last attempt failed",
    detail:
      "A past failure is your best qualification tool. Ask what specifically didn't work and who felt the pain — then position against that, not a generic competitor.",
  },
  {
    id: 3,
    cue: "“timeline”",
    hint: "Anchor a concrete next step",
    detail:
      "Vague timelines stall deals. Propose a specific date for the follow-up and name what each side brings to it.",
  },
];

export function OverlaySpike() {
  const [expandedId, setExpandedId] = useState<number | null>(null);
  const [passThrough, setPassThrough] = useState(true);
  // The window starts hidden from screen capture (sharingType = none, set
  // at creation); this mirrors that default. Demo mode flips it live.
  const [shareVisible, setShareVisible] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  // Rust tells us each time it flips ignoresMouseEvents, purely so the
  // person doing manual verification can watch the mode change live.
  useEffect(() => {
    const unlisten = listen<boolean>("overlay-spike://passthrough", (event) => {
      setPassThrough(event.payload);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // Report interactive rects to Rust. Layout shifts when a card expands or
  // the window resizes; a slow interval keeps the Rust-side hit-test rects
  // honest without a full ResizeObserver dance. Spike-grade, on purpose.
  useEffect(() => {
    function reportRegions() {
      const nodes = rootRef.current?.querySelectorAll("[data-interactive]") ?? [];
      const regions = Array.from(nodes).map((node) => {
        const r = (node as HTMLElement).getBoundingClientRect();
        return { x: r.x, y: r.y, width: r.width, height: r.height };
      });
      invoke("set_overlay_interactive_regions", { regions }).catch(() => {});
    }
    reportRegions();
    const id = setInterval(reportRegions, 250);
    return () => clearInterval(id);
  }, [expandedId]);

  return (
    <div className="spike-root" ref={rootRef}>
      <header className="spike-header" data-interactive data-tauri-drag-region>
        <span className="spike-title" data-tauri-drag-region>
          Overlay spike — drag me
        </span>
        <span className={`spike-mode ${passThrough ? "is-passthrough" : "is-interactive"}`}>
          {passThrough ? "pass-through" : "interactive"}
        </span>
        <button
          className="spike-close"
          onClick={() => invoke("close_overlay_spike").catch(() => {})}
          title="Close overlay"
        >
          ✕
        </button>
      </header>

      {FAKE_SUGGESTIONS.map((s) => (
        <div
          key={s.id}
          className={`spike-card ${expandedId === s.id ? "is-expanded" : ""}`}
          data-interactive
          onClick={() => setExpandedId(expandedId === s.id ? null : s.id)}
        >
          <div className="spike-cue">heard: {s.cue}</div>
          <div className="spike-hint">{s.hint}</div>
          {expandedId === s.id && <div className="spike-detail">{s.detail}</div>}
        </div>
      ))}

      <label className="spike-card spike-toggle" data-interactive>
        <input
          type="checkbox"
          checked={shareVisible}
          onChange={(e) => {
            const visible = e.target.checked;
            setShareVisible(visible);
            invoke("set_overlay_capture_visibility", { visible }).catch(() => {});
          }}
        />
        Visible in screen shares &amp; recordings
      </label>

      <p className="spike-note">
        The gaps between cards are holes — clicks there should land on whatever is behind
        this window. Cards should highlight on hover and expand on click.
      </p>
    </div>
  );
}
