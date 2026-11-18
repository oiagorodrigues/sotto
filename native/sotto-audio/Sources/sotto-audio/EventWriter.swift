import Darwin
import Foundation
import SottoProtocol

/// The only place that writes to stdout, so lines from different tasks never interleave.
final class EventWriter: @unchecked Sendable {
    private let lock = NSLock()

    func emit(_ event: SidecarEvent) {
        guard let line = try? SidecarJSON.encodeLine(event) else {
            Log.error("could not encode an event")
            return
        }
        lock.lock()
        defer { lock.unlock() }
        if !writeAll(fd: STDOUT_FILENO, line) {
            exit(1)
        }
    }

    func error(_ code: SidecarErrorCode, _ message: String, fatal: Bool = false) {
        Log.error("\(code.rawValue): \(message)")
        emit(.error(code: code, message: message, fatal: fatal))
    }

    func fatal(_ code: SidecarErrorCode, _ message: String) -> Never {
        error(code, message, fatal: true)
        exit(1)
    }
}

func writeAll(fd: Int32, _ data: Data) -> Bool {
    data.withUnsafeBytes { raw in
        var offset = 0
        while offset < raw.count {
            let n = write(fd, raw.baseAddress! + offset, raw.count - offset)
            if n < 0 {
                if errno == EINTR { continue }
                return false
            }
            offset += n
        }
        return true
    }
}

enum Log {
    static func error(_ message: String) {
        FileHandle.standardError.write(Data("sotto-audio: \(message)\n".utf8))
    }
}
