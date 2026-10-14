/// The lane graph of the paged history: which column each commit's dot sits in and which
/// line segments cross each row. Built strictly sequentially from `CommitSummary.parents`,
/// so appending a page never changes rows already built — the pane can render incrementally.
///
/// The builder only ever sees commits already paged in, so it satisfies "nothing scales with
/// repository size" by construction. Lane indices are unbounded here; `GraphRow.drawnEdges`
/// clamps them to the drawn width.

/// One line segment crossing a graph row.
public struct GraphEdge: Hashable, Sendable {
    /// How the segment relates to the row's commit dot.
    public enum Phase: Hashable, Sendable {
        /// A lane passing straight through, top edge to bottom edge.
        case through
        /// A lane ending at the dot: a child connection merging in from the top.
        case into
        /// A lane starting at the dot: a parent connection leaving toward the bottom.
        case out
    }

    /// Lane position where the segment starts: at the row's top edge (`through`, `into`) or
    /// at the dot (`out`).
    public let from: Int
    /// Lane position where the segment ends: at the dot (`into`) or at the row's bottom
    /// edge (`through`, `out`).
    public let to: Int
    public let colorIndex: Int
    public let phase: Phase

    public init(from: Int, to: Int, colorIndex: Int, phase: Phase) {
        self.from = from
        self.to = to
        self.colorIndex = colorIndex
        self.phase = phase
    }
}

/// One row of the graph, aligned with one commit of the history list.
public struct GraphRow: Hashable, Sendable {
    /// Lane of the commit's dot.
    public let commitLane: Int
    /// Color of the dot and of the commit's own lane.
    public let colorIndex: Int
    /// Every segment drawn across this row.
    public let edges: [GraphEdge]
    public let isMerge: Bool

    public init(commitLane: Int, colorIndex: Int, edges: [GraphEdge], isMerge: Bool) {
        self.commitLane = commitLane
        self.colorIndex = colorIndex
        self.edges = edges
        self.isMerge = isMerge
    }

    /// Lanes the graph column draws; lanes at or past the last one share its column.
    public static let maxDrawnLanes = 12

    /// The edges to stroke when lanes at or past `laneCap - 1` share one column: positions
    /// clamp to that column, and of the edges that then trace the same segment only the last
    /// survives, since it is the one painted on top. Order is otherwise kept, so the result
    /// draws the same picture as `edges` with one stroke per visible segment.
    public func drawnEdges(laneCap: Int) -> [GraphEdge] {
        let last = max(laneCap, 1) - 1
        let clamped = edges.map { edge in
            GraphEdge(
                from: min(edge.from, last), to: min(edge.to, last), colorIndex: edge.colorIndex,
                phase: edge.phase)
        }
        var lastIndex: [Segment: Int] = [:]
        for (index, edge) in clamped.enumerated() {
            lastIndex[Segment(edge)] = index
        }
        return clamped.indices.compactMap { index in
            lastIndex[Segment(clamped[index])] == index ? clamped[index] : nil
        }
    }

    /// What a stroke covers: an edge without its color.
    private struct Segment: Hashable {
        let from: Int
        let to: Int
        let phase: GraphEdge.Phase

        init(_ edge: GraphEdge) {
            from = edge.from
            to = edge.to
            phase = edge.phase
        }
    }
}

/// Assigns lanes with the active-lanes algorithm: the first lane expecting a commit takes
/// it, other expecting lanes merge in and close, unexpected commits open a new rightmost
/// lane. The commit's lane then expects its first parent; other parents reuse a lane already
/// expecting them or open new lanes. Commit-time order is not topological: under committer
/// clock skew a parent can be emitted before one of its children. A parent already emitted
/// gets no lane, since nothing below would ever take it; the child's edge to it ends at the
/// dot.
public struct GraphBuilder: Sendable {
    public private(set) var rows: [GraphRow] = []
    /// The widest the graph has been, for sizing the column without rescanning rows.
    public private(set) var maxLaneCount = 0

