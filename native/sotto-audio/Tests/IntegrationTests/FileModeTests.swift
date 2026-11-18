import Foundation
import SottoProtocol
import Testing

private let packageRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

private func executableURL() -> URL? {
    for bundle in Bundle.allBundles where bundle.bundlePath.hasSuffix(".xctest") {
        let url = URL(fileURLWithPath: bundle.bundlePath).deletingLastPathComponent().appendingPathComponent("sotto-audio")
        if FileManager.default.isExecutableFile(atPath: url.path) { return url }
    }
    let fallback = packageRoot.appendingPathComponent(".build/debug/sotto-audio")
    return FileManager.default.isExecutableFile(atPath: fallback.path) ? fallback : nil
}

private func run(_ exe: URL, arguments: [String]) throws -> (status: Int32, events: [SidecarEvent]) {
    let process = Process()
    process.executableURL = exe
    process.arguments = arguments
    let out = Pipe()
    process.standardOutput = out
    process.standardError = FileHandle.nullDevice
    try process.run()
    let data = out.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    let events = try data.split(separator: 0x0A).map { try SidecarJSON.decodeEvent(Data($0)) }
    return (process.terminationStatus, events)
}

@Suite struct FileModeTests {
    @Test func ptBRQuestionProducesPartialsFinalAndStopped() async throws {
        guard let exe = executableURL() else {
            Issue.record("sotto-audio executable not found; run `swift build` first")
            return
        }
        let wav = packageRoot.appendingPathComponent("../../fixtures/audio/pt-BR-question.wav").standardized
        let (status, events) = try run(exe, arguments: ["--file", wav.path, "--speaker", "them", "--locale", "pt-BR"])

        guard case let .ready(_, _, locales)? = events.first else {
            Issue.record("first event is not ready")
            return
        }
        guard locales.contains("pt-BR") else {
            print("SKIPPED: SpeechTranscriber has no installed pt-BR model on this machine (installed: \(locales))")
            return
        }
        #expect(status == 0)

        var partials = 0
        var finals = 0
        var lastSegmentRevision: [UInt64: UInt32] = [:]
        for case let .transcript(speaker, id, revision, start, end, _, isFinal) in events {
            #expect(speaker == .them)
            #expect(end >= start)
            #expect(revision > (lastSegmentRevision[id] ?? 0))
            lastSegmentRevision[id] = revision
            if isFinal { finals += 1 } else { partials += 1 }
        }
        #expect(partials >= 1)
        #expect(finals >= 1)
        #expect(events.contains { if case .started = $0 { true } else { false } })
        #expect(events.last == .stopped)
    }
}
