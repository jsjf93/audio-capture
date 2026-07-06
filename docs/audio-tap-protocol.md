# Audio tap helper wire protocol

This is the single source of truth for the byte format the Swift helper
(`swift-helper/`) writes to its stdout, and the Rust parser
(`crates/audio-core/src/system_audio/protocol.rs`) reads from it. Both sides
are independent, hand-written implementations of this spec — there is no
shared codegen — so any change here must be applied to both, and any change
to either implementation should come with a round-trip test against this doc.

## Why stdout, and why a custom binary framing

Stdout was chosen over a Unix domain socket: this is a strict
1-parent(Rust):1-child(Swift) relationship that never needs to survive a
parent restart (a corrupted/hung stream is handled by restarting the child
entirely, not by reconnecting), so a socket's main advantages don't apply,
and "child exits → reader gets `EOF`" is exactly the supervisor signal we
want, for free.

A custom binary framing (rather than e.g. newline-delimited JSON for
everything) is used because audio payloads are large, frequent, and
performance-sensitive — but status/heartbeat messages are rare and
diagnostic, so those are plain JSON inside the same framing rather than a
second protocol.

## Header (12 bytes, every message)

| Offset | Size | Field         | Notes                                   |
|--------|------|---------------|------------------------------------------|
| 0      | 4    | `MAGIC`       | ASCII bytes `"ATAP"`                     |
| 4      | 1    | `VERSION`     | `1`                                       |
| 5      | 1    | `TYPE`        | `0` = Audio, `1` = StatusEvent, `2` = Heartbeat |
| 6      | 2    | `FLAGS`       | `u16`, little-endian, reserved (must be `0` for now) |
| 8      | 4    | `PAYLOAD_LEN` | `u32`, little-endian, byte length of the payload that follows |
| 12     | —    | `PAYLOAD`     | `PAYLOAD_LEN` bytes, meaning depends on `TYPE` |

All multi-byte integers in this spec are **little-endian** (matching both
Apple Silicon's and Rust's native `x86_64`/`aarch64` byte order — no
conversion needed on either side).

## Audio payload (`TYPE = 0`)

| Offset | Size | Field          | Notes                                        |
|--------|------|----------------|-----------------------------------------------|
| 0      | 8    | `timestamp_ns` | `u64` LE — monotonic, relative to the helper's own process start. **Not** wall-clock, and **not** comparable across processes without a documented offset — see the cross-process clock note below. |
| 8      | 4    | `sample_rate`  | `u32` LE, Hz                                  |
| 12     | 1    | `channels`     | `u8`                                          |
| 13     | 1    | `format`       | `0` = interleaved `f32`, little-endian. No other value is defined yet — treat anything else as a fatal framing error. |
| 14     | 4    | `frame_count`  | `u32` LE — number of *frames* (not samples); total sample count is `frame_count * channels` |
| 18     | —    | `samples`      | `frame_count * channels * 4` bytes, interleaved `f32` LE |

## StatusEvent payload (`TYPE = 1`)

UTF-8 JSON, e.g.:

```json
{"level": "error", "code": "tap_create_failed", "message": "AudioHardwareCreateProcessTap failed with OSStatus -66681"}
```

`level` is one of `"info"`, `"error"`. `code` is a short machine-readable
identifier (snake_case) for programmatic handling by the supervisor; `message`
is human-readable detail. Status events are low-frequency, so trading a few
bytes for JSON's debuggability (readable in a hex dump, parseable by hand) is
worth it — unlike audio payloads, this is not a hot path.

## Heartbeat payload (`TYPE = 2`)

Empty (`PAYLOAD_LEN = 0`). Emitted every ~250ms regardless of whether audio
frames are also flowing. This exists specifically so a supervisor can
distinguish "process alive but the tap has wedged" (heartbeats keep arriving,
audio frames stop) from "process genuinely hung or deadlocked" (nothing
arrives at all, including heartbeats) — audio-frame cadence alone can't tell
those apart, since a legitimately wedged tap might also just stop producing
audio frames without the process exiting.

## Error handling: no resynchronization

If a header fails to validate (`MAGIC` mismatch, unknown `VERSION`, or an
implausible `PAYLOAD_LEN`), the reader treats it as **fatal to this process
instance**: log it, stop reading, and (on the Rust side) let the supervisor
kill and restart the child. This spec deliberately does not define a
resynchronization scheme (e.g. scanning forward for the next `MAGIC`) —
restart-on-corruption is simpler to get right and is already the
supervisor's answer to the documented Core Audio failure modes (silent
all-zero decay, etc.), so corruption doesn't need a separate recovery path.

## Cross-process clock convention

Both sides emit `timestamp_ns` as **monotonic nanoseconds since that
process's own start** — the Swift helper's clock and the Rust mic-capture
clock are in different processes and are *not* comparable to each other
directly. Nothing in Phase 1–3 needs cross-stream timing alignment, but the
convention (monotonic, not wall-clock, documented per-process origin) is
fixed now so a future phase that needs to interleave mic/system-output
frames chronologically has a known contract to work from instead of an
implicit assumption to reverse-engineer later.
