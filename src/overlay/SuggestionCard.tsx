import { useEffect, useState } from "react";
import type { SuggestionCardState } from "./useSuggestions";

interface Props {
  card: SuggestionCardState;
  expanded: boolean;
  /// Another card is expanded — this one fades back (design state 04).
  receded: boolean;
  onToggle: () => void;
  onDismiss: () => void;
}

// Mocks the Tier-2 streaming experience client-side: a brief "Generating —"
// beat, then per-token reveal at the design's ~35ms cadence with a trailing
// block cursor. The real Tier-2 agent will stream actual tokens over an
// event; only this hook changes then.
const GENERATING_MS = 500;
const TOKEN_INTERVAL_MS = 35;

type StreamPhase = "idle" | "generating" | "streaming" | "done";

function useStreamedText(fullText: string, active: boolean) {
  const [phase, setPhase] = useState<StreamPhase>("idle");
  const [tokenCount, setTokenCount] = useState(0);

  useEffect(() => {
    if (!active) {
      setPhase("idle");
      setTokenCount(0);
      return;
    }
    setPhase("generating");
    const tokens = fullText.split(" ");
    let revealed = 0;
    let interval: number | undefined;
    const start = setTimeout(() => {
      setPhase("streaming");
      interval = window.setInterval(() => {
        revealed += 1;
        setTokenCount(revealed);
        if (revealed >= tokens.length) {
          window.clearInterval(interval);
          setPhase("done");
        }
      }, TOKEN_INTERVAL_MS);
    }, GENERATING_MS);
    return () => {
      clearTimeout(start);
      if (interval !== undefined) window.clearInterval(interval);
    };
  }, [active, fullText]);

  return {
    phase,
    visibleText: fullText.split(" ").slice(0, tokenCount).join(" "),
  };
}

export function SuggestionCard({
  card,
  expanded,
  receded,
  onToggle,
  onDismiss,
}: Props) {
  const { phase, visibleText } = useStreamedText(card.detail, expanded);

  const classes = [
    "card",
    `source-${card.source}`,
    expanded ? "is-expanded" : "",
    receded ? "is-receded" : "",
    card.leaving ? "is-leaving" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes} data-interactive onClick={onToggle}>
      <div className="card-cue-row">
        <span className="cue-dot" aria-hidden />
        <span className="cue-text">
          {card.source === "them" ? "heard:" : "you:"} &ldquo;{card.cue}&rdquo;
        </span>
        <span className="card-controls">
          <span className="chevron" aria-hidden>
            {expanded ? "⌃" : "⌄"}
          </span>
          <button
            className="card-dismiss"
            title="Dismiss"
            onClick={(e) => {
              e.stopPropagation();
              onDismiss();
            }}
          >
            ✕
          </button>
        </span>
      </div>

      <div className="card-hint">{card.hint}</div>

      <div className="card-detail-clip">
        <div className="card-detail">
          {phase === "generating" && (
            <span className="detail-generating">Generating —</span>
          )}
          {(phase === "streaming" || phase === "done") && (
            <span>
              {visibleText}
              {phase === "streaming" && (
                <span className="stream-cursor" aria-hidden />
              )}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