    private struct Lane: Hashable {
        var expected: Oid
        var colorIndex: Int
    }

    private var lanes: [Lane] = []
    private var nextColor = 0
    /// Every commit already given a row; grows only with `rows`.
    private var emitted: Set<Oid> = []

    public init() {}

    /// Extends the graph with the next commits of the history, oldest of the already-built
    /// rows first. Only appends; earlier rows never change.
    public mutating func append(_ commits: ArraySlice<CommitSummary>) {
        for commit in commits {
            appendRow(for: commit)
        }
    }

    private mutating func appendRow(for commit: CommitSummary) {
        emitted.insert(commit.oid)
        let firstParent = commit.parents.first.flatMap { emitted.contains($0) ? nil : $0 }
        let matching = lanes.indices.filter { lanes[$0].expected == commit.oid }
        let topCount = lanes.count
        let commitLane = matching.first ?? topCount
        let color: Int
        if let first = matching.first {
            color = lanes[first].colorIndex
        } else {
            color = takeColor()
        }

        var edges = matching.map { index in
            GraphEdge(
                from: index, to: commitLane, colorIndex: lanes[index].colorIndex, phase: .into)
        }
        let closed = closeMatching(matching)
        var bottom = closed.bottom
        // The commit's own lane continues to its first parent at the commit's position;
        // survivors at or past that slot shift right by one.
        let slot = min(commitLane, bottom.count)
        if let firstParent {
            bottom.insert(Lane(expected: firstParent, colorIndex: color), at: slot)
            edges.append(GraphEdge(from: commitLane, to: slot, colorIndex: color, phase: .out))
        }
        let continues = firstParent != nil
        for (top, pre, colorIndex) in closed.passThroughs {
            let target = continues && pre >= slot ? pre + 1 : pre
            edges.append(
                GraphEdge(from: top, to: target, colorIndex: colorIndex, phase: .through))
        }
        attachParents(
            commit.parents.dropFirst(), commitLane: commitLane, bottom: &bottom, edges: &edges)

        lanes = bottom
        maxLaneCount = max(maxLaneCount, topCount, lanes.count, commitLane + 1)
        rows.append(
            GraphRow(
                commitLane: commitLane, colorIndex: color, edges: edges,
                isMerge: commit.parents.count > 1))
    }

    /// The next color in the append-only sequence.
    private mutating func takeColor() -> Int {
        defer { nextColor += 1 }
        return nextColor
    }

    /// Closes every matching lane; survivors keep their order and shift left past them. Each
    /// pass-through names its lane above the row, its position below before the commit's own
    /// lane is inserted, and its color.
    private func closeMatching(_ matching: [Int])
        -> (bottom: [Lane], passThroughs: [(top: Int, pre: Int, colorIndex: Int)])
    {
        var bottom: [Lane] = []
        var passThroughs: [(top: Int, pre: Int, colorIndex: Int)] = []
        for (index, lane) in lanes.enumerated() where !matching.contains(index) {
            passThroughs.append((index, bottom.count, lane.colorIndex))
            bottom.append(lane)
        }
        return (bottom, passThroughs)
    }

    /// Connects the commit to its other parents: a lane already expecting one is reused, and
    /// each remaining parent not yet emitted opens a new rightmost lane.
    private mutating func attachParents(
        _ parents: ArraySlice<Oid>, commitLane: Int, bottom: inout [Lane], edges: inout [GraphEdge]
    ) {
        for parent in parents where !emitted.contains(parent) {
            if let existing = bottom.firstIndex(where: { $0.expected == parent }) {
                edges.append(
                    GraphEdge(
                        from: commitLane, to: existing,
                        colorIndex: bottom[existing].colorIndex, phase: .out))
            } else {
                let lane = Lane(expected: parent, colorIndex: takeColor())
                bottom.append(lane)
                edges.append(
                    GraphEdge(
                        from: commitLane, to: bottom.count - 1, colorIndex: lane.colorIndex,
                        phase: .out))
            }
        }
    }
}
