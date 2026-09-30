import AppKit
import RcGitKit

/// The directory tree of the selected ref, or of the working directory with its status
/// overlay. Children load when a folder expands, one tree object (or one directory read)
/// per expand, so the cost does not depend on repository size.
@MainActor
final class TreeViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate {
    var childrenProvider: ((Oid, String) async throws -> [BrowseRow])?
    var onOpenFile: ((BrowseRow, String) -> Void)?
    var onShowHistory: ((String) -> Void)?
    var onError: ((String) -> Void)?
    /// Fires with `contentWidth` whenever the visible rows change, so the pane can hug them.
    var onContentWidthChanged: ((CGFloat) -> Void)?
    /// Whether a row is on screen; the model decides, from the hidden-files setting.
    var isRowVisible: ((BrowseRow) -> Bool)?

    final class Node {
        let row: BrowseRow
        let path: String
        /// Every child read from the repository, hidden ones included.
        var children: [Node]?
        /// The children on screen, kept beside `children` so showing hidden files needs no read.
        var visibleChildren: [Node]?
        var isLoading = false

        var entry: TreeEntry { row.entry }

        init(row: BrowseRow, parentPath: String) {
            self.row = row
            self.path = parentPath.isEmpty ? row.entry.name : parentPath + "/" + row.entry.name
        }
    }

    private let outline = NSOutlineView()
    private let scroll = NSScrollView()
    private let placeholder = NSTextField(labelWithString: "Open a repository (⌘O)")
    private var allRoots: [Node] = []
    private var roots: [Node] = []
    private var generation = 0
    /// Set while the selection is moved from here rather than by a click, so re-selecting
    /// the open file does not open it again.
    private var restoringSelection = false
    /// The path `showOpenFile` last matched the selection to.
    private var shownFilePath: String?

    override func loadView() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.dataSource = self
        outline.delegate = self
        Style.sidebarList(outline)
        outline.indentationPerLevel = 14
        outline.autoresizesOutlineColumn = true
        outline.allowsEmptySelection = true
        outline.target = self
        outline.doubleAction = #selector(toggleSelected(_:))
        outline.menu = buildMenu()

        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true

        placeholder.textColor = .secondaryLabelColor
        placeholder.alignment = .center
        placeholder.translatesAutoresizingMaskIntoConstraints = false

