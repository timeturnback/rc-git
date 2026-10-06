/// The repositories whose tabs were closed this run, most recent last, for ⇧⌘T. In memory
/// only: the saved tab list already brings closed tabs back at the next launch.
public struct ClosedTabs {
    public static let capacity = 20

    public private(set) var paths: [String] = []

    public init() {}

    /// Records a closed tab. A repository closed again moves to the top instead of stacking
    /// twice; the oldest entry falls off past the capacity.
    public mutating func push(_ path: String) {
        paths.removeAll { $0 == path }
        paths.append(path)
        if paths.count > Self.capacity { paths.removeFirst(paths.count - Self.capacity) }
    }

    /// Whether `pop(skipping:)` has anything to hand back.
    public func canReopen(skipping open: Set<String>) -> Bool {
        paths.contains { !open.contains($0) }
    }

    /// The most recently closed repository that is not open now. Entries above it that were
    /// reopened some other way (Open Recent, a drop) are discarded on the way down.
    public mutating func pop(skipping open: Set<String>) -> String? {
        while let path = paths.popLast() {
            if !open.contains(path) { return path }
        }
        return nil
    }
}
