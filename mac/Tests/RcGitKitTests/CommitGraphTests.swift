import Testing

@testable import RcGitKit

/// Lane assignment for the history graph: linear chains, branches, merges, octopus merges,
/// closing roots, and append stability.
@Suite struct CommitGraphTests {
    let r = oid(0x01)
    let a = oid(0x0A)
    let b = oid(0x0B)
    let c = oid(0x0C)
    let m = oid(0x0D)

    private func build(_ commits: [CommitSummary]) -> GraphBuilder {
        var builder = GraphBuilder()
        builder.append(commits[...])
        return builder
    }

    /// Lanes a row touches: its dot and both ends of every edge.
    private func lanesTouched(_ row: GraphRow) -> Int {
        let edgeLanes = row.edges.flatMap { [$0.from, $0.to] }
        return max(row.commitLane, edgeLanes.max() ?? 0) + 1
    }

    @Test func linearChainStaysInOneLane() {
        let rows = build([
            commitSummary(a, parents: [b]),
            commitSummary(b, parents: [r]),
            commitSummary(r),
        ]).rows
        #expect(rows.map(\.commitLane) == [0, 0, 0])
        #expect(rows.map(\.colorIndex) == [0, 0, 0])
        #expect(rows.map(lanesTouched) == [1, 1, 1])
        #expect(rows.allSatisfy { !$0.isMerge })
        #expect(rows[2].edges == [GraphEdge(from: 0, to: 0, colorIndex: 0, phase: .into)])
    }