        let container = NSView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(scroll)
        container.addSubview(placeholder)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: container.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            placeholder.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: container.centerYAnchor),
            placeholder.leadingAnchor.constraint(
                greaterThanOrEqualTo: container.leadingAnchor, constant: 12),
        ])
        view = container
    }

    /// Replaces the whole tree with the root of another ref or commit.
    func reload(root: Oid?) {
        generation += 1
        allRoots = []
        roots = []
        outline.reloadData()
        guard let root else {
            placeholder.isHidden = false
            reportContentWidth()
            return
        }
        placeholder.isHidden = true
        load(root, path: "", into: nil)
    }

    /// Between a cell's trailing edge and the list's, once the column fills the list: the
    /// source list's `Style.margin` row inset plus the padding it keeps inside the selection.
    /// Measured with `frameOfCell(atColumn:row:)`; AppKit does not publish it.
    private static let cellTrailingInset: CGFloat = 16

    /// The width that shows every row currently on the outline in full: where each row's cell
    /// starts — past the source list's inset, the indentation, and the disclosure triangle —
    /// plus what the cell draws, the list's trailing inset, and a scroller that takes space.
    /// Only expanded rows are measured, so the cost follows what is on screen, not repository
    /// size.
    private func reportContentWidth() {
        guard let onContentWidthChanged else { return }
        let rowSize = outline.effectiveRowSizeStyle
        var widest: CGFloat = 0
        for row in 0..<outline.numberOfRows {
            guard let node = outline.item(atRow: row) as? Node else { continue }
            let content = SidebarCellView.contentWidth(
                label: node.entry.name, badge: Self.badge(for: node.row)?.letter,
                rowSize: rowSize)
            widest = max(widest, outline.frameOfCell(atColumn: 0, row: row).minX + content)
        }
        let scroller =
            scroll.scrollerStyle == .legacy
            ? NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy) : 0
        onContentWidthChanged(widest + Self.cellTrailingInset + scroller)
    }

    private func load(_ tree: Oid, path: String, into parent: Node?) {
        guard let childrenProvider else { return }
        let generation = generation
        parent?.isLoading = true
        Task {
            do {
                let rows = try await childrenProvider(tree, path)
                guard generation == self.generation else { return }
                let nodes = rows.map { Node(row: $0, parentPath: parent?.path ?? "") }
                if let parent {
                    parent.children = nodes
                    parent.visibleChildren = visible(nodes)
                    parent.isLoading = false
                    outline.reloadItem(parent, reloadChildren: true)
                } else {
                    allRoots = nodes
                    roots = visible(nodes)
                    outline.reloadData()
                }
                // Rows that just arrived may hold the open file: a reload drops the selection.
                if outline.selectedRow < 0 {
                    matchSelectionToShownFile()
                }
                reportContentWidth()
            } catch {
                guard generation == self.generation else { return }
                parent?.isLoading = false
                onError?(String(describing: error))
            }
        }
    }

    private func visible(_ nodes: [Node]) -> [Node] {
        guard let isRowVisible else { return nodes }
        return nodes.filter { isRowVisible($0.row) }
    }

    /// Re-applies `isRowVisible` to every row already read, after the hidden-files setting
    /// changed. Reads nothing; expanded folders stay expanded and the selection stays put
    /// when its row is still on screen.
    func refilter() {
        let expanded = (0..<outline.numberOfRows).compactMap { row -> Node? in
            guard let node = outline.item(atRow: row) as? Node, outline.isItemExpanded(node)
            else { return nil }
            return node
        }
        let selected = outline.item(atRow: outline.selectedRow) as? Node
        roots = visible(allRoots)
        var pending = allRoots
        while let node = pending.popLast() {
            guard let children = node.children else { continue }
            node.visibleChildren = visible(children)
            pending += children
        }
        outline.reloadData()
        for node in expanded where outline.row(forItem: node) >= 0 {
            outline.expandItem(node)
        }
        let selectedRow = selected.map { outline.row(forItem: $0) } ?? -1
        restoringSelection = true
        if selectedRow >= 0 {
            outline.selectRowIndexes(IndexSet(integer: selectedRow), byExtendingSelection: false)
        } else {
            outline.deselectAll(nil)
        }
        restoringSelection = false
        reportContentWidth()
    }

    /// Keeps the selection from naming a file the pane has left — a link followed, a step
    /// back — because a row that is still selected cannot be clicked to open it again.
    func showOpenFile(_ path: String?) {
        guard path != shownFilePath else { return }
        shownFilePath = path
        matchSelectionToShownFile()
    }

    /// Selects the open file's row when it is on the outline, and deselects a file row that
    /// is not the open file. Reads nothing and expands nothing; a selected folder stays
    /// unless the open file has a row to move to.
    private func matchSelectionToShownFile() {
        let selected = outline.item(atRow: outline.selectedRow) as? Node
        guard selected?.path != shownFilePath else { return }
        let row = shownFilePath.flatMap { path in
            (0..<outline.numberOfRows).first { (outline.item(atRow: $0) as? Node)?.path == path }
        }
        restoringSelection = true
        if let row {
            outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else if selected?.entry.kind.isDirectory == false {
            outline.deselectAll(nil)
        }
        restoringSelection = false
    }

    private func buildMenu() -> NSMenu {
        let menu = NSMenu()
        let history = NSMenuItem(
            title: "Show File History", action: #selector(showHistoryClicked(_:)),
            keyEquivalent: "")
        history.target = self
        menu.addItem(history)
        let copy = NSMenuItem(
            title: "Copy Path", action: #selector(copyPath(_:)), keyEquivalent: "")
        copy.target = self
        menu.addItem(copy)
        return menu
    }

    private var actedNode: Node? {
        let row = outline.clickedRow >= 0 ? outline.clickedRow : outline.selectedRow
        return outline.item(atRow: row) as? Node
    }

    @objc private func showHistoryClicked(_ sender: Any?) {
        guard let node = actedNode else { return }
        onShowHistory?(node.path)
    }

    @objc private func copyPath(_ sender: Any?) {
        guard let node = actedNode else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(node.path, forType: .string)
    }

    @objc private func toggleSelected(_ sender: Any?) {
        guard let node = outline.item(atRow: outline.clickedRow) as? Node else { return }
        if node.entry.kind.isDirectory, !node.row.missing {
            if outline.isItemExpanded(node) {
                outline.collapseItem(node)
            } else {
                outline.expandItem(node)
            }
        } else {
            onOpenFile?(node.row, node.path)
        }
    }

    // MARK: NSOutlineViewDataSource

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        guard let node = item as? Node else { return roots.count }
        return node.visibleChildren?.count ?? 0
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let node = item as? Node else { return roots[index] }
        return node.visibleChildren?[index] as Any
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        guard let node = item as? Node else { return false }
        return node.entry.kind.isDirectory && !node.row.missing
    }

    // MARK: NSOutlineViewDelegate

    func outlineViewItemWillExpand(_ notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? Node else { return }
        if node.children == nil, !node.isLoading {
            load(node.entry.oid, path: node.path, into: node)
        }
    }

    func outlineViewItemDidExpand(_ notification: Notification) {
        reportContentWidth()
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        reportContentWidth()
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any)
        -> NSView?
    {
        guard let node = item as? Node else { return nil }
        let id = NSUserInterfaceItemIdentifier("entry")
        let cell =
            outline.makeView(withIdentifier: id, owner: self) as? SidebarCellView
            ?? SidebarCellView(identifier: id)
        cell.imageView?.image = Icons.image(for: node.entry)
        cell.label.stringValue = node.entry.name
        // Ignored files and paths deleted from disk are listed, but read as not really there.
        let dimmed = node.row.missing || node.row.status == .ignored
        cell.label.textColor = dimmed ? .secondaryLabelColor : .labelColor
        let badge = Self.badge(for: node.row)
        cell.setBadge(badge?.letter, color: badge?.color ?? .secondaryLabelColor)
        cell.toolTip = node.path
        return cell
    }

    func outlineView(_ outlineView: NSOutlineView, tintConfigurationForItem item: Any)
        -> NSTintConfiguration?
    {
        (item as? Node).map { Icons.tint(for: $0.entry) }
    }

    func outlineView(
        _ outlineView: NSOutlineView, typeSelectStringFor tableColumn: NSTableColumn?, item: Any
    ) -> String? {
        (item as? Node)?.entry.name
    }

    /// The status letter a working-tree row carries at its trailing edge, if any.
    static func badge(for row: BrowseRow) -> (letter: String, color: NSColor)? {
        if row.missing { return ("D", .systemRed) }
        return switch row.status {
        case .modified: ("M", .systemOrange)
        case .untracked: ("U", .systemGreen)
        case .ignored: ("I", .tertiaryLabelColor)
        case .deleted: ("D", .systemRed)
        case .unchanged, nil: nil
        }
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !restoringSelection,
            let node = outline.item(atRow: outline.selectedRow) as? Node,
            !node.entry.kind.isDirectory
        else { return }
        onOpenFile?(node.row, node.path)
    }
}
