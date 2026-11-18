import Darwin
import Foundation
import Speech
import SottoProtocol

func readyEvent() async -> SidecarEvent {
    let locales = await SpeechTranscriber.installedLocales
        .map { $0.identifier(.bcp47) }
        .sorted()
    return .ready(protocolVersion: SottoProtocol.protocolVersion, sidecarVersion: sidecarVersion, installedLocales: locales)
}

struct Arguments {
    var excludePID: pid_t?
    var file: String?
    var speaker: Speaker = .them
    var locale = "en-US"
    var realtime = false

    init(_ args: [String]) throws {
        var it = args.dropFirst().makeIterator()
        func value(_ flag: String) throws -> String {
            guard let v = it.next() else { throw ArgumentError(message: "\(flag) needs a value") }
            return v
        }
        while let arg = it.next() {
            switch arg {
            case "--exclude-pid":
                guard let pid = pid_t(try value(arg)) else { throw ArgumentError(message: "--exclude-pid needs an integer") }
                excludePID = pid
            case "--file": file = try value(arg)
            case "--speaker":
                guard let s = Speaker(rawValue: try value(arg)) else { throw ArgumentError(message: "--speaker is me or them") }
                speaker = s
            case "--locale": locale = try value(arg)
            case "--realtime": realtime = true
            default: throw ArgumentError(message: "unknown argument \(arg)")
            }
        }
    }
}

struct ArgumentError: Error { let message: String }

@main
struct SottoAudio {
    static func main() async {
        signal(SIGPIPE, SIG_IGN)
        let writer = EventWriter()
        let arguments: Arguments
        do {
            arguments = try Arguments(CommandLine.arguments)
        } catch let error as ArgumentError {
            writer.fatal(.badCommand, error.message)
        } catch {
            writer.fatal(.internal, "\(error)")
        }

        if let file = arguments.file {
            await runFileMode(
                FileModeOptions(path: file, speaker: arguments.speaker, locale: arguments.locale, realtime: arguments.realtime),
                writer: writer
            )
        }

        writer.emit(await readyEvent())
        let session = Session(writer: writer, parentPID: arguments.excludePID ?? getppid())
        let commands = readCommands(writer: writer)
        for await command in commands {
            await session.handle(command)
        }
        await session.handle(.shutdown)
    }

    /// Blocking stdin reads stay on their own thread; a decoded command goes to the async loop.
    private static func readCommands(writer: EventWriter) -> AsyncStream<SidecarCommand> {
        let (stream, continuation) = AsyncStream<SidecarCommand>.makeStream()
        let thread = Thread {
            while let line = readLine(strippingNewline: true) {
                if line.trimmingCharacters(in: .whitespaces).isEmpty { continue }
                do {
                    continuation.yield(try SidecarJSON.decodeCommand(Data(line.utf8)))
                } catch {
                    writer.error(.badCommand, "invalid command: \(error)")
                }
            }
            continuation.finish()
        }
        thread.qualityOfService = .userInitiated
        thread.start()
        return stream
    }
}
