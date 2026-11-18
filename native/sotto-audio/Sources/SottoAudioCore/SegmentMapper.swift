import Foundation

/// One result from `SpeechTranscriber`, already converted to milliseconds on the shared timeline.
public struct RawTranscriptResult: Sendable, Equatable {
    public var startMs: UInt64
    public var endMs: UInt64
    public var text: String
    public var isFinal: Bool

    public init(startMs: UInt64, endMs: UInt64, text: String, isFinal: Bool) {
        self.startMs = startMs
        self.endMs = endMs
        self.text = text
        self.isFinal = isFinal
    }
}

public struct SegmentUpdate: Sendable, Equatable {
    public var segmentID: UInt64
    public var revision: UInt32
    public var startMs: UInt64
    public var endMs: UInt64
    public var text: String
    public var isFinal: Bool
}

/// Gives volatile results a stable segment id. Successive volatile results revise the open
/// segment; a final result closes it, and the next result starts a new segment. Empty text
/// revokes the open segment and also closes it, so a consumer never has to resurrect an id.
public struct SegmentMapper: Sendable {
    private struct Open {
        var id: UInt64
        var revision: UInt32
    }

    private var nextID: UInt64 = 1
    private var open: Open?
    private var finalizedEndMs: UInt64 = 0

    public init() {}

    public mutating func ingest(_ result: RawTranscriptResult) -> SegmentUpdate? {
        let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let end = max(result.endMs, result.startMs)

        if text.isEmpty {
            guard let current = open else { return nil }
            open = nil
            if result.isFinal { finalizedEndMs = max(finalizedEndMs, end) }
            return SegmentUpdate(
                segmentID: current.id, revision: current.revision + 1,
                startMs: result.startMs, endMs: end, text: "", isFinal: result.isFinal
            )
        }

        if open == nil, end <= finalizedEndMs {
            return nil
        }

        var current = open ?? {
            defer { nextID += 1 }
            return Open(id: nextID, revision: 0)
        }()
        current.revision += 1

        if result.isFinal {
            open = nil
            finalizedEndMs = max(finalizedEndMs, end)
        } else {
            open = current
        }
        return SegmentUpdate(
            segmentID: current.id, revision: current.revision,
            startMs: result.startMs, endMs: end, text: text, isFinal: result.isFinal
        )
    }
}
