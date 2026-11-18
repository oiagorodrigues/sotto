import Foundation
import SottoAudioCore
import SottoProtocol
import Testing

private func volatile(_ s: UInt64, _ e: UInt64, _ t: String) -> RawTranscriptResult {
    RawTranscriptResult(startMs: s, endMs: e, text: t, isFinal: false)
}

private func final(_ s: UInt64, _ e: UInt64, _ t: String) -> RawTranscriptResult {
    RawTranscriptResult(startMs: s, endMs: e, text: t, isFinal: true)
}

@Suite struct SegmentMapperTests {
    @Test func growthKeepsSegmentAndBumpsRevision() {
        var m = SegmentMapper()
        let a = m.ingest(volatile(0, 500, "so"))
        let b = m.ingest(volatile(0, 900, "so how"))
        #expect(a?.segmentID == 1 && a?.revision == 1)
        #expect(b?.segmentID == 1 && b?.revision == 2)
        #expect(b?.text == "so how" && b?.endMs == 900 && b?.isFinal == false)
    }

    @Test func replacementOverwritesText() {
        var m = SegmentMapper()
        _ = m.ingest(volatile(0, 900, "we try"))
        let b = m.ingest(volatile(0, 900, "we tried"))
        #expect(b?.segmentID == 1 && b?.revision == 2 && b?.text == "we tried")
    }

    @Test func emptyTextRevokesAndClosesTheSegment() {
        var m = SegmentMapper()
        _ = m.ingest(volatile(0, 900, "noise"))
        let revoked = m.ingest(volatile(0, 900, "  "))
        #expect(revoked?.segmentID == 1 && revoked?.revision == 2 && revoked?.text == "")
        let next = m.ingest(volatile(1000, 1500, "hello"))
        #expect(next?.segmentID == 2 && next?.revision == 1)
    }

    @Test func emptyTextWithNothingOpenIsIgnored() {
        var m = SegmentMapper()
        #expect(m.ingest(volatile(0, 100, "")) == nil)
    }

    @Test func finalClosesAndNextRangeGetsNewId() {
        var m = SegmentMapper()
        _ = m.ingest(volatile(0, 900, "we tried"))
        let f = m.ingest(final(0, 3960, "We tried vendors."))
        #expect(f?.segmentID == 1 && f?.revision == 2 && f?.isFinal == true && f?.endMs == 3960)
        let next = m.ingest(volatile(3960, 5000, "so"))
        #expect(next?.segmentID == 2 && next?.revision == 1 && next?.isFinal == false)
    }

    @Test func finalWithoutVolatileStartsItsOwnSegment() {
        var m = SegmentMapper()
        let f = m.ingest(final(0, 1000, "hi"))
        #expect(f?.segmentID == 1 && f?.revision == 1 && f?.isFinal == true)
    }

    @Test func staleResultInsideFinalizedRangeIsDropped() {
        var m = SegmentMapper()
        _ = m.ingest(final(0, 2000, "done"))
        #expect(m.ingest(volatile(0, 1500, "late")) == nil)
        #expect(m.ingest(volatile(2000, 2500, "new")) != nil)
    }

    @Test func idsAreIndependentPerMapper() {
        var a = SegmentMapper()
        var b = SegmentMapper()
        _ = a.ingest(final(0, 10, "x"))
        #expect(b.ingest(volatile(0, 10, "y"))?.segmentID == 1)
    }
}

@Suite struct ClockTests {
    @Test func appleSiliconTimebaseIsIdentity() {
        let clock = HostClock(t0Ticks: 1_000, numer: 1, denom: 1)
        #expect(clock.nanos(fromTicks: 5_000) == 5_000)
        #expect(clock.offsetNs(fromTicks: 4_000) == 3_000)
    }

    @Test func intelStyleTimebaseScales() {
        let clock = HostClock(t0Ticks: 0, numer: 125, denom: 3)
        #expect(clock.nanos(fromTicks: 3_000) == 125_000)
        #expect(clock.offsetMs(fromTicks: 24_000_000) == 1_000)
    }

    @Test func offsetBeforeT0IsNegative() {
        let clock = HostClock(t0Ticks: 10_000_000, numer: 1, denom: 1)
        #expect(clock.offsetNs(fromTicks: 4_000_000) == -6_000_000)
    }

    @Test func largeTicksDoNotOverflow() {
        let clock = HostClock(t0Ticks: 0, numer: 125, denom: 3)
        #expect(clock.nanos(fromTicks: UInt64.max / 200) > 0)
    }

    @Test func bothStreamsShareOneOrigin() {
        let clock = HostClock(t0Ticks: 1_000_000, numer: 1, denom: 1)
        let mic = clock.offsetMs(fromTicks: 1_000_000 + 250_000_000)
        let tap = clock.offsetMs(fromTicks: 1_000_000 + 250_000_000)
        #expect(mic == 250 && tap == 250)
    }

