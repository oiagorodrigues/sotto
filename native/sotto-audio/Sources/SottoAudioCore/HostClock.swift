import Darwin
import Foundation

/// Converts Mach host ticks (`AVAudioTime.hostTime`, `AudioTimeStamp.mHostTime`) to nanoseconds
/// and to offsets from one origin shared by both streams.
public struct HostClock: Sendable, Equatable {
    public let numer: UInt32
    public let denom: UInt32
    public let t0Ticks: UInt64

    public init(t0Ticks: UInt64, numer: UInt32, denom: UInt32) {
        self.t0Ticks = t0Ticks
        self.numer = numer
        self.denom = max(denom, 1)
    }

    public static func systemTimebase() -> (numer: UInt32, denom: UInt32) {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        return (info.numer, info.denom)
    }

    public static func now() -> HostClock {
        let tb = systemTimebase()
        return HostClock(t0Ticks: mach_absolute_time(), numer: tb.numer, denom: tb.denom)
    }

    public func nanos(fromTicks ticks: UInt64) -> UInt64 {
        let (high, low) = ticks.multipliedFullWidth(by: UInt64(numer))
        return UInt64(denom).dividingFullWidth((high, low)).quotient
    }

    public var t0Ns: UInt64 { nanos(fromTicks: t0Ticks) }

    /// Signed because a buffer captured just before `t0` is older than the origin.
    public func offsetNs(fromTicks ticks: UInt64) -> Int64 {
        let ns = nanos(fromTicks: ticks)
        return ns >= t0Ns ? Int64(clamping: ns - t0Ns) : -Int64(clamping: t0Ns - ns)
    }

    /// Milliseconds from `t0` to `ticks`, clamped at zero, for `Started.emitted_at_ms`.
    public func elapsedMs(atTicks ticks: UInt64) -> UInt64 {
        UInt64(max(0, offsetMs(fromTicks: ticks)))
    }

    public func elapsedMsNow() -> UInt64 {
        elapsedMs(atTicks: mach_absolute_time())
    }

    public func offsetMs(fromTicks ticks: UInt64) -> Int64 {
        offsetNs(fromTicks: ticks) / 1_000_000
    }
}

/// Places consecutive buffers of one stream on the shared timeline. The analyzer rejects
/// overlapping or reordered input, so host-time jitter below `toleranceMs` is absorbed and a
/// real gap (dropped buffers, device pause) is kept.
public struct StreamTimeline: Sendable {
    public let sampleRate: Int
    public let toleranceMs: Int
    private var nextSample: Int64?

    public init(sampleRate: Int, toleranceMs: Int = 50) {
        self.sampleRate = sampleRate
        self.toleranceMs = toleranceMs
    }

    /// Returns the start sample of a buffer of `frameCount` samples captured at `hostOffsetNs`.
    public mutating func place(hostOffsetNs: Int64, frameCount: Int) -> Int64 {
        let observed = max(0, hostOffsetNs) * Int64(sampleRate) / 1_000_000_000
        let start: Int64
        if let expected = nextSample {
            let tolerance = Int64(sampleRate * toleranceMs / 1000)
            start = observed > expected + tolerance ? observed : expected
        } else {
            start = observed
        }
        nextSample = start + Int64(frameCount)
        return start
    }

    public func milliseconds(forSample sample: Int64) -> UInt64 {
        UInt64(max(0, sample)) * 1000 / UInt64(sampleRate)
    }
}
