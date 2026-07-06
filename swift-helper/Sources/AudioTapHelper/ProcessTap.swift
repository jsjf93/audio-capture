import AudioToolbox
import CoreAudio
import Foundation

/// Sets up and tears down a Core Audio Process Tap capturing all system
/// output, and feeds raw captured bytes into a `RingBuffer` for the writer
/// thread in `main.swift` to drain.
///
/// The construction sequence and property keys here are adapted from the
/// open-source AudioTee project (github.com/makeusabrew/audiotee, MIT
/// licensed), which is real working code against this same (undocumented,
/// easy-to-get-subtly-wrong) API. Two details worth calling out because
/// they contradict what's easy to assume from Apple's own docs:
///
/// 1. The aggregate device is created with an *empty* sub-device list — no
///    real output device is attached as a member. The tap is attached
///    separately afterward via the dedicated `kAudioAggregateDevicePropertyTapList`
///    property, not by putting it in the sub-device list. Trying to make
///    the tap look like a regular sub-device is the mistake that produces
///    silent all-zero output.
/// 2. `AudioDeviceCreateIOProcID` (the plain HAL callback API) is used
///    directly on the aggregate device — not `AVAudioEngine`, which can't
///    be retargeted onto a tap-backed aggregate device at all.
final class ProcessTap {
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var deviceID = AudioObjectID(kAudioObjectUnknown)
    private var ioProcID: AudioDeviceIOProcID?

    private(set) var format = AudioStreamBasicDescription()

    let ringBuffer: RingBuffer

    init(ringBufferCapacityBytes: Int = 1_000_000) {
        self.ringBuffer = RingBuffer(capacityBytes: ringBufferCapacityBytes)
    }

    deinit {
        stop()
    }

    func start() throws {
        tapID = try createTap()
        deviceID = try createAggregateDevice()
        try attachTap(tapID: tapID, toAggregateDevice: deviceID)
        format = try waitForFormat(deviceID: deviceID)
        try startIOProc()
    }

