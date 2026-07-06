import Foundation

/// Swift-side encoder for the wire protocol documented in
/// `docs/audio-tap-protocol.md`. See that file for the byte-level spec;
/// this is one of the two independent implementations of it (the other is
/// `crates/audio-core/src/system_audio/protocol.rs`).
enum Framing {
    private static let magic: [UInt8] = Array("ATAP".utf8)
    private static let version: UInt8 = 1

    private enum MessageType: UInt8 {
        case audio = 0
        case statusEvent = 1
        case heartbeat = 2
    }

    /// `samplesBytes` must already be interleaved `f32` little-endian PCM —
    /// callers pass through raw bytes from Core Audio without reinterpreting
    /// them, since Apple Silicon is little-endian and so is this format.
    static func audioFrame(
        timestampNs: UInt64,
        sampleRate: UInt32,
        channels: UInt8,
        frameCount: UInt32,
        samplesBytes: UnsafeRawPointer,
        samplesByteCount: Int
    ) -> Data {
        var payload = Data(capacity: 18 + samplesByteCount)
        payload.append(contentsOf: withUnsafeBytes(of: timestampNs.littleEndian) { Array($0) })
        payload.append(contentsOf: withUnsafeBytes(of: sampleRate.littleEndian) { Array($0) })
        payload.append(channels)
        payload.append(0) // format = 0 (interleaved f32 LE)
        payload.append(contentsOf: withUnsafeBytes(of: frameCount.littleEndian) { Array($0) })
        payload.append(Data(bytes: samplesBytes, count: samplesByteCount))
        return frame(type: .audio, payload: payload)
    }

    static func statusEvent(level: String, code: String, message: String) -> Data {
        let json: [String: String] = ["level": level, "code": code, "message": message]
        let payload = (try? JSONSerialization.data(withJSONObject: json)) ?? Data()
        return frame(type: .statusEvent, payload: payload)
    }

    static func heartbeat() -> Data {
        frame(type: .heartbeat, payload: Data())
    }

    private static func frame(type: MessageType, payload: Data) -> Data {
        var header = Data(capacity: 12)
        header.append(contentsOf: magic)
        header.append(version)
        header.append(type.rawValue)
        header.append(contentsOf: [0, 0] as [UInt8]) // flags, reserved
        let len = UInt32(payload.count).littleEndian
        header.append(contentsOf: withUnsafeBytes(of: len) { Array($0) })
        return header + payload
    }
}
