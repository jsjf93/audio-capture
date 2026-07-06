import AudioToolbox
import CoreAudio
import Dispatch
import Foundation

// Phase 2: capture system output audio via a Core Audio Process Tap and
// stream it to stdout using the framing protocol documented in
// docs/audio-tap-protocol.md. Logs go to stderr; only framed protocol
// bytes ever go to stdout — see the same discipline established in
// Phase 0's hello-world version of this file.

func logLine(_ message: String) {
    FileHandle.standardError.write(Data("[audio-tap-helper] \(message)\n".utf8))
}

func writeStdout(_ data: Data) {
    FileHandle.standardOutput.write(data)
}

let processStartTime = DispatchTime.now().uptimeNanoseconds

let stateLock = NSLock()
var runningFlag = true

func isRunning() -> Bool {
    stateLock.lock()
    defer { stateLock.unlock() }
    return runningFlag
}

func requestStop() {
    stateLock.lock()
    runningFlag = false
    stateLock.unlock()
}

signal(SIGINT) { _ in requestStop() }
signal(SIGTERM) { _ in requestStop() }

logLine("starting (pid \(ProcessInfo.processInfo.processIdentifier))")

let tap = ProcessTap()

do {
    try tap.start()
    logLine("tap started: \(tap.format.mChannelsPerFrame)ch @ \(Int(tap.format.mSampleRate))Hz, \(tap.format.mBytesPerFrame) bytes/frame")
} catch {
    let tapError = error as? ProcessTapError
    let message = tapError?.description ?? String(describing: error)
    let code = tapError?.code ?? "tap_start_failed"
    writeStdout(Framing.statusEvent(level: "error", code: code, message: message))
    logLine("FATAL: \(message)")
    exit(1)
}

let sampleRate = UInt32(tap.format.mSampleRate)
let channels = UInt8(tap.format.mChannelsPerFrame)
let bytesPerFrame = Int(tap.format.mBytesPerFrame)

guard bytesPerFrame > 0 else {
    writeStdout(Framing.statusEvent(
        level: "error", code: "invalid_format",
        message: "tap reported 0 bytes per frame"))
    logLine("FATAL: tap reported 0 bytes per frame")
    exit(1)
}

// Writer thread: drains the ring buffer the IOProc callback fills and turns
// whatever's available into framed messages on stdout. Kept off the
// real-time audio thread entirely — see RingBuffer.swift and ProcessTap.swift.
let writerThread = Thread {
    // Round down to a whole number of frames so every drain is frame-aligned
    // (every ring-buffer write is already frame-aligned, since Core Audio
    // delivers whole frames per callback — this just keeps `drain` calls
    // aligned too, so `count / bytesPerFrame` below always divides evenly).
    let maxDrainBytes = (64 * 1024 / bytesPerFrame) * bytesPerFrame
    let scratch = UnsafeMutableRawPointer.allocate(
        byteCount: maxDrainBytes, alignment: MemoryLayout<UInt8>.alignment)
    defer { scratch.deallocate() }

    var lastHeartbeat = DispatchTime.now().uptimeNanoseconds
    let heartbeatIntervalNs: UInt64 = 250_000_000

    while isRunning() {
        let drained = tap.ringBuffer.drain(maxBytes: maxDrainBytes, into: scratch) { pointer, count in
            let frameCount = UInt32(count / bytesPerFrame)
            guard frameCount > 0 else { return }
            let timestampNs = DispatchTime.now().uptimeNanoseconds - processStartTime
            let message = Framing.audioFrame(
                timestampNs: timestampNs,
                sampleRate: sampleRate,
                channels: channels,
                frameCount: frameCount,
                samplesBytes: pointer,
                samplesByteCount: count
            )
            writeStdout(message)
        }

        let now = DispatchTime.now().uptimeNanoseconds
        if now - lastHeartbeat >= heartbeatIntervalNs {
            writeStdout(Framing.heartbeat())
            lastHeartbeat = now
        }

        if drained == 0 {
            Thread.sleep(forTimeInterval: 0.002)
        }
    }
}
writerThread.start()

logLine("capturing system audio; send SIGINT/SIGTERM to stop")

// Keep the main thread (and process) alive, checking periodically for the
// stop signal, rather than blocking on something un-interruptible. Modeled
// on the same pattern used by AudioTee for the same reason: there's no
// other blocking call here that both keeps the process alive and reacts to
// a signal-driven shutdown request promptly.
while isRunning() {
    _ = CFRunLoopRunInMode(.defaultMode, 0.1, false)
}

logLine("stopping")
tap.stop()
logLine("stopped cleanly")
