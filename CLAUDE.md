# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A macOS-first desktop app (Tauri + Rust + React/TS) that captures the microphone and system output audio as two independently-tagged, simultaneous real-time streams — never merged — so downstream features can tell "the user" apart from "everyone else in a meeting" without speaker diarization. This is the foundation for a larger meeting/interview assistant (transcription, summaries, live prompts, etc. are future work); right now it's just the capture pipeline.

## Commands

### Rust (workspace root)
- `cargo build --workspace` / `cargo test --workspace` — build/test `crates/audio-core`, `crates/transcribe`, and `src-tauri`.
- `cargo test -p audio-core <test_name>` — run a single test by name (substring match).
- `cargo run -p audio-core --example <name> -- <args>` — manual verification tools (see Examples below). These need real mic/audio-capture permissions and cannot run in CI.
- `bash scripts/download-model.sh [tiny.en|base.en|small.en]` — fetch a ggml Whisper model into `models/` (gitignored). Required before running any transcription example; default is `base.en`.
- **Broken-CLT workaround (this machine, July 2026):** the Command Line Tools install is missing its toolchain libc++ headers, so any C++ compile (`whisper-rs-sys` building whisper.cpp) fails with `'mutex' file not found` after a `cargo clean`. Prefix builds with `CXXFLAGS="-isystem /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/c++/v1"` until the CLT is reinstalled (`sudo rm -rf /Library/Developer/CommandLineTools && xcode-select --install`), after which this note should be deleted. Cached builds don't need it.

### Swift helper (`swift-helper/`)
- `swift build` (debug) / `swift build -c release` — builds `AudioTapHelper`. Requires macOS 14.4+.
- `bash scripts/build-helper.sh` — builds the release binary for the host architecture, best-effort cross-builds the other of arm64/x86_64 via Rosetta, and lipo's a universal sidecar if both succeed. Copies everything into `src-tauri/binaries/` under Tauri's `<name>-<target-triple>` sidecar naming convention. Run automatically by `tauri build` (wired into `beforeBuildCommand`).

### Frontend / app
- `pnpm install`, `pnpm tauri dev` — run the app in dev mode (hot-reloads frontend; Rust changes trigger a rebuild+relaunch).
- `npx tsc --noEmit` — typecheck the frontend.
- `pnpm tauri build` — native-arch release build + `.app`/`.dmg`.
- `pnpm tauri build --target universal-apple-darwin` — universal (arm64+x86_64) build. Requires both sidecar binaries already present in `src-tauri/binaries/` (run `build-helper.sh` first, or let `beforeBuildCommand` do it).
- Cargo/`rustc` aren't on PATH in a fresh shell on this machine (rustup added them to `.bash_profile`/`.profile` but the dev shell is zsh) — source `"$HOME/.cargo/env"` first if a `cargo`/`rustc` command isn't found.

## Architecture

