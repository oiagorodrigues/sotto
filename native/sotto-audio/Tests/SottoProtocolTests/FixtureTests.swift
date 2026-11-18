import Foundation
import SottoProtocol
import Testing

private let packageRoot = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
private let fixtureDir = packageRoot.appendingPathComponent("../../crates/sotto-protocol/tests/fixtures/sidecar").standardized

private func fixtures() throws -> [(name: String, data: Data)] {
    let names = try FileManager.default.contentsOfDirectory(atPath: fixtureDir.path).sorted()
    return try names.filter { $0.hasSuffix(".json") }.map { ($0, try Data(contentsOf: fixtureDir.appendingPathComponent($0))) }
}

private func jsonObject(_ data: Data) throws -> NSObject {
    try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) as! NSObject
}

@Suite struct FixtureTests {
    @Test func fixturesExist() throws {
        let all = try fixtures()
        #expect(all.count >= 16)
        #expect(all.contains { $0.name.hasPrefix("command_") })
        #expect(all.contains { $0.name.hasPrefix("event_") })
    }

    @Test func everyFixtureRoundTrips() throws {
        for (name, data) in try fixtures() {
            let encoded: Data
            if name.hasPrefix("command_") {
                let command = try JSONDecoder().decode(SidecarCommand.self, from: data)
                encoded = try JSONEncoder().encode(command)
            } else {
                let event = try JSONDecoder().decode(SidecarEvent.self, from: data)
                encoded = try JSONEncoder().encode(event)
            }
            #expect(try jsonObject(encoded) == jsonObject(data), "\(name) changed after a round trip")
        }
    }

    @Test func decodedValues() throws {
        let start = try SidecarJSON.decodeCommand(Data(#"{"type":"start","locale":"pt-BR","engine":"local","speakers":["me","them"]}"#.utf8))
        #expect(start == .start(locale: "pt-BR", engine: .local, speakers: [.me, .them]))
        let line = try SidecarJSON.encodeLine(.stopped)
        #expect(String(decoding: line, as: UTF8.self) == "{\"type\":\"stopped\"}\n")
    }

    @Test func uuidsAreLowercaseStrings() throws {
        let id = UUID(uuidString: "01930A6E-8B7C-7D3E-9F00-0123456789AB")!
        let line = try SidecarJSON.encodeLine(.screenshot(requestID: id, pngBase64: "", widthPx: 1, heightPx: 1))
        #expect(String(decoding: line, as: UTF8.self).contains("01930a6e-8b7c-7d3e-9f00-0123456789ab"))
    }

    @Test(arguments: [
        #"{"type":"stop","extra":1}"#,
        #"{"type":"shutdown","x":true}"#,
        #"{"type":"finalize","speaker":"me","through_ms":10,"x":true}"#,
        #"{"type":"start","locale":"pt-BR","engine":"local","speakers":["me"],"extra":"no"}"#,
        #"{"type":"check_permissions","a":1}"#,
        #"{"type":"request_permission","permission":"microphone","a":1}"#,
        #"{"type":"capture_screenshot","request_id":"01930a6e-8b7c-7d3e-9f00-0123456789ab","max_long_side_px":10,"a":1}"#,
    ])
    func unknownCommandFieldsAreRejected(json: String) {
        #expect(throws: DecodingError.self) { try SidecarJSON.decodeCommand(Data(json.utf8)) }
    }

    @Test func unknownCommandTypeIsRejected() {
        #expect(throws: DecodingError.self) { try SidecarJSON.decodeCommand(Data(#"{"type":"explode"}"#.utf8)) }
    }

    @Test func startedCarriesEmittedAtMs() throws {
        let line = try SidecarJSON.encodeLine(.started(t0HostNs: 123_456_789_000, emittedAtMs: 412))
        let text = String(decoding: line, as: UTF8.self)
        #expect(text.contains(#""emitted_at_ms":412"#) && text.contains(#""t0_host_ns":123456789000"#))
        #expect(try SidecarJSON.decodeEvent(line) == .started(t0HostNs: 123_456_789_000, emittedAtMs: 412))
        #expect(throws: DecodingError.self) {
            try SidecarJSON.decodeEvent(Data(#"{"type":"started","t0_host_ns":1}"#.utf8))
        }
    }

    @Test func startFailuresAreFatalErrors() throws {
        let event = SidecarEvent.startFailure(.unsupportedLocale, "locale xx is not supported")
        #expect(event == .error(code: .unsupportedLocale, message: "locale xx is not supported", fatal: true))
        let text = String(decoding: try SidecarJSON.encodeLine(event), as: UTF8.self)
        #expect(text.contains(#""fatal":true"#))
    }
}
