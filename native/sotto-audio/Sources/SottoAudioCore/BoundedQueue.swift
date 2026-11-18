import Foundation

/// FIFO with a fixed capacity. A full queue rejects the new element and counts it, so load
/// shows up as an `overflow` event instead of silent loss.
public final class BoundedQueue<Element: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [Element] = []
    private var head = 0
    private var dropped: UInt64 = 0
    public let capacity: Int

    public init(capacity: Int) {
        precondition(capacity > 0)
        self.capacity = capacity
    }

    /// Returns false when the element was dropped.
    @discardableResult
    public func push(_ element: Element) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if items.count - head >= capacity {
            dropped += 1
            return false
        }
        items.append(element)
        return true
    }

    public func pop() -> Element? {
        lock.lock()
        defer { lock.unlock() }
        guard head < items.count else { return nil }
        let element = items[head]
        head += 1
        if head > 64 && head * 2 > items.count {
            items.removeFirst(head)
            head = 0
        }
        return element
    }

    public var droppedCount: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return dropped
    }

    public var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return items.count - head
    }
}
