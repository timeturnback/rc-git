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

    @Test func linearChainStaysInOneLane() {
        let rows = build([
            commitSummary(a, parents: [b]),
            commitSummary(b, parents: [r]),
            commitSummary(r),
        ]).rows
        #expect(rows.map(\.commitLane) == [0, 0, 0])
        #expect(rows.map(\.colorIndex) == [0, 0, 0])
        #expect(rows.map(\.laneCount) == [1, 1, 1])
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
        #expect(rows.map(\.laneCount) == [2, 2, 2, 2])
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
        #expect(rows[0].laneCount == 3)
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
        #expect(rows[4].laneCount == 1, "C2's edge to the emitted P ends at its dot")
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
        #expect(last.laneCount == 1)
        #expect(last.edges.allSatisfy { $0.phase != .through })
    }

    @Test func truncationLeavesLanesOpen() {
        let builder = build([commitSummary(m, parents: [a, b])])
        #expect(builder.rows[0].laneCount == 2, "both parent lanes stay open past the last row")
        #expect(builder.maxLaneCount == 2)
    }
}
