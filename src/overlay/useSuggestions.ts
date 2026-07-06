import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

// Consumes `overlay://suggestion` events. Today those come from the Rust
// mock generator; when the Tier-1 cue agent lands it will emit the same
// event, and this hook (and everything above it) won't change.

export interface Suggestion {
  id: string;
  source: "you" | "them";
  cue: string;
  hint: string;
  detail: string;
}

export interface SuggestionCardState extends Suggestion {
  /// True while the dismiss exit animation plays; the card is removed from
  /// state when it finishes.
  leaving: boolean;
}

// From the design board's motion spec: cards age out over 15–20s, dismiss
// animates over 150ms. CARD_LIFETIME_MS must match the `card-age` CSS
// animation duration in overlay.css.
export const CARD_LIFETIME_MS = 18_000;
export const DISMISS_ANIMATION_MS = 150;

export function useSuggestions(expandedId: string | null) {
  const [cards, setCards] = useState<SuggestionCardState[]>([]);
  const timersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  // Ref rather than dependency: the auto-dismiss timers consult the
  // *current* expanded card at fire time instead of being re-created on
  // every expand/collapse.
  const expandedIdRef = useRef(expandedId);
  expandedIdRef.current = expandedId;

  function beginLeaving(id: string) {
    setCards((prev) => prev.map((c) => (c.id === id ? { ...c, leaving: true } : c)));
    const removal = setTimeout(() => {
      setCards((prev) => prev.filter((c) => c.id !== id));
      timersRef.current.delete(id);
    }, DISMISS_ANIMATION_MS);
    timersRef.current.set(id, removal);
  }

  function scheduleAutoDismiss(id: string, delay: number) {
    const timer = setTimeout(() => {
      // An expanded card is pinned: the user is reading it, so it never
      // ages out from under them. Re-check after they collapse it.
      if (expandedIdRef.current === id) {
        scheduleAutoDismiss(id, 3_000);
        return;
      }
      beginLeaving(id);
    }, delay);
    timersRef.current.set(id, timer);
  }

  useEffect(() => {
    const unlistenPromise = listen<Suggestion>("overlay://suggestion", (event) => {
      const suggestion = event.payload;
      setCards((prev) => {
        // Guards React StrictMode's double-mounted listener in dev.
        if (prev.some((c) => c.id === suggestion.id)) return prev;
        return [{ ...suggestion, leaving: false }, ...prev];
      });
      scheduleAutoDismiss(suggestion.id, CARD_LIFETIME_MS);
    });

    const timers = timersRef.current;
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
      timers.forEach((t) => clearTimeout(t));
      timers.clear();
    };
  }, []);

  function dismiss(id: string) {
    const pending = timersRef.current.get(id);
    if (pending) clearTimeout(pending);
    beginLeaving(id);
  }

  return { cards, dismiss };
}
