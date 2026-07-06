# Design brief: real-time conversation-assistant overlay

> This document is a self-contained prompt for a design tool/designer. Everything
> in "Hard technical constraints" is already proven working in a spike — design
> to it with confidence, don't design around it.

## What this product is

A macOS desktop assistant for live conversations — sales calls and job
interviews are the anchor scenarios. While the user is in a call, the app
listens to both sides (the user's mic and everyone else via system audio),
transcribes in real time, and an AI agent surfaces small, timely suggestion
cards: a follow-up question worth asking, a topic worth expanding on, a cue the
user might have missed. Clicking a card expands it into deeper, streamed-in
guidance.

The emotional job: **quiet backup, not a second meeting participant.** The user
is mid-sentence with a real human; this thing must feel like a calm colleague
sliding a post-it note across the desk, never like a dashboard demanding
attention. If a design choice trades subtlety for richness, choose subtlety.

## The surface being designed

One floating overlay window, roughly 320–420px wide, portrait-ish, sitting on
top of a video call (Zoom/Meet/Teams, often fullscreen). The user positions and
sizes it; it typically lives at a screen edge beside the call window.

Scope: **the overlay only.** The main app window (settings, session history)
is a later, separate brief.

## Hard technical constraints (all verified working)

- The window is **truly transparent**: anything not painted is not there —
  visually *and* interactively. Clicks in unpainted gaps land on the app
  beneath. Only the cards/controls themselves are hover- and clickable. Design
  can and should exploit this: the overlay is a loose stack of floating
  elements, not a panel with a background.
- It floats above fullscreen apps and stays on all Spaces/desktops.
- It is **invisible to screen sharing and recordings by default**, with a
  user-facing "demo mode" toggle that makes it capturable. The current mode
  must be subtly but unambiguously visible to the user (sharing an overlay you
  believed was hidden is the nightmare scenario).
- Rendered with web tech (React/CSS): gradients, shadows, rounded corners,
  SVG, CSS animation all fine. One exception: **no frosted-glass/backdrop-blur
  of the screen behind the window** — the web layer cannot sample other
  windows. Cards must carry their own opacity and contrast.
- The desktop behind it is arbitrary and changes constantly (dark video grid,
  white shared slides, anything). Legibility cannot depend on the background.
- It must never steal keyboard focus from the call; assume pointer-only
  interaction, no text input.
- The user drags it by a handle region and can resize it.

## Content anatomy of a suggestion card

Each card carries:

1. **Trigger cue** — the fragment of conversation that prompted it, e.g.
   `heard: "budget approval"`. Small, secondary; it builds trust ("it's
   actually listening") and lets the user judge relevance in ~200ms.
2. **Who said it** — the cue came either from the user ("you") or from the
   other side ("them"). A quiet visual distinction, not a loud label.
3. **The suggestion** — one line, imperative, ≤ ~12 words. This is the payload
   and the visual anchor: *"Ask who signs off on this purchase."*
4. **Expanded detail** (on click) — 2–4 sentences of deeper guidance that
   **streams in** token-by-token from a slower AI model; needs a
   loading/streaming treatment.
5. **Age** — suggestions go stale fast in live conversation. Older cards
   should visibly recede (fade/compress) and eventually self-dismiss.

## States to design

- **Idle/listening** — call in progress, no active suggestions. Near-invisible:
  perhaps just the drag handle and a minimal "listening" indicator with mic +
  system-audio health (two tiny status dots — they already exist conceptually
  in the product as green/amber/red per audio source).
- **New suggestion arrives** — attention without alarm. No sound. Subtle
  entrance; the user may be talking, so nothing that demands a saccade.
- **The stack** — up to ~3 cards visible, newest most prominent, older ones
  receding. Overflow indicator if more exist. Excess must never crawl toward
  covering the call.
- **Hover** — card becomes visibly interactive (this is also the moment the
  window technically switches from pass-through to clickable, so a clear hover
  affordance genuinely matters).
- **Expanded** — one card open, with streaming text arriving; a way to
  collapse/dismiss; other cards de-emphasized.
- **Dismissed** — quick single-gesture dismissal per card.
- **Paused/error** — capture stopped or a source went stale; the overlay
  should show "not listening" honestly but quietly.
- **Demo mode on** — the visible-to-screen-share state indicator.

## Configurability to accommodate (design the *range*, not a settings UI)

- Position: anywhere on screen (user-dragged); consider edge-snapping.
- Density: comfortable vs. compact (compact for small screens beside a call).
- Max visible cards (1–5).
- Card opacity (a floor above which text stays legible).
- Light/dark: default dark-glass cards that survive any background;
  a light variant is optional.

## Reading-time budget

The user glances at a card for **1–2 seconds while actively talking**. Type
hierarchy, contrast, and line length must make the one-line suggestion fully
absorbable in that window; everything else is progressive disclosure.

## Anti-goals

- Not a chat window; not a transcript viewer (a transcript may exist elsewhere,
  the overlay only surfaces distilled cues).
- No anthropomorphic assistant character, no avatar, no "AI is thinking..."
  theatrics beyond a minimal streaming affordance.
- No heavy branding, no logo on the overlay.
- Nothing that pulses, bounces, or animates continuously while idle.

## Deliverables wanted

- Visual direction for the card system (shape, color, type scale, elevation)
  shown over both a dark video-grid background and a white shared-document
  background.
- Every state listed above, at comfortable and compact density.
- Motion notes: entrance, hover, expand, streaming, recede, dismiss — with
  duration guidance (fast: this is a utility, 150–250ms territory).
- The idle/listening treatment, including the two audio-health dots and the
  demo-mode indicator.