    func stop() {
        if let ioProcID {
            AudioDeviceStop(deviceID, ioProcID)
            AudioDeviceDestroyIOProcID(deviceID, ioProcID)
            self.ioProcID = nil
        }
        if deviceID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyAggregateDevice(deviceID)
            deviceID = AudioObjectID(kAudioObjectUnknown)
        }
        if tapID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
    }

    // MARK: - Setup steps

    private func createTap() throws -> AudioObjectID {
        let description = CATapDescription()
        description.name = "audio-capture-system-tap"
        // Empty process list + isExclusive = true means "exclude nothing",
        // i.e. tap every process. isExclusive is a *direction* flag, not a
        // lock-mode toggle: with a non-empty process list it would mean
        // "exclude only these listed processes"; false would flip the list
        // to mean "include only these listed processes". Getting this
        // backwards silently produces an all-zero-samples stream with no
        // error — it doesn't fail loudly.
        description.processes = []
        description.isExclusive = true
        description.isPrivate = true
        description.muteBehavior = .unmuted
        description.isMixdown = true // request standard interleaved PCM, not a per-app multichannel bundle
        description.isMono = true // simplifies Phase 2 verification; revisit for stereo later
        description.deviceUID = nil // system default output device
        description.stream = 0

        var newTapID = AudioObjectID(kAudioObjectUnknown)
        let status = AudioHardwareCreateProcessTap(description, &newTapID)
        guard status == kAudioHardwareNoError else {
            throw ProcessTapError.tapCreationFailed(status)
        }
        return newTapID
    }

    private func createAggregateDevice() throws -> AudioObjectID {
        let uid = UUID().uuidString
        let description: [String: Any] = [
            kAudioAggregateDeviceNameKey: "audio-capture-tap-aggregate",
            kAudioAggregateDeviceUIDKey: uid,
            kAudioAggregateDeviceSubDeviceListKey: [] as CFArray,
            kAudioAggregateDeviceMasterSubDeviceKey: 0,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
        ]

        var newDeviceID = AudioObjectID(0)
        let status = AudioHardwareCreateAggregateDevice(description as CFDictionary, &newDeviceID)
        guard status == kAudioHardwareNoError else {
            throw ProcessTapError.aggregateDeviceCreationFailed(status)
        }
        return newDeviceID
    }

    private func attachTap(tapID: AudioObjectID, toAggregateDevice deviceID: AudioObjectID) throws {
        var uidAddress = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var tapUID: CFString = "" as CFString
        var uidSize = UInt32(MemoryLayout<CFString>.stride)
        _ = withUnsafeMutablePointer(to: &tapUID) { ptr in
            AudioObjectGetPropertyData(tapID, &uidAddress, 0, nil, &uidSize, ptr)
        }

        var listAddress = AudioObjectPropertyAddress(
            mSelector: kAudioAggregateDevicePropertyTapList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let tapArray = [tapUID] as CFArray
        let listSize = UInt32(MemoryLayout<CFArray>.stride)
        let status = withUnsafePointer(to: tapArray) { ptr in
            AudioObjectSetPropertyData(deviceID, &listAddress, 0, nil, listSize, ptr)
        }
        guard status == kAudioHardwareNoError else {
            throw ProcessTapError.tapAssignmentFailed(status)
        }
    }

    /// The aggregate device isn't necessarily immediately queryable right
    /// after `AudioHardwareCreateAggregateDevice` returns; poll briefly for
    /// it to report itself alive before asking for its stream format, and
    /// retry the format fetch itself a few times too. This is a real
    /// hardware/driver-timing quirk observed in working reference
    /// implementations of this same API, not defensive superstition.
    private func waitForFormat(deviceID: AudioObjectID) throws -> AudioStreamBasicDescription {
        for _ in 0..<20 {
            if isDeviceAlive(deviceID) { break }
            Thread.sleep(forTimeInterval: 0.1)
        }

        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioDevicePropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.stride)
        var streamFormat = AudioStreamBasicDescription()

        for attempt in 0..<5 {
            let status = AudioObjectGetPropertyData(deviceID, &address, 0, nil, &size, &streamFormat)
            if status == noErr {
                return streamFormat
            }
            if attempt < 4 {
                Thread.sleep(forTimeInterval: 0.02)
            }
        }
        throw ProcessTapError.deviceFormatUnavailable
    }

    private func isDeviceAlive(_ deviceID: AudioObjectID) -> Bool {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceIsAlive,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var isAlive: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        let status = AudioObjectGetPropertyData(deviceID, &address, 0, nil, &size, &isAlive)
        return status == kAudioHardwareNoError && isAlive == 1
    }

    private func startIOProc() throws {
        let selfPointer = Unmanaged.passUnretained(self).toOpaque()
        var newIOProcID: AudioDeviceIOProcID?

        let status = AudioDeviceCreateIOProcID(
            deviceID,
            { (_, _, inInputData, _, _, _, clientData) -> OSStatus in
                let tap = Unmanaged<ProcessTap>.fromOpaque(clientData!).takeUnretainedValue()
                tap.handleAudio(inInputData)
                return noErr
            },
            selfPointer,
            &newIOProcID
        )
        guard status == noErr, let newIOProcID else {
            throw ProcessTapError.ioProcCreationFailed(status)
        }
        ioProcID = newIOProcID

        let startStatus = AudioDeviceStart(deviceID, newIOProcID)
        guard startStatus == noErr else {
            throw ProcessTapError.deviceStartFailed(startStatus)
        }
    }

    /// Runs on Core Audio's real-time IO thread: copy into the ring buffer
    /// and return immediately. No allocation, no logging, no I/O here.
    private func handleAudio(_ inputData: UnsafePointer<AudioBufferList>) {
        let bufferList = inputData.pointee
        let buffer = bufferList.mBuffers
        guard let data = buffer.mData, buffer.mDataByteSize > 0 else { return }
        ringBuffer.write(from: data, count: Int(buffer.mDataByteSize))
    }
}

enum ProcessTapError: Error, CustomStringConvertible {
    case tapCreationFailed(OSStatus)
    case aggregateDeviceCreationFailed(OSStatus)
    case tapAssignmentFailed(OSStatus)
    case deviceFormatUnavailable
    case ioProcCreationFailed(OSStatus)
    case deviceStartFailed(OSStatus)

    var description: String {
        switch self {
        case .tapCreationFailed(let status):
            return "AudioHardwareCreateProcessTap failed with OSStatus \(status)"
        case .aggregateDeviceCreationFailed(let status):
            return "AudioHardwareCreateAggregateDevice failed with OSStatus \(status)"
        case .tapAssignmentFailed(let status):
            return "attaching tap to aggregate device failed with OSStatus \(status)"
        case .deviceFormatUnavailable:
            return "could not read stream format from the aggregate device"
        case .ioProcCreationFailed(let status):
            return "AudioDeviceCreateIOProcID failed with OSStatus \(status)"
        case .deviceStartFailed(let status):
            return "AudioDeviceStart failed with OSStatus \(status)"
        }
    }

    /// A short machine-readable identifier for the StatusEvent `code` field.
    var code: String {
        switch self {
        case .tapCreationFailed: return "tap_create_failed"
        case .aggregateDeviceCreationFailed: return "aggregate_device_create_failed"
        case .tapAssignmentFailed: return "tap_assignment_failed"
        case .deviceFormatUnavailable: return "device_format_unavailable"
        case .ioProcCreationFailed: return "ioproc_create_failed"
        case .deviceStartFailed: return "device_start_failed"
        }
    }
}
