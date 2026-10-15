import AppKit
import RcGitKit
import Testing

@testable import RcGitApp

/// The commit list: the one table in the view that is not the changes outline.
@MainActor
private func commitTable(in view: NSView) -> NSTableView? {
    if let table = view as? NSTableView, !(table is NSOutlineView) { return table }
    for subview in view.subviews {
        if let found = commitTable(in: subview) { return found }
    }
    return nil
}

private func commit(_ byte: UInt8, title: String) -> CommitSummary {
    CommitSummary(
        oid: Oid(bytes: [UInt8](repeating: byte, count: 20)), parents: [], authorName: "A",
        authorEmail: "a@example.com", authorTime: 1_704_067_200, authorOffset: 0,
        committerTime: 1_704_067_200, title: title)
}

@MainActor
@Suite(.timeLimit(.minutes(1))) struct HistoryViewControllerTests {
    private let first = commit(0x11, title: "fix one")
    private let second = commit(0x22, title: "fix two")

    private func renderSearch(_ controller: HistoryViewController, detail: Oid?) {
        controller.renderSearch(
            filter: CommitFilter(message: "fix"), results: [first, second], scanned: 2,
            complete: true, searching: false, error: nil, detail: detail)
    }

    @Test func aSearchKeepsTheDetailCommitSelected() throws {
        let controller = HistoryViewController()
        let table = try #require(commitTable(in: controller.view))
        var picked: [CommitSummary] = []
        controller.onSelectCommit = { picked.append($0) }

        renderSearch(controller, detail: second.oid)

        #expect(table.selectedRow == 1)
        #expect(picked.isEmpty)
    }

    @Test func aDetailCommitTheSearchMissesLeavesNoRowSelected() throws {
        let controller = HistoryViewController()
        let table = try #require(commitTable(in: controller.view))

        renderSearch(controller, detail: commit(0x33, title: "other").oid)

        #expect(table.selectedRow == -1)
    }

    @Test func noDetailCommitLeavesNoRowSelected() throws {
        let controller = HistoryViewController()
        let table = try #require(commitTable(in: controller.view))

        renderSearch(controller, detail: nil)

        #expect(table.selectedRow == -1)
    }
}
