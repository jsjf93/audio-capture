import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SuggestionCard } from "./SuggestionCard";
import { useSuggestions } from "./useSuggestions";
import { useInteractiveRegions } from "./useInteractiveRegions";
import { useCaptureHealth, type HealthState } from "../hooks/useCaptureHealth";
import "./overlay.css";

// The suggestion overlay, built to the design board ("Quiet backup, not a
// second participant"): a handle pill with audio-health dots, a stack of
// up to 3 suggestion cards (newest in front, older ones aging out), click
// to expand with streamed guidance, and a deliberately loud DEMO pill for
// the one state that must never be subtle — being visible to a screen
// share.

const MAX_VISIBLE_CARDS = 3;

function dotClass(state: HealthState | null): string {
  switch (state) {
    case "healthy":
      return "dot-green";
    case "starting":
    case "stale":
      return "dot-amber";
    case "failed":
      return "dot-red";
    default:
      return "dot-idle"; // no active capture session
  }
}

export function OverlayApp() {
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [demoMode, setDemoMode] = useState(false);
  const { cards, dismiss } = useSuggestions(expandedId);
  const health = useCaptureHealth();
  const rootRef = useRef<HTMLDivElement>(null);

  useInteractiveRegions(rootRef, [cards, expandedId, demoMode]);

  const visibleCards = cards.slice(0, MAX_VISIBLE_CARDS);
  const earlierCount = Math.max(0, cards.length - MAX_VISIBLE_CARDS);

  function toggleDemoMode() {
    const next = !demoMode;
    setDemoMode(next);
    invoke("set_overlay_capture_visibility", { visible: next }).catch(() => {});
  }

  function handleDismiss(id: string) {
    if (expandedId === id) setExpandedId(null);
    dismiss(id);
  }

  return (
    <div className="overlay-root" ref={rootRef}>
      <div className="top-row">
        {demoMode && (
          <button className="demo-pill" data-interactive onClick={toggleDemoMode} title="Click to hide from screen shares again">
            <span className="demo-dot" aria-hidden />
            DEMO — VISIBLE TO SHARE
          </button>
        )}
        <div className="handle-pill" data-interactive data-tauri-drag-region>
          <span className="grip" data-tauri-drag-region aria-hidden>
            ⠿
          </span>
          <span className="pill-divider" aria-hidden />
          <span className={`health-dot ${dotClass(health.microphone)}`} title="Microphone (you)" />
          <span className={`health-dot ${dotClass(health.system_output)}`} title="System audio (them)" />
          {!demoMode && (
            <button className="pill-btn" onClick={toggleDemoMode} title="Make visible in screen shares (demo mode)">
              ◎
            </button>
          )}
          <button className="pill-btn" onClick={() => invoke("close_overlay").catch(() => {})} title="Close overlay">
            ✕
          </button>
        </div>
      </div>

      {earlierCount > 0 && <div className="earlier-note">+{earlierCount} earlier</div>}

      <div className="stack">
        {visibleCards.map((card) => (
          <SuggestionCard
            key={card.id}
            card={card}
            expanded={expandedId === card.id}
            receded={expandedId !== null && expandedId !== card.id}
            onToggle={() => setExpandedId(expandedId === card.id ? null : card.id)}
            onDismiss={() => handleDismiss(card.id)}
          />
        ))}
      </div>
    </div>
  );
}