### Crate/package boundaries (this is the load-bearing design decision)
- **`crates/audio-core`** — pure Rust, zero Tauri dependency by design. Owns all capture logic: sources, the event bus, the wire protocol, health/supervision primitives. Must remain buildable and testable standalone (`cargo test -p audio-core` needs no Tauri, no bundling, and — except for a couple of hardware-dependent tests that self-skip — no real hardware).
- **`crates/transcribe`** — pure Rust, zero Tauri dependency, same discipline as audio-core. The transcription stage: consumes bus frames as just another subscriber (capture code has no idea it exists), chunks them with an energy-based VAD (`chunker.rs` — deliberately simple, isolated so a trained VAD can replace it), resamples to Whisper's 16 kHz (`resample.rs`; rubato pinned to 0.16 — 3.0 rewrote the API), and runs local whisper.cpp inference (`whisper.rs`, via whisper-rs with Metal). The `Transcriber` trait is the seam where a cloud STT impl slots in later. `TranscriptSegment` carries the same `SourceKind` tag as the audio it came from — the you-vs-everyone-else separation must survive every pipeline stage.
- **`crates/cue-agent`** — pure Rust, zero Tauri dependency. The Tier-1 suggestion stage: subscribes to the `TranscriptBus`, maintains a rolling conversation context with trigger gating (`trigger.rs`: cooldown + minimum-new-words so filler doesn't burn model calls, plus a cross-stream echo guard that drops near-duplicate segments arriving on both streams when speaker audio bleeds into the mic — discovered necessary in real-world testing), and calls the Anthropic API (`anthropic.rs`: Haiku, tool-use for structured output where *not* calling a tool is the "nothing worth saying" path, prompt caching on the system prompt). **Memory** is a notes scratchpad: the model can call `update_notes` on any call and gets the notes back on every subsequent call — the verbatim window (~12k chars) is a recency window, not the memory ceiling. **Modes** (`modes.rs`: sales/meeting/general) are data, not code — a shared prompt skeleton plus a per-mode block (role, triggers, eagerness) and per-mode trigger tuning; `start_assistant` takes the mode id. Publishes to a `SuggestionBus`. The `SuggestionModel` trait is the test seam (fake models in `tests/agent_integration.rs`) and the future local-model seam. Needs `ANTHROPIC_API_KEY` at runtime, not at build time.
- **`src-tauri`** — the only thing that knows about Tauri. Owns IPC (commands/events), app state, and the supervision *policy* (audio-core provides the detection primitives; src-tauri decides what to do about it and owns the concrete source instances to restart). Also owns the assistant session (`start_assistant`/`stop_assistant`): transcription + cue agent + a forwarder that maps `Suggestion`s (domain `SourceKind`) to the overlay's `overlay://suggestion` events ("you"/"them") at the IPC boundary. Deliberately separate from capture start — capture without the assistant is a valid, cheaper mode, and the assistant has extra prerequisites (Whisper model on disk, API key).
- **`swift-helper`** — an independent Swift package (not a Cargo member), built separately and invoked as a subprocess. This isolation is deliberate: the Core Audio Process Tap API it uses is fragile and easy to get subtly wrong (see `docs/audio-tap-protocol.md`), so a crash or hang there can't take down the whole app.

### The `AudioSource` trait (`crates/audio-core/src/source.rs`)
The central abstraction. `MicrophoneSource` (`mic.rs`, via `cpal`), `SystemOutputSource` (`system_audio/source.rs`, spawns the Swift helper), and `FakeAudioSource` (`fake.rs`, replays fixed samples — used in tests and for hardware-free development) all implement it. Every implementation follows the same shape: a dedicated OS thread owns the real-time-sensitive work (a `cpal` stream, or reading a child process's stdout) and publishes `AudioFrame`s onto a shared `AudioBus`; nothing about *how* a source captures audio is visible to whatever consumes its frames. Adding a new consumer (a future transcription client, say) means subscribing to the bus — zero changes to any `AudioSource` impl.

Each frame carries a `SourceKind` (`Microphone` or `SystemOutput`) that is never stripped or merged — that tag *is* the substitute for diarization, and `bus_integration.rs` tests specifically assert no cross-contamination between simultaneous sources.

### Threading and channel discipline
Real-time callbacks (the `cpal` audio callback; the Swift helper's Core Audio IOProc) never block, allocate, or do I/O — they push into a lock-free ring buffer (`rtrb` on the Rust side) and an ordinary thread drains it. `AudioBus` (`bus.rs`) is a `tokio::sync::broadcast` wrapper: multi-consumer, non-blocking publish, and a lagging subscriber gets `Lagged(n)` and resumes rather than backpressuring the producer. This is a deliberate contract — the bus promises prompt best-effort delivery, not lossless delivery; a consumer that can't tolerate drops owns its own buffering.

### The helper protocol (`docs/audio-tap-protocol.md`)
Defines the exact binary framing the Swift helper writes to stdout and Rust (`system_audio/protocol.rs`) decodes — read that doc before touching either side. Two independent, hand-written implementations of the same spec (no shared codegen); if you change one, check the other and the round-trip tests in `protocol.rs`. Logs always go to stderr; only framed protocol bytes go to stdout, on both the Swift and Rust sides of every subprocess boundary in this project.

### Health/supervision (`crates/audio-core/src/health.rs`, wired in `src-tauri/src/lib.rs`)
`StalenessWatcher` watches the bus for a specific `SourceKind` going quiet — this is generic and applies to *either* source, not just the system-audio helper, because a real bug once caused the microphone source to silently stop publishing frames while the OS-level audio callback kept firing the entire time (see git history / the mic.rs comment on the `filled` variable if it's still there). Don't assume only the Swift helper can fail silently. `RestartPolicy` is the circuit breaker (default: 5 restarts/60s) that stops a permanently-broken source from crash-looping. Supervision is scoped to an active capture session (started in `start_mic_capture`/`start_system_capture`, aborted on stop) specifically so it can never fire before anything has actually started.

### Tauri IPC boundary
Raw audio frames never cross into the webview. The frontend only ever sees small, throttled, derived events: `capture://level` (per-source RMS, ~10Hz) and `capture://health` (state transitions only, not polled). Control flows the other way via commands (`start_mic_capture`, `stop_mic_capture`, `start_system_capture`, `stop_system_capture`) — there is deliberately no single combined "start everything" command on the Rust side; the frontend (`src/App.tsx`) composes the two calls, so each source's status can be tracked and shown independently.

### The suggestion overlay (`src-tauri/src/overlay.rs` + `src/overlay/`)
A second Tauri window (label `overlay`): transparent, undecorated, always-on-top including over other apps' fullscreen Spaces (`fullScreenAuxiliary` + window level 25 via raw objc2 `msg_send`, main thread only), and hidden from screen capture by default (`sharingType = none`, runtime-toggleable — the DEMO pill). Requires `macOSPrivateApi: true` + the `macos-private-api` tauri cargo feature for true transparency. Per-region click-through works by polling the global cursor from a Rust thread against frontend-reported `[data-interactive]` rects and flipping `ignoresMouseEvents` — the webview can't do this itself because an ignoring window receives no mouse events at all. `data-tauri-drag-region` needs the `core:window:allow-start-dragging` capability permission or it silently no-ops. Suggestions arrive as `overlay://suggestion` events from the assistant pipeline's forwarder in lib.rs — the overlay knows nothing about transcription or the LLM. Design source of truth: `docs/overlay-design-brief.md` plus the motion/accent rules commented in `src/overlay/overlay.css` (a few numbers deviate from the design board deliberately, from real-world dark-background testing; the comments say which).

### Sidecar bundling gotcha
Tauri's `externalBin` mechanism expects source files on disk named `<name>-<target-triple>` but **strips the triple suffix** when copying into the bundled `.app`'s `Contents/MacOS/` — the running app looks for a plain `audio-tap-helper` next to its own executable in a release build (see `resolve_bundled_helper_path` in `src-tauri/src/lib.rs`), not the suffixed name. For a universal build, `tauri build --target universal-apple-darwin` does *not* lipo the sidecar itself — you must provide a pre-merged `audio-tap-helper-universal-apple-darwin` (which is what `scripts/build-helper.sh` does automatically when both arch-specific builds succeed).

### Examples as verification tools (`crates/audio-core/examples/`)
These aren't demos — they're the project's manual/integration verification story, since OS-level audio capture correctness can't run in CI:
- `mic_rms.rs` — mic-only sanity check.
- `dual_capture.rs` — the core proof: captures both streams simultaneously to separate WAV files, for confirming zero bleed between them.
- `decode_tap_capture.rs` / `inspect_wav.rs` — decode a raw helper capture or inspect any WAV's per-second RMS, to eyeball whether a capture is real audio vs. silence/garbage.
- `mic_only_long_run.rs` — isolates whether a mic issue is mic-specific or an interaction with concurrent system-audio capture.

And in `crates/transcribe/examples/` (both need a model — run `scripts/download-model.sh` first):
- `mic_transcribe.rs` — live mic → VAD → Whisper → console, printing per-chunk latency. The transcription phase's manual milestone check.
- `transcribe_wav.rs` — offline transcription of any WAV; hardware-free verification (pairs well with `say -o test.wav --data-format=LEF32@22050 "known text"`) and the seed of the future record-and-replay evaluation harness.
- `dual_transcribe.rs` — both streams transcribed live via `run_transcription` + `TranscriptBus`, printed as tagged `[you]`/`[them]` lines. The text-level equivalent of `dual_capture`'s no-bleed proof (also needs the Swift helper built).
