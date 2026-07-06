import Foundation

/// A single-producer/single-consumer byte ring buffer.
///
/// The producer is Core Audio's real-time IOProc callback thread: `write`
/// must never allocate or block, so on overflow it simply drops the
/// incoming bytes rather than growing or waiting. The consumer is an
/// ordinary background thread that drains whatever is available and turns
/// it into framed messages on stdout — see `main.swift`.
///
/// This mirrors the same "never block the real-time callback on I/O"
/// discipline used on the Rust side of this project (there, `rtrb`). Note
/// this isn't fully wait-free the way `rtrb` is: it takes an `NSLock`
/// around the bookkeeping, so in theory the audio callback could briefly
/// block if the writer thread is mid-drain. That's a conscious tradeoff,
/// not an oversight — the critical section is only ever pointer arithmetic
/// plus a bounded memcpy (microseconds), never a blocking syscall, so the
/// worst case is a tiny bounded delay, not the unbounded stall a direct
/// `write()` to a pipe from the real-time thread could cause if the parent
/// process is briefly slow to read (which is exactly what this ring buffer
/// exists to avoid). Reference implementations of this same tap API (e.g.
/// AudioTee) skip this entirely and write to the pipe straight from the
/// IOProc callback; splitting it out like this is a deliberate refinement,
/// not something proven necessary yet — worth revisiting if profiling ever
/// shows contention here. Raw pointers are used instead of a Swift `Array`
/// to avoid copy-on-write reference-count checks on every write.
final class RingBuffer {
    private let storage: UnsafeMutableRawPointer
    private let capacity: Int
    private var writeIndex = 0
    private var readIndex = 0
    private var available = 0
    private let lock = NSLock()

    init(capacityBytes: Int) {
        self.capacity = capacityBytes
        self.storage = UnsafeMutableRawPointer.allocate(
            byteCount: capacityBytes,
            alignment: MemoryLayout<UInt8>.alignment
        )
    }

    deinit {
        storage.deallocate()
    }

    /// Called from the real-time IOProc callback. Drops the write on
    /// overflow instead of blocking or growing — an overflow here means the
    /// drain loop on the consumer side is stalled, which would be visible
    /// as a gap in the audio stream, not a crash in the callback.
    func write(from source: UnsafeRawPointer, count: Int) {
        lock.lock()
        defer { lock.unlock() }

        guard available + count <= capacity else {
            return // overflow: drop this write
        }

        if writeIndex + count <= capacity {
            storage.advanced(by: writeIndex).copyMemory(from: source, byteCount: count)
        } else {
            let firstPart = capacity - writeIndex
            let secondPart = count - firstPart
            storage.advanced(by: writeIndex).copyMemory(from: source, byteCount: firstPart)
            storage.copyMemory(from: source.advanced(by: firstPart), byteCount: secondPart)
        }
        writeIndex = (writeIndex + count) % capacity
        available += count
    }

    /// Drains up to `maxBytes` currently available bytes and hands them to
    /// `body` as a single contiguous pointer (linearizing across the
    /// wrap-around boundary into `scratch` if necessary). Returns the
    /// number of bytes drained (0 if nothing was available).
    ///
    /// Called from the ordinary (non-real-time) writer thread, so taking a
    /// lock here is fine — the real-time thread only ever holds it for a
    /// bounded memcpy, never while blocked on I/O.
    func drain(maxBytes: Int, into scratch: UnsafeMutableRawPointer, body: (UnsafeRawPointer, Int) -> Void) -> Int {
        lock.lock()
        let count = min(available, maxBytes)
        guard count > 0 else {
            lock.unlock()
            return 0
        }

        if readIndex + count <= capacity {
            let ptr = storage.advanced(by: readIndex)
            readIndex = (readIndex + count) % capacity
            available -= count
            lock.unlock()
            body(ptr, count)
        } else {
            let firstPart = capacity - readIndex
            let secondPart = count - firstPart
            scratch.copyMemory(from: storage.advanced(by: readIndex), byteCount: firstPart)
            scratch.advanced(by: firstPart).copyMemory(from: storage, byteCount: secondPart)
            readIndex = secondPart
            available -= count
            lock.unlock()
            body(scratch, count)
        }
        return count
    }
}