    @Test func branchAndMergeForkAndRejoin() {
        let rows = build([
            commitSummary(m, parents: [a, b]),
            commitSummary(a, parents: [r]),
            commitSummary(b, parents: [r]),
            commitSummary(r),
        ]).rows
        #expect(rows.map(\.commitLane) == [0, 0, 1, 0])
        #expect(rows.map(\.colorIndex) == [0, 0, 1, 0])
        #expect(rows.map(\.isMerge) == [true, false, false, false])
        #expect(rows.map(lanesTouched) == [2, 2, 2, 2])
        #expect(
            rows[1].edges.contains(GraphEdge(from: 1, to: 1, colorIndex: 1, phase: .through)),
            "b's lane stays right of a's continuing lane")
        let joins = rows[3].edges.filter { $0.phase == .into }
        #expect(
            Set(joins) == [
                GraphEdge(from: 0, to: 0, colorIndex: 0, phase: .into),
                GraphEdge(from: 1, to: 0, colorIndex: 1, phase: .into),
            ], "both lanes merge into the shared root")
    }

    @Test func octopusMergeOpensALanePerExtraParent() {
        let rows = build([
            commitSummary(m, parents: [a, b, c]),
            commitSummary(a),
            commitSummary(b),
            commitSummary(c),
        ]).rows
        #expect(rows[0].isMerge)
        #expect(lanesTouched(rows[0]) == 3)
        #expect(rows[0].edges.filter { $0.phase == .out }.count == 3)
        #expect(
            rows.map(\.commitLane) == [0, 0, 0, 0],
            "each root closes its lane, so the survivors keep sliding left")
        #expect(rows.map(\.colorIndex) == [0, 0, 1, 2])
    }

    @Test func aClosingRootFreesItsLane() {
        let rows = build([
            commitSummary(m, parents: [a, b]),
            commitSummary(a),
            commitSummary(b),
        ]).rows
        #expect(rows[1].commitLane == 0)
        #expect(
            rows[1].edges.contains(GraphEdge(from: 1, to: 0, colorIndex: 1, phase: .through)),
            "the surviving lane slides left past the closed one")
        #expect(rows[2].commitLane == 0, "the second root lands in the freed lane")
        #expect(rows[2].colorIndex == 1, "and keeps its own color")
    }

    @Test func appendingInPagesNeverChangesEarlierRows() {
        let commits = [
            commitSummary(m, parents: [a, b]),
            commitSummary(a, parents: [r]),
            commitSummary(b, parents: [r]),
            commitSummary(r),
        ]
        let whole = build(commits)
        var paged = GraphBuilder()
        paged.append(commits[..<2])
        let firstRows = paged.rows
        paged.append(commits[2...])
        #expect(paged.rows == whole.rows, "two appends equal one build")
        #expect(Array(paged.rows.prefix(2)) == firstRows, "earlier rows never mutate")
        #expect(paged.maxLaneCount == whole.maxLaneCount)
    }

    @Test func aParentEmittedBeforeItsChildOpensNoLane() throws {
        let (c1, c2, p, q, y) = (oid(0x21), oid(0x22), oid(0x23), oid(0x24), oid(0x25))
        let rows = build([
            commitSummary(m, parents: [c1, c2]),
            commitSummary(c1, parents: [p]),
            commitSummary(p, parents: [q]),
            commitSummary(c2, parents: [p]),
            commitSummary(q, parents: [y]),
            commitSummary(y),
        ]).rows
        #expect(lanesTouched(rows[4]) == 1, "C2's edge to the emitted P ends at its dot")
        #expect(rows[4].edges.allSatisfy { $0.phase != .through })
        let last = try #require(rows.last)
        #expect(last.edges.allSatisfy { $0.phase != .through }, "no lane leaks to the end")
    }

    @Test func aRootEmittedBeforeItsChildOpensNoLane() throws {
        let unrelated = oid(0x26)
        let rows = build([
            commitSummary(m, parents: [b, c]),
            commitSummary(b, parents: [a]),
            commitSummary(a),
            commitSummary(c, parents: [a]),
            commitSummary(unrelated),
        ]).rows
        let last = try #require(rows.last)
        #expect(lanesTouched(last) == 1)
        #expect(last.edges.allSatisfy { $0.phase != .through })
    }

    @Test func aMergeParentAlreadyExpectedReusesItsLane() {
        let x = oid(0x27)
        let builder = build([
            commitSummary(x, parents: [b]),
            commitSummary(m, parents: [a, b]),
            commitSummary(a, parents: [r]),
            commitSummary(b, parents: [r]),
            commitSummary(r),
        ])
        #expect(
            builder.rows[1].edges.filter { $0.phase == .out } == [
                GraphEdge(from: 1, to: 1, colorIndex: 1, phase: .out),
                GraphEdge(from: 1, to: 0, colorIndex: 0, phase: .out),
            ], "m's edge to b joins x's lane instead of opening a third")
        #expect(lanesTouched(builder.rows[1]) == 2)
        #expect(builder.maxLaneCount == 2)

        let (e, d) = (oid(0x28), oid(0x29))
        let nested = build([
            commitSummary(e, parents: [b, d]),
            commitSummary(d, parents: [c, b]),
            commitSummary(c, parents: [a]),
            commitSummary(b, parents: [a]),
            commitSummary(a),
        ]).rows
        #expect(nested[1].edges.contains(GraphEdge(from: 1, to: 0, colorIndex: 0, phase: .out)))
        #expect(nested.map(lanesTouched).max() == 2, "no third lane opens for b")
    }

    @Test func drawnEdgesStrokeOverflowLanesOnceInTheLastColumn() throws {
        let edges = (0..<30).map { GraphEdge(from: $0, to: $0, colorIndex: $0, phase: .through) }
        let row = GraphRow(
            commitLane: 0, colorIndex: 0, edges: edges, isMerge: false)
        let cap = GraphRow.maxDrawnLanes
        let drawn = row.drawnEdges(laneCap: cap)
        #expect(drawn.count == cap, "one stroke per drawn column")
        #expect(Array(drawn.prefix(cap - 1)) == Array(edges.prefix(cap - 1)), "under the cap")
        let overflow = try #require(drawn.last)
        #expect(
            overflow == GraphEdge(from: cap - 1, to: cap - 1, colorIndex: 29, phase: .through),
            "the last overflow edge is the one painted on top")
    }

    @Test func drawnEdgesKeepDistinctSegmentsInTheLastColumn() {
        let edges = [
            GraphEdge(from: 14, to: 13, colorIndex: 1, phase: .through),
            GraphEdge(from: 20, to: 20, colorIndex: 2, phase: .out),
            GraphEdge(from: 15, to: 15, colorIndex: 3, phase: .through),
            GraphEdge(from: 2, to: 30, colorIndex: 4, phase: .out),
        ]
        let row = GraphRow(
            commitLane: 2, colorIndex: 0, edges: edges, isMerge: false)
        #expect(
            row.drawnEdges(laneCap: 12) == [
                GraphEdge(from: 11, to: 11, colorIndex: 2, phase: .out),
                GraphEdge(from: 11, to: 11, colorIndex: 3, phase: .through),
                GraphEdge(from: 2, to: 11, colorIndex: 4, phase: .out),
            ], "phase and endpoints tell segments apart; color does not")
    }

    @Test func truncationLeavesLanesOpen() {
        let builder = build([commitSummary(m, parents: [a, b])])
        #expect(lanesTouched(builder.rows[0]) == 2, "both parent lanes stay open past the last row")
        #expect(builder.maxLaneCount == 2)
    }
}
