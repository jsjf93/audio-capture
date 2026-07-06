# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A macOS-first desktop app (Tauri + Rust + React/TS) that captures the microphone and system output audio as two independently-tagged, simultaneous real-time streams — never merged — so downstream features can tell "the user" apart from "everyone else in a meeting" without speaker diarization. This is the foundation for a larger meeting/interview assistant (transcription, summaries, live prompts, etc. are future work); right now it's just the capture pipeline.

## Commands

### Rust (workspace root)
- `cargo build --workspace` / `cargo test --workspace` — build/test both `crates/audio-core` and `src-tauri`.
- `cargo test -p audio-core <test_name>` — run a single test by name (substring match).
- `cargo run -p audio-core --example <name> -- <args>` — manual verification tools (see Examples below). These need real mic/audio-capture permissions and cannot run in CI.

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
- **`src-tauri`** — the only thing that knows about Tauri. Owns IPC (commands/events), app state, and the supervision *policy* (audio-core provides the detection primitives; src-tauri decides what to do about it and owns the concrete source instances to restart).
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

### Sidecar bundling gotcha
Tauri's `externalBin` mechanism expects source files on disk named `<name>-<target-triple>` but **strips the triple suffix** when copying into the bundled `.app`'s `Contents/MacOS/` — the running app looks for a plain `audio-tap-helper` next to its own executable in a release build (see `resolve_bundled_helper_path` in `src-tauri/src/lib.rs`), not the suffixed name. For a universal build, `tauri build --target universal-apple-darwin` does *not* lipo the sidecar itself — you must provide a pre-merged `audio-tap-helper-universal-apple-darwin` (which is what `scripts/build-helper.sh` does automatically when both arch-specific builds succeed).

### Examples as verification tools (`crates/audio-core/examples/`)
These aren't demos — they're the project's manual/integration verification story, since OS-level audio capture correctness can't run in CI:
- `mic_rms.rs` — mic-only sanity check.
- `dual_capture.rs` — the core proof: captures both streams simultaneously to separate WAV files, for confirming zero bleed between them.
- `decode_tap_capture.rs` / `inspect_wav.rs` — decode a raw helper capture or inspect any WAV's per-second RMS, to eyeball whether a capture is real audio vs. silence/garbage.
- `mic_only_long_run.rs` — isolates whether a mic issue is mic-specific or an interaction with concurrent system-audio capture.
