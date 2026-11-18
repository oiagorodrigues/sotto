import AVFoundation
import Foundation
import SottoAudioCore
import SottoProtocol

struct CapturedBuffer: @unchecked Sendable {
    let buffer: AVAudioPCMBuffer
    /// Capture time relative to the shared t0.
    let hostOffsetNs: Int64
}

typealias BufferSink = @Sendable (AVAudioPCMBuffer, Int64) -> Void

/// Moves captured audio off the real-time callback: a bounded queue, then a serial queue that
/// converts to the analyzer format, reports levels and hands the result to `sink`.
final class StreamProcessor: @unchecked Sendable {
    let speaker: Speaker
    private let queue = BoundedQueue<CapturedBuffer>(capacity: 128)
    private let work: DispatchQueue
    private let monitor = DispatchQueue(label: "sotto.audio.monitor", qos: .utility)
    private let writer: EventWriter
    private let sink: BufferSink
    private let converter: FormatConverter
    private var timeline: StreamTimeline
    private var levels: LevelAccumulator
    private let overflowLock = NSLock()
    private var reportedDrops: UInt64 = 0
    private var overflowTimer: DispatchSourceTimer?

    init(speaker: Speaker, target: AVAudioFormat, writer: EventWriter, sink: @escaping BufferSink) {
        self.speaker = speaker
        self.writer = writer
        self.sink = sink
        converter = FormatConverter(target: target)
        let rate = Int(target.sampleRate)
        timeline = StreamTimeline(sampleRate: rate)
        levels = LevelAccumulator(sampleRate: rate)
        work = DispatchQueue(label: "sotto.audio.\(speaker.rawValue)", qos: .userInitiated)
        startOverflowMonitor()
    }

    /// Called from capture callbacks. Never blocks; a full queue drops the buffer and counts it.
    func submit(_ captured: CapturedBuffer) {
        if queue.push(captured) {
            work.async { [self] in
                if let next = queue.pop() { process(next) }
            }
        }
    }

    /// File mode: the producer is not real time, so it waits instead of dropping.
    func submitBlocking(_ captured: CapturedBuffer) {
        work.sync { process(captured) }
    }

    /// Waits until everything submitted so far has been processed.
    func drain() {
        work.sync {}
        reportOverflow()
    }

    func stopMonitoring() {
        overflowTimer?.cancel()
        overflowTimer = nil
    }

    private func process(_ captured: CapturedBuffer) {
        let out: AVAudioPCMBuffer?
        do {
            out = try converter.convert(captured.buffer)
        } catch {
            writer.error(.audioDevice, "audio conversion failed: \(error)")
            return
        }
        guard let out, out.frameLength > 0 else { return }

        let start = timeline.place(hostOffsetNs: captured.hostOffsetNs, frameCount: Int(out.frameLength))
        for db in measure(out) {
            writer.emit(.level(speaker: speaker, dbfs: db))
        }
        sink(out, start)
    }

    private func measure(_ buffer: AVAudioPCMBuffer) -> [Float] {
        let n = Int(buffer.frameLength)
        if let ints = buffer.int16ChannelData {
            return levels.add(int16: UnsafeBufferPointer(start: ints[0], count: n))
        }
        if let floats = buffer.floatChannelData {
            return levels.add(float: UnsafeBufferPointer(start: floats[0], count: n))
        }
        return []
    }

    private func startOverflowMonitor() {
        let timer = DispatchSource.makeTimerSource(queue: monitor)
        timer.schedule(deadline: .now() + 1, repeating: 1)
        timer.setEventHandler { [weak self] in self?.reportOverflow() }
        timer.resume()
        overflowTimer = timer
    }

    private func reportOverflow() {
        let dropped = queue.droppedCount
        overflowLock.lock()
        defer { overflowLock.unlock() }
        guard dropped != reportedDrops else { return }
        reportedDrops = dropped
        writer.emit(.overflow(speaker: speaker, droppedBuffers: dropped))
    }
}

enum ConversionError: Error {
    case noConverter(String)
    case failed(String)
}

/// One `AVAudioConverter` per stream, kept across buffers because the resampler carries state.
final class FormatConverter {
    private let target: AVAudioFormat
    private var converter: AVAudioConverter?
    private var inputFormat: AVAudioFormat?

    init(target: AVAudioFormat) {
        self.target = target
    }

    func convert(_ input: AVAudioPCMBuffer) throws -> AVAudioPCMBuffer? {
        var source = input
        if inputFormat != source.format {
            converter = AVAudioConverter(from: source.format, to: target)
            inputFormat = source.format
        }
        if converter == nil {
            // Voice processing can report a many-channel format without a layout, which
            // AVAudioConverter refuses to mix down.
            guard let mono = Self.mixToMono(source),
                  let c = AVAudioConverter(from: mono.format, to: target)
            else { throw ConversionError.noConverter("\(source.format)") }
            converter = c
            inputFormat = input.format
            source = mono
        } else if let c = converter, c.inputFormat != source.format {
            guard let mono = Self.mixToMono(source) else { throw ConversionError.noConverter("\(source.format)") }
            source = mono
        }
        guard let converter else { return nil }

        let ratio = target.sampleRate / source.format.sampleRate
        let capacity = AVAudioFrameCount((Double(source.frameLength) * ratio).rounded(.up)) + 32
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return nil }
        var consumed = false
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            if consumed {
                inputStatus.pointee = .noDataNow
                return nil
            }
            consumed = true
            inputStatus.pointee = .haveData
            return source
        }
        if status == .error { throw ConversionError.failed(error.map { "\($0)" } ?? "unknown") }
        return out
    }

    private static func mixToMono(_ input: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        guard let data = input.floatChannelData,
              let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: input.format.sampleRate, channels: 1, interleaved: false),
              let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: input.frameLength),
              let dst = out.floatChannelData?[0]
        else { return nil }
        let frames = Int(input.frameLength)
        let channels = Int(input.format.channelCount)
        let interleaved = input.format.isInterleaved
        for i in 0..<frames {
            var sum: Float = 0
            for ch in 0..<channels {
                sum += interleaved ? data[0][i * channels + ch] : data[ch][i]
            }
            dst[i] = sum / Float(channels)
        }
        out.frameLength = input.frameLength
        return out
    }
}

/// Copies a buffer so the capture callback can return immediately.
func copyBuffer(_ source: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
    guard let copy = AVAudioPCMBuffer(pcmFormat: source.format, frameCapacity: source.frameLength) else { return nil }
    copy.frameLength = source.frameLength
    let src = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: source.audioBufferList))
    let dst = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
    for i in 0..<min(src.count, dst.count) {
        guard let s = src[i].mData, let d = dst[i].mData else { continue }
        memcpy(d, s, Int(min(src[i].mDataByteSize, dst[i].mDataByteSize)))
        dst[i].mDataByteSize = min(src[i].mDataByteSize, dst[i].mDataByteSize)
    }
    return copy
}
