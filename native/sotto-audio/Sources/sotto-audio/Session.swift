import AVFoundation
import Foundation
import SottoAudioCore
import SottoProtocol

let sidecarVersion = "0.1.0"
private let rawFrameFD: Int32 = 3

/// Writes PCM frames to fd 3 for the cloud engine.
final class RawFrameWriter: @unchecked Sendable {
    private let lock = NSLock()

    static func isAvailable() -> Bool {
        fcntl(rawFrameFD, F_GETFD) != -1
    }

    func write(speaker: Speaker, startMs: UInt64, samples: [Int16]) -> Bool {
        let frame = PCMFrameEncoder.encode(speaker: speaker, startMs: startMs, samples: samples)
        lock.lock()
        defer { lock.unlock() }
        return writeAll(fd: rawFrameFD, frame)
    }
}

private final class Pipeline {
    let processor: StreamProcessor
    let speech: SpeechStream?

    init(processor: StreamProcessor, speech: SpeechStream?) {
        self.processor = processor
        self.speech = speech
    }
}

actor Session {
    private let writer: EventWriter
    private let permissions = PermissionCenter()
    private let parentPID: pid_t
    private let rawWriter = RawFrameWriter()

    private var pipelines: [Speaker: Pipeline] = [:]
    private var mic: MicCapture?
    private var tap: SystemAudioCapture?
    private var active = false

    init(writer: EventWriter, parentPID: pid_t) {
        self.writer = writer
        self.parentPID = parentPID
    }

    func handle(_ command: SidecarCommand) async {
        switch command {
        case let .start(locale, engine, speakers):
            await start(locale: locale, engine: engine, speakers: speakers)
        case .stop:
            await stop()
            writer.emit(.stopped)
        case let .finalize(speaker, throughMs):
            do {
                try await pipelines[speaker]?.speech?.finalize(throughMs: throughMs)
            } catch {
                writer.error(.analyzer, "finalize failed: \(error)")
            }
        case let .captureScreenshot(requestID, maxLongSide):
            await screenshot(requestID: requestID, maxLongSide: maxLongSide)
        case .checkPermissions:
            writer.emit(.permissions(permissions.states()))
        case let .requestPermission(kind):
            await permissions.request(kind)
            writer.emit(.permissions(permissions.states()))
        case .shutdown:
            await stop()
            writer.emit(.stopped)
            exit(0)
        }
    }

    private func start(locale identifier: String, engine: EngineKind, speakers: [Speaker]) async {
        guard !active else {
            writer.error(.badCommand, "a session is already running")
            return
        }
        let unique = Speaker.allCases.filter(speakers.contains)
        guard !unique.isEmpty else {
            writer.error(.badCommand, "start needs at least one speaker")
            return
        }
        if unique.contains(.me), PermissionCenter.microphone() == .notDetermined {
            await permissions.request(.microphone)
        }
        if unique.contains(.me), PermissionCenter.microphone() != .granted {
            await failStart(.permissionDenied, "microphone access is not granted")
            return
        }
        if engine == .raw, !RawFrameWriter.isAvailable() {
            await failStart(.internal, "file descriptor 3 is not open; the raw engine needs it")
            return
        }

        let clock = HostClock.now()
        do {
            var speechLocale: Locale?
            if engine == .local {
                speechLocale = try await SpeechStream.resolveLocale(identifier)
            }
            for speaker in unique {
                pipelines[speaker] = try await makePipeline(speaker: speaker, engine: engine, locale: speechLocale)
            }
            if unique.contains(.me), let pipeline = pipelines[.me] {
                let capture = MicCapture()
                try capture.start(clock: clock, processor: pipeline.processor)
                mic = capture
            }
            if unique.contains(.them), let pipeline = pipelines[.them] {
                let capture = SystemAudioCapture()
                try capture.start(
                    excluding: [parentPID, getpid()],
                    clock: clock, processor: pipeline.processor
                )
                permissions.markSystemAudioGranted()
                tap = capture
            }
        } catch let error as StreamError {
            await failStart(error.code, error.description)
            return
        } catch let error as CaptureError {
            switch error {
            case .permission: await failStart(.permissionDenied, error.description)
            case .device: await failStart(.audioDevice, error.description)
            }
            return
        } catch {
            await failStart(.internal, "\(error)")
            return
        }
        active = true
        writer.emit(.started(t0HostNs: clock.t0Ns, emittedAtMs: clock.elapsedMsNow()))
    }

    /// Tears down anything `start` built, then reports a fatal error; the sidecar stays up for the next command.
    private func failStart(_ code: SidecarErrorCode, _ message: String) async {
        await stop()
        Log.error("\(code.rawValue): \(message)")
        writer.emit(.startFailure(code, message))
    }

    private func makePipeline(speaker: Speaker, engine: EngineKind, locale: Locale?) async throws -> Pipeline {
        switch engine {
        case .local:
            guard let locale else { throw StreamError.unsupportedLocale("") }
            let speech = try await SpeechStream.make(speaker: speaker, locale: locale, writer: writer)
            let processor = StreamProcessor(speaker: speaker, target: speech.format, writer: writer) { buffer, start in
                speech.feed(buffer, startSample: start)
            }
            return Pipeline(processor: processor, speech: speech)
        case .raw:
            guard let format = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(pcmSampleRateHz), channels: 1, interleaved: true) else {
                throw StreamError.noFormat
            }
            let frames = rawWriter
            let w = writer
            let processor = StreamProcessor(speaker: speaker, target: format, writer: writer) { buffer, start in
                guard let channel = buffer.int16ChannelData else { return }
                let samples = Array(UnsafeBufferPointer(start: channel[0], count: Int(buffer.frameLength)))
                let ms = UInt64(max(0, start)) * 1000 / UInt64(pcmSampleRateHz)
                if !frames.write(speaker: speaker, startMs: ms, samples: samples) {
                    w.error(.internal, "writing to fd 3 failed")
                }
            }
            return Pipeline(processor: processor, speech: nil)
        }
    }

    private func stop() async {
        mic?.stop()
        tap?.stop()
        mic = nil
        tap = nil
        let current = pipelines
        pipelines = [:]
        active = false
        for pipeline in current.values {
            pipeline.processor.drain()
            pipeline.processor.stopMonitoring()
            await pipeline.speech?.finish()
        }
    }

    private func screenshot(requestID: UUID, maxLongSide: UInt32) async {
        do {
            let shot = try await Screenshotter.capture(parentPID: parentPID, maxLongSide: maxLongSide)
            writer.emit(.screenshot(
                requestID: requestID, pngBase64: shot.png.base64EncodedString(),
                widthPx: UInt32(shot.width), heightPx: UInt32(shot.height)
            ))
        } catch let error as ScreenshotError {
            writer.error(error.isPermission ? .permissionDenied : .screenshot, error.description)
        } catch {
            writer.error(.screenshot, "\(error)")
        }
    }
}

extension ScreenshotError {
    var isPermission: Bool {
        if case .permission = self { return true }
        return false
    }
}
