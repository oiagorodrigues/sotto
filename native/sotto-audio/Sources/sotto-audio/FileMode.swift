import AVFoundation
import Foundation
import SottoAudioCore
import SottoProtocol

struct FileModeOptions {
    var path: String
    var speaker: Speaker
    var locale: String
    var realtime: Bool
}

/// Feeds a WAV through the same analyzer path as live capture, for tests and onboarding benchmarks.
func runFileMode(_ options: FileModeOptions, writer: EventWriter) async -> Never {
    writer.emit(await readyEvent())
    do {
        let locale = try await SpeechStream.resolveLocale(options.locale)
        let speech = try await SpeechStream.make(speaker: options.speaker, locale: locale, writer: writer)
        let processor = StreamProcessor(speaker: options.speaker, target: speech.format, writer: writer) { buffer, start in
            speech.feed(buffer, startSample: start)
        }
        let file = try AVAudioFile(forReading: URL(fileURLWithPath: options.path))
        let clock = HostClock.now()
        writer.emit(.started(t0HostNs: clock.t0Ns, emittedAtMs: clock.elapsedMsNow()))

        let rate = file.processingFormat.sampleRate
        let chunk = AVAudioFrameCount(rate * 0.02)
        let begin = ContinuousClock.now
        var position: Int64 = 0
        while position < file.length {
            guard let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: chunk) else { break }
            try file.read(into: buffer, frameCount: chunk)
            if buffer.frameLength == 0 { break }
            let offsetNs = Int64(Double(position) / rate * 1e9)
            processor.submitBlocking(CapturedBuffer(buffer: buffer, hostOffsetNs: offsetNs))
            position += Int64(buffer.frameLength)
            if options.realtime {
                let due = begin.advanced(by: .nanoseconds(Int64(Double(position) / rate * 1e9)))
                try await Task.sleep(until: due, clock: .continuous)
            }
        }
        processor.drain()
        processor.stopMonitoring()
        await speech.finish()
        writer.emit(.stopped)
        exit(0)
    } catch let error as StreamError {
        writer.fatal(error.code, error.description)
    } catch {
        writer.fatal(.internal, "\(error)")
    }
}
