import Foundation

/// Core to sidecar. Mirrors `SidecarCommand` in `crates/sotto-protocol/src/sidecar.rs`.
public enum SidecarCommand: Codable, Sendable, Equatable {
    case start(locale: String, engine: EngineKind, speakers: [Speaker])
    case stop
    case finalize(speaker: Speaker, throughMs: UInt64)
    case captureScreenshot(requestID: UUID, maxLongSidePx: UInt32)
    case checkPermissions
    case requestPermission(PermissionKind)
    case shutdown

    private enum CodingKeys: String, CodingKey {
        case type, locale, engine, speakers, speaker
        case throughMs = "through_ms"
        case requestID = "request_id"
        case maxLongSidePx = "max_long_side_px"
        case permission
    }

    private enum Tag: String {
        case start, stop, finalize
        case captureScreenshot = "capture_screenshot"
        case checkPermissions = "check_permissions"
        case requestPermission = "request_permission"
        case shutdown
    }

    private static func allowedKeys(for tag: Tag) -> [String] {
        switch tag {
        case .start: ["type", "locale", "engine", "speakers"]
        case .stop, .checkPermissions, .shutdown: ["type"]
        case .finalize: ["type", "speaker", "through_ms"]
        case .captureScreenshot: ["type", "request_id", "max_long_side_px"]
        case .requestPermission: ["type", "permission"]
        }
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let raw = try c.decode(String.self, forKey: .type)
        guard let tag = Tag(rawValue: raw) else {
            throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "unknown command \"\(raw)\"")
        }
        try rejectUnknownKeys(decoder, allowed: Self.allowedKeys(for: tag))
        switch tag {
        case .start:
            self = .start(
                locale: try c.decode(String.self, forKey: .locale),
                engine: try c.decode(EngineKind.self, forKey: .engine),
                speakers: try c.decode([Speaker].self, forKey: .speakers)
            )
        case .stop: self = .stop
        case .finalize:
            self = .finalize(
                speaker: try c.decode(Speaker.self, forKey: .speaker),
                throughMs: try c.decode(UInt64.self, forKey: .throughMs)
            )
        case .captureScreenshot:
            self = .captureScreenshot(
                requestID: try decodeUUID(c, .requestID),
                maxLongSidePx: try c.decode(UInt32.self, forKey: .maxLongSidePx)
            )
        case .checkPermissions: self = .checkPermissions
        case .requestPermission:
            self = .requestPermission(try c.decode(PermissionKind.self, forKey: .permission))
        case .shutdown: self = .shutdown
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .start(locale, engine, speakers):
            try c.encode(Tag.start.rawValue, forKey: .type)
            try c.encode(locale, forKey: .locale)
            try c.encode(engine, forKey: .engine)
            try c.encode(speakers, forKey: .speakers)
        case .stop: try c.encode(Tag.stop.rawValue, forKey: .type)
        case let .finalize(speaker, throughMs):
            try c.encode(Tag.finalize.rawValue, forKey: .type)
            try c.encode(speaker, forKey: .speaker)
            try c.encode(throughMs, forKey: .throughMs)
        case let .captureScreenshot(requestID, maxLongSidePx):
            try c.encode(Tag.captureScreenshot.rawValue, forKey: .type)
            try c.encode(requestID.uuidString.lowercased(), forKey: .requestID)
            try c.encode(maxLongSidePx, forKey: .maxLongSidePx)
        case .checkPermissions: try c.encode(Tag.checkPermissions.rawValue, forKey: .type)
        case let .requestPermission(permission):
            try c.encode(Tag.requestPermission.rawValue, forKey: .type)
            try c.encode(permission, forKey: .permission)
        case .shutdown: try c.encode(Tag.shutdown.rawValue, forKey: .type)
        }
    }
}
