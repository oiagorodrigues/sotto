import AVFoundation
import Foundation
import SottoAudioCore

enum CaptureError: Error, CustomStringConvertible {
    case device(String)
    case permission(String)

    var description: String {
        switch self {
        case let .device(m), let .permission(m): m
        }
    }
}

/// Microphone through `AVAudioEngine` with voice processing (echo cancellation) enabled.
final class MicCapture {
    private let engine = AVAudioEngine()
    private var running = false

    func start(clock: HostClock, processor: StreamProcessor) throws {
        let input = engine.inputNode
        do {
            try input.setVoiceProcessingEnabled(true)
        } catch {
            throw CaptureError.device("could not enable voice processing: \(error)")
        }
        // Enabling voice processing changes the node format, so read it afterwards.
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw CaptureError.device("microphone reports no usable input format")
        }
        input.installTap(onBus: 0, bufferSize: 2048, format: format) { buffer, when in
            guard let copy = copyBuffer(buffer) else { return }
            let ticks = when.isHostTimeValid ? when.hostTime : mach_absolute_time()
            processor.submit(CapturedBuffer(buffer: copy, hostOffsetNs: clock.offsetNs(fromTicks: ticks)))
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw CaptureError.device("could not start the audio engine: \(error)")
        }
        running = true
    }

    func stop() {
        guard running else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        running = false
    }
}