    @Test func timelineAbsorbsJitterAndKeepsGaps() {
        var t = StreamTimeline(sampleRate: 16_000)
        #expect(t.place(hostOffsetNs: 0, frameCount: 1_600) == 0)
        // 5 ms of jitter stays contiguous
        #expect(t.place(hostOffsetNs: 105_000_000, frameCount: 1_600) == 1_600)
        // a 500 ms hole is kept
        let afterGap = t.place(hostOffsetNs: 700_000_000, frameCount: 1_600)
        #expect(afterGap == 11_200)
        #expect(t.milliseconds(forSample: afterGap) == 700)
    }

    @Test func timelineNeverMovesBackwards() {
        var t = StreamTimeline(sampleRate: 16_000)
        _ = t.place(hostOffsetNs: 1_000_000_000, frameCount: 1_600)
        #expect(t.place(hostOffsetNs: 500_000_000, frameCount: 1_600) == 17_600)
        #expect(t.place(hostOffsetNs: -5, frameCount: 10) == 19_200)
    }
}

@Suite struct PCMTests {
    @Test func frameLayout() {
        let frame = PCMFrameEncoder.encode(speaker: .them, startMs: 0x0102, samples: [1, -2, 0x0304])
        #expect(frame.count == pcmFrameHeaderBytes + 6)
        #expect(Array(frame) == [1, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0xFE, 0xFF, 0x04, 0x03])
    }

    @Test func meTagIsZero() {
        #expect(PCMFrameEncoder.encode(speaker: .me, startMs: 0, samples: [])[0] == 0)
        #expect(PCMFrameEncoder.encode(speaker: .me, startMs: 0, samples: []).count == 13)
    }

    @Test func levelOfFullScaleAndSilence() {
        let full = [Int16](repeating: .max, count: 100)
        let silent = [Int16](repeating: 0, count: 100)
        #expect(full.withUnsafeBufferPointer { LevelMeter.dbfs(int16: $0) } > -0.01)
        #expect(silent.withUnsafeBufferPointer { LevelMeter.dbfs(int16: $0) } == LevelMeter.floorDbfs)
    }

    @Test func accumulatorReportsTenTimesPerSecond() {
        var acc = LevelAccumulator(sampleRate: 16_000)
        let second = [Int16](repeating: 8_192, count: 16_000)
        let levels = second.withUnsafeBufferPointer { acc.add(int16: $0) }
        #expect(levels.count == 10)
        #expect(abs(levels[0] - -12.04) < 0.05)
    }
}

@Suite struct BoundedQueueTests {
    @Test func dropsWhenFullAndCounts() {
        let q = BoundedQueue<Int>(capacity: 2)
        #expect(q.push(1) && q.push(2))
        #expect(!q.push(3) && !q.push(4))
        #expect(q.droppedCount == 2 && q.count == 2)
        #expect(q.pop() == 1)
        #expect(q.push(5))
        #expect(q.pop() == 2 && q.pop() == 5 && q.pop() == nil)
        #expect(q.droppedCount == 2)
    }

    @Test func concurrentProducersAccountForEveryElement() async {
        let q = BoundedQueue<Int>(capacity: 100)
        await withTaskGroup(of: Void.self) { group in
            for p in 0..<8 {
                group.addTask { for i in 0..<100 { q.push(p * 100 + i) } }
            }
        }
        #expect(UInt64(q.count) + q.droppedCount == 800)
        #expect(q.count == 100)
    }
}

@Suite struct StartedClockTests {
    @Test func elapsedMsIsMeasuredFromT0AndClampedAtZero() {
        let clock = HostClock(t0Ticks: 1_000_000_000, numer: 1, denom: 1)
        #expect(clock.elapsedMs(atTicks: 1_412_000_000) == 412)
        #expect(clock.elapsedMs(atTicks: 500_000_000) == 0)
    }
}

@Suite struct DisplayAnchorTests {
    private let normal = WindowSnapshot(ownerPID: 7, layer: 0, bounds: CGRect(x: 0, y: 0, width: 400, height: 400))
    private let overlay = WindowSnapshot(ownerPID: 7, layer: 25, bounds: CGRect(x: 2000, y: 0, width: 100, height: 100))

    @Test func prefersTheOverlayLayer() {
        #expect(DisplayAnchor.point(for: [normal, overlay], ownerPID: 7) == CGPoint(x: 2050, y: 50))
    }

    @Test func findsAnOverlayWhenNoWindowIsAtLevelZero() {
        #expect(DisplayAnchor.point(for: [overlay], ownerPID: 7) == CGPoint(x: 2050, y: 50))
    }

    @Test func frontmostWinsWithinALayer() {
        let other = WindowSnapshot(ownerPID: 7, layer: 0, bounds: CGRect(x: 10, y: 10, width: 20, height: 20))
        #expect(DisplayAnchor.point(for: [other, normal], ownerPID: 7) == CGPoint(x: 20, y: 20))
    }

    @Test func ignoresOtherOwnersAndEmptyWindows() {
        let foreign = WindowSnapshot(ownerPID: 9, layer: 25, bounds: CGRect(x: 5, y: 5, width: 10, height: 10))
        let empty = WindowSnapshot(ownerPID: 7, layer: 25, bounds: .zero)
        #expect(DisplayAnchor.point(for: [foreign, empty], ownerPID: 7) == nil)
        #expect(DisplayAnchor.point(for: [foreign, empty, normal], ownerPID: 7) == CGPoint(x: 200, y: 200))
    }
}
