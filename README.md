# Audio Capture

A local macOS desktop app that captures your microphone and your computer's system audio output as two separate, simultaneous, real-time streams — and keeps them separate all the way through the pipeline. Because the app runs locally, it always knows which stream is "you" (the microphone) and which is "everyone else" (system output, e.g. the other participants in a video call) — a cheap, reliable substitute for speaker diarization.

This is the capture foundation for a larger meeting/interview assistant (live transcription, summaries, contextual prompts, etc.), all of which is future work built on top of this pipeline rather than part of it yet.

## How it works, briefly

- A **Rust core** (`crates/audio-core`, wrapped by a **Tauri** app in `src-tauri`) captures the microphone directly via [`cpal`](https://github.com/RustAudio/cpal), and manages a small **Swift helper** subprocess (`swift-helper/`) that captures system audio using Apple's Core Audio Process Tap API (macOS 14.2+).
- Both sources publish onto a shared internal event bus, tagged by source — samples from the two are never merged.
- A minimal React/TypeScript UI shows a live level meter and status indicator for each stream, and start/stop controls.
- A supervisor watches both streams for signs of trouble (a crashed helper, a silently stalled capture) and restarts automatically.

See [`CLAUDE.md`](./CLAUDE.md) for a deeper architectural tour (module boundaries, threading model, the helper wire protocol) and [`docs/audio-tap-protocol.md`](./docs/audio-tap-protocol.md) for the exact byte-level protocol between the Swift helper and the Rust core.

## Requirements

- **macOS 14.4 or later**, Apple Silicon or Intel. The system-audio capture path depends on the Core Audio Process Tap API, which doesn't exist on earlier macOS versions or other platforms.
- **Xcode Command Line Tools** (`xcode-select --install`) — provides the Swift toolchain used to build the system-audio helper. Full Xcode isn't required for local development, only for code signing / notarization down the line.
- **Rust**, via [rustup](https://rustup.rs/):
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Node.js** (v20+) and **pnpm** (`corepack enable` or `npm install -g pnpm`).

## Setup

```sh
git clone <this-repo-url>
cd audio-capture
pnpm install
```

That's it for one-time setup — the Swift helper and Rust workspace are built automatically the first time you run the app or its tests.

## Running locally (development)

```sh
pnpm tauri dev
```

This starts the Vite dev server, builds the Rust workspace, builds the Swift helper on first run, and opens the app window. On first launch, macOS will prompt you separately for microphone access and for system-audio-capture access — you need to grant both for the two level meters to show activity. Click **Start capture** in the app to begin.

Rust source changes trigger an automatic rebuild and relaunch. Frontend changes hot-reload without a restart.

## Testing

```sh
cargo test --workspace
```

Runs the full Rust test suite (`crates/audio-core` and `src-tauri`), including a suite that spawns the Swift helper directly and checks its output is decodable. These tests don't require the app UI, but a couple of them touch real audio hardware/OS permissions and will skip themselves gracefully in environments where those aren't available (e.g. CI).

For frontend type-checking:

```sh
npx tsc --noEmit
```

There's also a set of manual verification tools under `crates/audio-core/examples/` for exercising real audio capture outside the app — see the "Examples" section of `CLAUDE.md` for what each one is for. Run one with, e.g.:

```sh
cargo run -p audio-core --example dual_capture -- 30 /tmp/mic.wav /tmp/system.wav
```

## Building an installable app

```sh
pnpm tauri build
```

Produces a native-architecture `.app` and `.dmg` under `target/release/bundle/`. For a universal binary that runs on both Apple Silicon and Intel Macs:

```sh
pnpm tauri build --target universal-apple-darwin
```

(This works even without Intel hardware to test on, by cross-building the Swift helper for x86_64 via Rosetta 2, if it's installed.)

**Note on distribution:** builds produced this way are ad-hoc signed, not notarized. That's sufficient to install and run on the machine that built it, or to hand to another Mac you control (you may need to right-click → Open the first time to bypass Gatekeeper's "unidentified developer" warning). Distributing to other people at scale would need a paid Apple Developer Program membership to notarize the build properly — nothing about the current architecture needs to change to add that later, it's a one-time signing/notarization setup step.

## Permissions

The app requests two separate macOS permissions the first time each capture path runs:

- **Microphone** — standard `NSMicrophoneUsageDescription` prompt.
- **Audio Capture** (system audio) — a separate, newer permission category specific to the Core Audio Process Tap API, distinct from Screen Recording (no recording indicator is shown).

If you deny a prompt by mistake, re-enable it under **System Settings → Privacy & Security → Microphone** / **Audio Capture**, and relaunch the app.
