import Foundation
import SottoProtocol

/// Frame on fd 3: speaker (u8), start_ms (u64 LE), sample_count (u32 LE), then i16 LE samples.
public enum PCMFrameEncoder {
    public static func encode(speaker: Speaker, startMs: UInt64, samples: [Int16]) -> Data {
        var data = Data(capacity: pcmFrameHeaderBytes + samples.count * 2)
        data.append(speaker.pcmTag)
        withUnsafeBytes(of: startMs.littleEndian) { data.append(contentsOf: $0) }
        withUnsafeBytes(of: UInt32(samples.count).littleEndian) { data.append(contentsOf: $0) }
        for s in samples {
            withUnsafeBytes(of: s.littleEndian) { data.append(contentsOf: $0) }
        }
        return data
    }
}

/// Root-mean-square level of 16-bit samples in dBFS, floored at -120 because JSON has no -inf.
public enum LevelMeter {
    public static let floorDbfs: Float = -120

    public static func dbfs(int16 samples: UnsafeBufferPointer<Int16>) -> Float {
        guard !samples.isEmpty else { return floorDbfs }
        var sum = 0.0
        for s in samples {
            let v = Double(s) / 32768.0
            sum += v * v
        }
        return dbfs(meanSquare: sum / Double(samples.count))
    }

    public static func dbfs(float samples: UnsafeBufferPointer<Float>) -> Float {
        guard !samples.isEmpty else { return floorDbfs }
        var sum = 0.0
        for s in samples { sum += Double(s) * Double(s) }
        return dbfs(meanSquare: sum / Double(samples.count))
    }

    static func dbfs(meanSquare: Double) -> Float {
        guard meanSquare > 0 else { return floorDbfs }
        return max(floorDbfs, Float(10 * log10(meanSquare)))
    }
}

/// Accumulates energy and reports a level every `intervalSamples` (about 10 Hz at 16 kHz).
public struct LevelAccumulator: Sendable {
    private let intervalSamples: Int
    private var sumSquares = 0.0
    private var count = 0

    public init(sampleRate: Int, hz: Int = 10) {
        intervalSamples = max(1, sampleRate / hz)
    }

    public mutating func add(int16 samples: UnsafeBufferPointer<Int16>) -> [Float] {
        var out: [Float] = []
        for s in samples {
            let v = Double(s) / 32768.0
            sumSquares += v * v
            count += 1
            if count >= intervalSamples { out.append(flush()) }
        }
        return out
    }

    public mutating func add(float samples: UnsafeBufferPointer<Float>) -> [Float] {
        var out: [Float] = []
        for s in samples {
            sumSquares += Double(s) * Double(s)
            count += 1
            if count >= intervalSamples { out.append(flush()) }
        }
        return out
    }

    private mutating func flush() -> Float {
        let level = LevelMeter.dbfs(meanSquare: sumSquares / Double(count))
        sumSquares = 0
        count = 0
        return level
    }
}
