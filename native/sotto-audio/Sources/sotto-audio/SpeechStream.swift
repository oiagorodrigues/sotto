import AVFoundation
import CoreMedia
import Foundation
import Speech
import SottoAudioCore
import SottoProtocol

enum StreamError: Error, CustomStringConvertible {
    case unsupportedLocale(String)
    case assetsUnavailable(String)
    case noFormat
    case analyzer(String)

    var description: String {
        switch self {
        case let .unsupportedLocale(id): "locale \(id) is not supported by SpeechTranscriber"
        case let .assetsUnavailable(why): "speech assets unavailable: \(why)"
        case .noFormat: "no compatible audio format for the analyzer"
        case let .analyzer(why): "analyzer failed: \(why)"
        }
    }

    var code: SidecarErrorCode {
        switch self {
        case .unsupportedLocale: .unsupportedLocale
        case .assetsUnavailable: .assetsUnavailable
        case .noFormat, .analyzer: .analyzer
        }
    }
}

/// One `SpeechAnalyzer` + `SpeechTranscriber` for one speaker.
final class SpeechStream: @unchecked Sendable {
    let speaker: Speaker
    let format: AVAudioFormat
    private let analyzer: SpeechAnalyzer
    private let continuation: AsyncStream<AnalyzerInput>.Continuation
    private let results: Task<Void, Never>

    private init(
        speaker: Speaker, format: AVAudioFormat, analyzer: SpeechAnalyzer,
        continuation: AsyncStream<AnalyzerInput>.Continuation, results: Task<Void, Never>
    ) {
        self.speaker = speaker
        self.format = format
        self.analyzer = analyzer
        self.continuation = continuation
        self.results = results
    }

    static func resolveLocale(_ identifier: String) async throws -> Locale {
        guard SpeechTranscriber.isAvailable,
              let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: identifier))
        else { throw StreamError.unsupportedLocale(identifier) }
        return locale
    }

    static func make(speaker: Speaker, locale: Locale, writer: EventWriter) async throws -> SpeechStream {
        let transcriber = SpeechTranscriber(locale: locale, preset: .timeIndexedProgressiveTranscription)
        do {
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                try await request.downloadAndInstall()
            }
        } catch {
            throw StreamError.assetsUnavailable("\(error)")
        }
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            throw StreamError.noFormat
        }

        let analyzer = SpeechAnalyzer(modules: [transcriber])
        do {
            try await analyzer.prepareToAnalyze(in: format)
        } catch {
            throw StreamError.analyzer("\(error)")
        }

        let (inputs, continuation) = AsyncStream<AnalyzerInput>.makeStream()
        let results = Task {
            var mapper = SegmentMapper()
            do {
                for try await result in transcriber.results {
                    let raw = RawTranscriptResult(
                        startMs: milliseconds(result.range.start),
                        endMs: milliseconds(result.range.end),
                        text: String(result.text.characters),
                        isFinal: result.isFinal
                    )
                    guard let u = mapper.ingest(raw) else { continue }
                    writer.emit(.transcript(
                        speaker: speaker, segmentID: u.segmentID, revision: u.revision,
                        startMs: u.startMs, endMs: u.endMs, text: u.text, isFinal: u.isFinal
                    ))
                }
            } catch {
                writer.error(.analyzer, "result stream failed: \(error)")
            }
        }

        do {
            try await analyzer.start(inputSequence: inputs)
        } catch {
            results.cancel()
            throw StreamError.analyzer("\(error)")
        }
        return SpeechStream(speaker: speaker, format: format, analyzer: analyzer, continuation: continuation, results: results)
    }

    func feed(_ buffer: AVAudioPCMBuffer, startSample: Int64) {
        let start = CMTime(value: startSample, timescale: CMTimeScale(buffer.format.sampleRate))
        continuation.yield(AnalyzerInput(buffer: buffer, bufferStartTime: start))
    }

    func finalize(throughMs: UInt64) async throws {
        try await analyzer.finalize(through: CMTime(value: CMTimeValue(throughMs), timescale: 1000))
    }

    /// Ending the input stream does not end the session, so the analyzer is finished explicitly.
    func finish() async {
        continuation.finish()
        do {
            try await analyzer.finalizeAndFinishThroughEndOfInput()
        } catch {
            Log.error("finalizeAndFinish failed: \(error)")
            await analyzer.cancelAndFinishNow()
        }
        await results.value
    }
}

private func milliseconds(_ time: CMTime) -> UInt64 {
    guard time.isValid, time.isNumeric else { return 0 }
    return UInt64(max(0, (time.seconds * 1000).rounded()))
}
