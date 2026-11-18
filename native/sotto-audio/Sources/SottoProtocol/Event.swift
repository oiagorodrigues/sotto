import Foundation

/// Sidecar to core. Mirrors `SidecarEvent` in `crates/sotto-protocol/src/sidecar.rs`.
public enum SidecarEvent: Codable, Sendable, Equatable {
    case ready(protocolVersion: UInt32, sidecarVersion: String, installedLocales: [String])
    case started(t0HostNs: UInt64, emittedAtMs: UInt64)
    case transcript(
        speaker: Speaker, segmentID: UInt64, revision: UInt32,
        startMs: UInt64, endMs: UInt64, text: String, isFinal: Bool
    )
    case level(speaker: Speaker, dbfs: Float)
    case screenshot(requestID: UUID, pngBase64: String, widthPx: UInt32, heightPx: UInt32)
    case permissions([PermissionState])
    case overflow(speaker: Speaker, droppedBuffers: UInt64)
    case error(code: SidecarErrorCode, message: String, fatal: Bool)
    case stopped

    private enum CodingKeys: String, CodingKey {
        case type, speaker, text, revision, dbfs, states, code, message, fatal
        case protocolVersion = "protocol_version"
        case sidecarVersion = "sidecar_version"
        case installedLocales = "installed_locales"
        case t0HostNs = "t0_host_ns"
        case emittedAtMs = "emitted_at_ms"
        case segmentID = "segment_id"
        case startMs = "start_ms"
        case endMs = "end_ms"
        case isFinal = "is_final"
        case requestID = "request_id"
        case pngBase64 = "png_base64"
        case widthPx = "width_px"
        case heightPx = "height_px"
        case droppedBuffers = "dropped_buffers"
    }

    private enum Tag: String {
        case ready, started, transcript, level, screenshot, permissions, overflow, error, stopped
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let raw = try c.decode(String.self, forKey: .type)
        guard let tag = Tag(rawValue: raw) else {
            throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "unknown event \"\(raw)\"")
        }
        switch tag {
        case .ready:
            self = .ready(
                protocolVersion: try c.decode(UInt32.self, forKey: .protocolVersion),
                sidecarVersion: try c.decode(String.self, forKey: .sidecarVersion),
                installedLocales: try c.decode([String].self, forKey: .installedLocales)
            )
        case .started:
            self = .started(
                t0HostNs: try c.decode(UInt64.self, forKey: .t0HostNs),
                emittedAtMs: try c.decode(UInt64.self, forKey: .emittedAtMs)
            )
        case .transcript:
            self = .transcript(
                speaker: try c.decode(Speaker.self, forKey: .speaker),
                segmentID: try c.decode(UInt64.self, forKey: .segmentID),
                revision: try c.decode(UInt32.self, forKey: .revision),
                startMs: try c.decode(UInt64.self, forKey: .startMs),
                endMs: try c.decode(UInt64.self, forKey: .endMs),
                text: try c.decode(String.self, forKey: .text),
                isFinal: try c.decode(Bool.self, forKey: .isFinal)
            )
        case .level:
            self = .level(
                speaker: try c.decode(Speaker.self, forKey: .speaker),
                dbfs: try c.decode(Float.self, forKey: .dbfs)
            )
        case .screenshot:
            self = .screenshot(
                requestID: try decodeUUID(c, .requestID),
                pngBase64: try c.decode(String.self, forKey: .pngBase64),
                widthPx: try c.decode(UInt32.self, forKey: .widthPx),
                heightPx: try c.decode(UInt32.self, forKey: .heightPx)
            )
        case .permissions: self = .permissions(try c.decode([PermissionState].self, forKey: .states))
        case .overflow:
            self = .overflow(
                speaker: try c.decode(Speaker.self, forKey: .speaker),
                droppedBuffers: try c.decode(UInt64.self, forKey: .droppedBuffers)
            )
        case .error:
            self = .error(
                code: try c.decode(SidecarErrorCode.self, forKey: .code),
                message: try c.decode(String.self, forKey: .message),
                fatal: try c.decode(Bool.self, forKey: .fatal)
            )
        case .stopped: self = .stopped
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .ready(version, sidecar, locales):
            try c.encode(Tag.ready.rawValue, forKey: .type)
            try c.encode(version, forKey: .protocolVersion)
            try c.encode(sidecar, forKey: .sidecarVersion)
            try c.encode(locales, forKey: .installedLocales)
        case let .started(t0, emittedAtMs):
            try c.encode(Tag.started.rawValue, forKey: .type)
            try c.encode(t0, forKey: .t0HostNs)
            try c.encode(emittedAtMs, forKey: .emittedAtMs)
        case let .transcript(speaker, id, revision, start, end, text, isFinal):
            try c.encode(Tag.transcript.rawValue, forKey: .type)
            try c.encode(speaker, forKey: .speaker)
            try c.encode(id, forKey: .segmentID)
            try c.encode(revision, forKey: .revision)
            try c.encode(start, forKey: .startMs)
            try c.encode(end, forKey: .endMs)
            try c.encode(text, forKey: .text)
            try c.encode(isFinal, forKey: .isFinal)
        case let .level(speaker, dbfs):
            try c.encode(Tag.level.rawValue, forKey: .type)
            try c.encode(speaker, forKey: .speaker)
            try c.encode(dbfs, forKey: .dbfs)
        case let .screenshot(id, png, w, h):
            try c.encode(Tag.screenshot.rawValue, forKey: .type)
            try c.encode(id.uuidString.lowercased(), forKey: .requestID)
            try c.encode(png, forKey: .pngBase64)
            try c.encode(w, forKey: .widthPx)
            try c.encode(h, forKey: .heightPx)
        case let .permissions(states):
            try c.encode(Tag.permissions.rawValue, forKey: .type)
            try c.encode(states, forKey: .states)
        case let .overflow(speaker, dropped):
            try c.encode(Tag.overflow.rawValue, forKey: .type)
            try c.encode(speaker, forKey: .speaker)
            try c.encode(dropped, forKey: .droppedBuffers)
        case let .error(code, message, fatal):
            try c.encode(Tag.error.rawValue, forKey: .type)
            try c.encode(code, forKey: .code)
            try c.encode(message, forKey: .message)
            try c.encode(fatal, forKey: .fatal)
        case .stopped: try c.encode(Tag.stopped.rawValue, forKey: .type)
        }
    }
}

extension SidecarEvent {
    /// Every failure while handling `start` is fatal for the session, though the sidecar stays alive.
    public static func startFailure(_ code: SidecarErrorCode, _ message: String) -> SidecarEvent {
        .error(code: code, message: message, fatal: true)
    }
}

public enum SidecarJSON {
    public static func encodeLine(_ event: SidecarEvent) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        return try encoder.encode(event) + Data([0x0A])
    }

    public static func decodeCommand(_ line: Data) throws -> SidecarCommand {
        try JSONDecoder().decode(SidecarCommand.self, from: line)
    }

    public static func decodeEvent(_ line: Data) throws -> SidecarEvent {
        try JSONDecoder().decode(SidecarEvent.self, from: line)
    }
}
