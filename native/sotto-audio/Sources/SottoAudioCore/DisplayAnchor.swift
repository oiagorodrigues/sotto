import CoreGraphics
import Foundation

public struct WindowSnapshot: Sendable, Equatable {
    public let ownerPID: Int32
    public let layer: Int
    public let bounds: CGRect

    public init(ownerPID: Int32, layer: Int, bounds: CGRect) {
        self.ownerPID = ownerPID
        self.layer = layer
        self.bounds = bounds
    }
}

public enum DisplayAnchor {
    /// The overlay and notch windows run at level 25, above the normal windows at level 0, so the
    /// highest layer wins; within a layer the input order (front to back) decides.
    public static func point(for windows: [WindowSnapshot], ownerPID: Int32) -> CGPoint? {
        let mine = windows.filter { $0.ownerPID == ownerPID && $0.bounds.width > 0 && $0.bounds.height > 0 }
        guard let best = mine.indices.max(by: { a, b in
            mine[a].layer != mine[b].layer ? mine[a].layer < mine[b].layer : a > b
        }) else { return nil }
        return CGPoint(x: mine[best].bounds.midX, y: mine[best].bounds.midY)
    }
}
