import AppKit
import RcGitKit
import Testing

@testable import RcGitApp

@MainActor
private func outline(in view: NSView) -> NSOutlineView? {
    if let outline = view as? NSOutlineView { return outline }
    for subview in view.subviews {
        if let found = outline(in: subview) { return found }
    }
    return nil
}

/// The label of each top-level row, as the outline would draw it.
@MainActor
private func topLabels(_ controller: RefsViewController) throws -> [String] {
    let list = try #require(outline(in: controller.view))
    let count = controller.outlineView(list, numberOfChildrenOfItem: nil)
    return (0..<count).map { index in
        let item = controller.outlineView(list, child: index, ofItem: nil)
        let cell = controller.outlineView(list, viewFor: nil, item: item)
        if let cell = cell as? SidebarCellView { return cell.label.stringValue }
        return (cell as? NSTableCellView)?.textField?.stringValue ?? ""
    }
}

@MainActor
@Suite(.timeLimit(.minutes(1))) struct RefsViewControllerTests {
    private let listing = RefListing(refs: [
        RefEntry(
            fullName: "refs/heads/main", shortName: "main", kind: .localBranch,
            target: Oid(bytes: [UInt8](repeating: 0x11, count: 20)))
    ])

    private func reload(_ controller: RefsViewController, stale: Bool) {
        controller.reload(
            listing, selected: nil, head: nil, showsWorkingTree: true,
            workingTreeSelected: false, stale: stale)
    }

    @Test func staleRefsShowANoteAboveTheList() throws {
        let controller = RefsViewController()
        reload(controller, stale: true)

        let labels = try topLabels(controller)

        #expect(labels.first == BranchBanner.staleRefsDetail)
        #expect(labels.dropFirst().first == "Working Tree")
    }

    @Test func theNoteIsNeverPicked() throws {
        let controller = RefsViewController()
        reload(controller, stale: true)
        let list = try #require(outline(in: controller.view))

        let note = controller.outlineView(list, child: 0, ofItem: nil)

        #expect(!controller.outlineView(list, shouldSelectItem: note))
    }

    @Test func freshRefsDropTheNote() throws {
        let controller = RefsViewController()
        reload(controller, stale: true)
        reload(controller, stale: false)

        let labels = try topLabels(controller)

        #expect(!labels.contains(BranchBanner.staleRefsDetail))
        #expect(labels.first == "Working Tree")
    }
}
