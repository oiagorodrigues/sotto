import AVFoundation
import CoreAudio
import Foundation
import SottoAudioCore

/// System audio through a Core Audio process tap that excludes the given processes, read
/// through a private aggregate device and an IOProc. macOS asks for system audio consent the
/// first time a tap is created (`NSAudioCaptureUsageDescription`).
final class SystemAudioCapture {
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var procID: AudioDeviceIOProcID?
    private let ioQueue = DispatchQueue(label: "sotto.audio.tap-io", qos: .userInteractive)

    func start(excluding pids: [pid_t], clock: HostClock, processor: StreamProcessor) throws {
        let excluded = pids.compactMap { Self.processObject(for: $0) }
        let description = CATapDescription(stereoGlobalTapButExcludeProcesses: excluded)
        description.uuid = UUID()
        description.isPrivate = true
        description.muteBehavior = .unmuted

        let status = AudioHardwareCreateProcessTap(description, &tapID)
        guard status == noErr else { throw CaptureError.permission("could not create the process tap (OSStatus \(status))") }

        do {
            try createAggregate(tapUUID: description.uuid)
            let format = try tapFormat()
            try startIO(format: format, clock: clock, processor: processor)
        } catch {
            stop()
            throw error
        }
    }

    /// Creates and destroys a tap so macOS shows its consent prompt. Returns whether it worked.
    static func probe() -> Bool {
        let description = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
        description.isPrivate = true
        var id = AudioObjectID(kAudioObjectUnknown)
        guard AudioHardwareCreateProcessTap(description, &id) == noErr else { return false }
        AudioHardwareDestroyProcessTap(id)
        return true
    }

    func stop() {
        if let procID, aggregateID != kAudioObjectUnknown {
            AudioDeviceStop(aggregateID, procID)
            AudioDeviceDestroyIOProcID(aggregateID, procID)
        }
        procID = nil
        if aggregateID != kAudioObjectUnknown {
            AudioHardwareDestroyAggregateDevice(aggregateID)
            aggregateID = AudioObjectID(kAudioObjectUnknown)
        }
        if tapID != kAudioObjectUnknown {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
    }

    private func createAggregate(tapUUID: UUID) throws {
        let outputUID = try Self.defaultOutputDeviceUID()
        let properties: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Sotto system audio",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceMainSubDeviceKey: outputUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: outputUID]],
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: true,
                kAudioSubTapUIDKey: tapUUID.uuidString,
            ]],
        ]
        let status = AudioHardwareCreateAggregateDevice(properties as CFDictionary, &aggregateID)
        guard status == noErr else { throw CaptureError.device("could not create the aggregate device (OSStatus \(status))") }
    }

    private func tapFormat() throws -> AVAudioFormat {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var asbd = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        let status = AudioObjectGetPropertyData(tapID, &address, 0, nil, &size, &asbd)
        guard status == noErr, let format = AVAudioFormat(streamDescription: &asbd) else {
            throw CaptureError.device("could not read the tap format (OSStatus \(status))")
        }
        return format
    }

    private func startIO(format: AVAudioFormat, clock: HostClock, processor: StreamProcessor) throws {
        var status = AudioDeviceCreateIOProcIDWithBlock(&procID, aggregateID, ioQueue) { _, input, inputTime, _, _ in
            guard let source = AVAudioPCMBuffer(pcmFormat: format, bufferListNoCopy: input, deallocator: nil),
                  source.frameLength > 0,
                  let copy = copyBuffer(source)
            else { return }
            let ticks = inputTime.pointee.mHostTime
            processor.submit(CapturedBuffer(buffer: copy, hostOffsetNs: clock.offsetNs(fromTicks: ticks)))
        }
        guard status == noErr, let procID else { throw CaptureError.device("could not create the IOProc (OSStatus \(status))") }
        status = AudioDeviceStart(aggregateID, procID)
        guard status == noErr else { throw CaptureError.device("could not start the aggregate device (OSStatus \(status))") }
    }

    private static func processObject(for pid: pid_t) -> AudioObjectID? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var pidValue = pid
        var object = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let status = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address,
            UInt32(MemoryLayout<pid_t>.size), &pidValue, &size, &object
        )
        return status == noErr && object != kAudioObjectUnknown ? object : nil
    }

    private static func defaultOutputDeviceUID() throws -> String {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultSystemOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var device = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        var status = AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &device)
        guard status == noErr, device != kAudioObjectUnknown else {
            throw CaptureError.device("no default output device (OSStatus \(status))")
        }
        address.mSelector = kAudioDevicePropertyDeviceUID
        var uid: Unmanaged<CFString>?
        size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        status = AudioObjectGetPropertyData(device, &address, 0, nil, &size, &uid)
        guard status == noErr, let uid else { throw CaptureError.device("could not read the output device UID (OSStatus \(status))") }
        return uid.takeRetainedValue() as String
    }
}
