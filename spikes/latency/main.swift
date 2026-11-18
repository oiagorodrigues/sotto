// Throwaway spike (design.md, slice 1 step 0). Feeds a WAV file to SpeechTranscriber at real-time
// pace and prints every result with how far it lags behind the audio that was already fed.
import AVFoundation
import Foundation
import Speech

struct Event: Encodable {
    let wallMs: Int
    let fedMs: Int
    let rangeStartMs: Int
    let rangeEndMs: Int
    let lagMs: Int
    let isFinal: Bool
    let text: String
}

final class FeedClock: @unchecked Sendable {
    private let lock = NSLock()
    private var fedAudioMs = 0
    func set(_ ms: Int) { lock.lock(); fedAudioMs = ms; lock.unlock() }
    func get() -> Int { lock.lock(); defer { lock.unlock() }; return fedAudioMs }
}

func ms(_ t: CMTime) -> Int { t.isValid ? Int((t.seconds * 1000).rounded()) : -1 }

@main
struct Spike {
    static func main() async throws {
        let args = CommandLine.arguments
        guard args.count >= 3 else {
            FileHandle.standardError.write(Data("usage: spike <file.wav> <locale> [chunkMs]\n".utf8))
            exit(64)
        }
        let chunkMs = args.count > 3 ? Int(args[3]) ?? 20 : 20

        guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: args[2])) else {
            print("unsupported locale \(args[2])"); exit(2)
        }
        let transcriber = SpeechTranscriber(locale: locale, preset: .timeIndexedProgressiveTranscription)
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
            try await request.downloadAndInstall()
        }
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            print("no compatible format"); exit(3)
        }

        let file = try AVAudioFile(forReading: URL(fileURLWithPath: args[1]), commonFormat: .pcmFormatInt16, interleaved: true)
        guard file.processingFormat.sampleRate == format.sampleRate, file.processingFormat.channelCount == format.channelCount else {
            print("file must be \(format.sampleRate) Hz, \(format.channelCount) ch; got \(file.processingFormat)"); exit(4)
        }

        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let prepStart = ContinuousClock.now
        try await analyzer.prepareToAnalyze(in: format)
        let prepMs = prepStart.duration(to: .now).components.attoseconds / 1_000_000_000_000_000
            + prepStart.duration(to: .now).components.seconds * 1000

        let (stream, continuation) = AsyncStream<AnalyzerInput>.makeStream()
        let feed = FeedClock()
        let t0 = ContinuousClock.now
        let encoder = JSONEncoder()

        let results = Task {
            var events: [Event] = []
            for try await r in transcriber.results {
                let d = t0.duration(to: .now)
                let wall = Int(d.components.seconds * 1000 + d.components.attoseconds / 1_000_000_000_000_000)
                let fed = feed.get()
                let e = Event(wallMs: wall, fedMs: fed, rangeStartMs: ms(r.range.start), rangeEndMs: ms(r.range.end),
                              lagMs: fed - ms(r.range.end), isFinal: r.isFinal, text: String(r.text.characters))
                events.append(e)
                print(String(data: try encoder.encode(e), encoding: .utf8)!)
            }
            return events
        }

        try await analyzer.start(inputSequence: stream)

        let framesPerChunk = AVAudioFrameCount(format.sampleRate * Double(chunkMs) / 1000)
        var position: AVAudioFramePosition = 0
        let feedStart = ContinuousClock.now
        while position < file.length {
            guard let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: framesPerChunk) else { break }
            try file.read(into: buffer, frameCount: framesPerChunk)
            if buffer.frameLength == 0 { break }
            let start = CMTime(value: position, timescale: CMTimeScale(format.sampleRate))
            continuation.yield(AnalyzerInput(buffer: buffer, bufferStartTime: start))
            position += AVAudioFramePosition(buffer.frameLength)
            feed.set(Int(Double(position) / format.sampleRate * 1000))
            let due = feedStart.advanced(by: .milliseconds(Int(Double(position) / format.sampleRate * 1000)))
            try await Task.sleep(until: due, clock: .continuous)
        }
        let audioEndMs = feed.get()
        continuation.finish()
        try await analyzer.finalizeAndFinishThroughEndOfInput()
        let events = try await results.value

        let d = t0.duration(to: .now)
        let doneMs = Int(d.components.seconds * 1000 + d.components.attoseconds / 1_000_000_000_000_000)
        let firstVolatile = events.first { !$0.isFinal && !$0.text.isEmpty }
        let volatileLags = events.filter { !$0.isFinal && !$0.text.isEmpty }.map(\.lagMs).sorted()
        let lastFinal = events.last { $0.isFinal }
        func pct(_ a: [Int], _ p: Double) -> Int { a.isEmpty ? -1 : a[min(a.count - 1, Int(Double(a.count) * p))] }
        print("""
        SUMMARY locale=\(args[2]) chunkMs=\(chunkMs) prepareMs=\(prepMs) audioMs=\(audioEndMs)
          firstPartialAtWallMs=\(firstVolatile?.wallMs ?? -1) (speech starts near 0)
          volatileLag p50=\(pct(volatileLags, 0.5)) p95=\(pct(volatileLags, 0.95)) n=\(volatileLags.count)
          lastFinalAtWallMs=\(lastFinal?.wallMs ?? -1) endOfAudioToLastFinalMs=\((lastFinal?.wallMs ?? -1) - audioEndMs) doneMs=\(doneMs)
          finalText=\(events.filter(\.isFinal).map(\.text).joined())
        """)
    }
}
