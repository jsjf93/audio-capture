# AudioTapHelper

A small standalone Swift executable that captures system audio output (everything currently playing through your speakers, across all apps) using Apple's Core Audio Process Tap API, and streams it as framed binary messages on stdout. It's built and invoked automatically by the main app as a subprocess — you won't normally need to touch this directly, but it's useful to know how to build and exercise it on its own when debugging system-audio capture specifically.

Human-readable logs go to **stderr**; only the binary framing protocol goes to **stdout**. The exact byte format is documented in [`../docs/audio-tap-protocol.md`](../docs/audio-tap-protocol.md) — read that before changing anything in `Framing.swift` or `ProcessTap.swift`.

## Requirements

macOS 14.4+ (the Core Audio Process Tap API doesn't exist on earlier versions) and the Swift toolchain from Xcode Command Line Tools (`xcode-select --install`).

## Building

```sh
swift build            # debug
swift build -c release # release
```

## Running standalone

Since it's a normal executable, you can run it directly and inspect its output without involving the Rust/Tauri side at all:

```sh
swift build
.build/debug/AudioTapHelper > /tmp/capture.bin
```

It captures until you send it `SIGINT` or `SIGTERM` (Ctrl-C works). The raw output isn't audio you can play directly — it's framed messages (audio + status + heartbeat, per the protocol doc). To turn a capture into a WAV file you can actually listen to, use the Rust-side decoder from the repo root:

```sh
cargo run -p audio-core --example decode_tap_capture -- /tmp/capture.bin /tmp/capture.wav
```

## Why this is a separate process at all

The Core Audio Process Tap API is fragile in ways that are easy to get subtly wrong (see the comments in `ProcessTap.swift` for specific documented footguns). Running it in its own process means a crash or hang here can't take down the rest of the app — the Rust side just sees the pipe close and can restart it. See `CLAUDE.md` at the repo root for more on how this fits into the overall architecture.
