import Foundation

public let protocolVersion: UInt32 = 1
public let pcmSampleRateHz = 16_000
public let pcmFrameHeaderBytes = 13

public enum Speaker: String, Codable, Sendable, Equatable, CaseIterable {
    case me
    case them

    public var pcmTag: UInt8 {
        switch self {
        case .me: 0
        case .them: 1
        }
    }
}

public enum EngineKind: String, Codable, Sendable, Equatable {
    case local
    case raw
}

public enum PermissionKind: String, Codable, Sendable, Equatable, CaseIterable {
    case microphone
    case systemAudio = "system_audio"
    case speechRecognition = "speech_recognition"
    case screenRecording = "screen_recording"
}

public enum PermissionStatus: String, Codable, Sendable, Equatable {
    case granted
    case denied
    case notDetermined = "not_determined"
    case restricted
}

public enum SidecarErrorCode: String, Codable, Sendable, Equatable {
    case unsupportedLocale = "unsupported_locale"
    case assetsUnavailable = "assets_unavailable"
    case permissionDenied = "permission_denied"
    case audioDevice = "audio_device"
    case analyzer
    case screenshot
    case badCommand = "bad_command"
    case `internal`
}

public struct PermissionState: Codable, Sendable, Equatable {
    public var permission: PermissionKind
    public var status: PermissionStatus

    public init(permission: PermissionKind, status: PermissionStatus) {
        self.permission = permission
        self.status = status
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case permission, status
    }

    public init(from decoder: any Decoder) throws {
        try rejectUnknownKeys(decoder, allowed: CodingKeys.allCases.map(\.stringValue))
        let c = try decoder.container(keyedBy: CodingKeys.self)
        permission = try c.decode(PermissionKind.self, forKey: .permission)
        status = try c.decode(PermissionStatus.self, forKey: .status)
    }
}

struct DynamicKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

func rejectUnknownKeys(_ decoder: any Decoder, allowed: [String]) throws {
    let c = try decoder.container(keyedBy: DynamicKey.self)
    let extra = c.allKeys.map(\.stringValue).filter { !allowed.contains($0) }
    if let key = extra.sorted().first {
        throw DecodingError.dataCorrupted(.init(
            codingPath: decoder.codingPath,
            debugDescription: "unknown field \"\(key)\""
        ))
    }
}

func decodeUUID<K: CodingKey>(_ c: KeyedDecodingContainer<K>, _ key: K) throws -> UUID {
    let s = try c.decode(String.self, forKey: key)
    guard let id = UUID(uuidString: s) else {
        throw DecodingError.dataCorruptedError(forKey: key, in: c, debugDescription: "invalid UUID")
    }
    return id
}
