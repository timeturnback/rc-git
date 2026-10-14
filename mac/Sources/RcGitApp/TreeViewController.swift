import AppKit
import RcGitKit

/// The directory tree of the selected ref, or of the working directory with its status
/// overlay. Children load when a folder expands, one tree object (or one directory read)
/// per expand, so the cost does not depend on repository size.
@MainActor
final class TreeViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate {
    var childrenProvider: ((Oid, String) async throws -> [BrowseRow])?
    /// The row, its path, and whether every component of that path is exact
    /// (`TreeEntry.hasExactName`); the model refuses path actions on an inexact one.
    var onOpenFile: ((BrowseRow, String, Bool) -> Void)?
    var onShowHistory: ((String, Bool) -> Void)?
    var onCopyPath: ((String, Bool) -> Void)?
    var onError: ((String) -> Void)?
    /// Fires with `contentWidth` whenever the visible rows change, so the pane can hug them.
    var onContentWidthChanged: ((CGFloat) -> Void)?
    /// Whether the pane would use a width now; when it says no, no row is measured.
    var shouldReportContentWidth: (() -> Bool)?
    /// Whether a row is on screen; the model decides, from the hidden-files setting.
    var isRowVisible: ((BrowseRow) -> Bool)?

    final class Node {
        let row: BrowseRow
        let path: String
        /// Whether every component of `path` is the stored name byte for byte.
        let pathIsExact: Bool
        /// Every child read from the repository, hidden ones included.
        var children: [Node]?
        /// The children on screen, kept beside `children` so showing hidden files needs no read.
        var visibleChildren: [Node]?
        var isLoading = false
        /// What the cell draws, typeset once per row size: `row` never changes.
        var measuredWidth: (rowSize: NSTableView.RowSizeStyle, width: CGFloat)?

        var entry: TreeEntry { row.entry }

        init(row: BrowseRow, parent: Node?) {
            self.row = row
            let parentPath = parent?.path ?? ""
            self.path = parentPath.isEmpty ? row.entry.name : parentPath + "/" + row.entry.name
            self.pathIsExact = (parent?.pathIsExact ?? true) && row.entry.hasExactName
        }
    }

    private let outline = NSOutlineView()
    private let scroll = NSScrollView()
    private static let unreadableTreeText = "Could not read this tree"
    private let placeholder = NSTextField(
        labelWithString: TreePlaceholder.noRepository)
    private var allRoots: [Node] = []
    private var roots: [Node] = []
    private var generation = 0
    /// Set while the selection is moved from here rather than by a click, so re-selecting
    /// the open file does not open it again.
    private var restoringSelection = false
    /// The path `showOpenFile` last matched the selection to, and whether it is exact.
    private var shownFilePath: String?
    private var shownPathIsExact = true
    /// The widest cell content plus its x offset over the rows on the outline, kept up as
    /// rows arrive, and the row size it was measured at; `nil` when it has to be recomputed
    /// from every row.
    private var widestRow: (rowSize: NSTableView.RowSizeStyle, width: CGFloat)?
    /// Set while `refilter` re-expands folders, which re-measures every row once at the end.
    private var restoringExpansion = false

    override func loadView() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.dataSource = self
        outline.delegate = self
        Style.sidebarList(outline)
        outline.indentationPerLevel = Style.folderIndent
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

    /// Replaces the whole tree with the root of another ref or commit, or with `placeholder`
    /// (the model's `treePlaceholder`) when there is no root.
    func reload(root: Oid?, placeholder text: String) {
        generation += 1
        allRoots = []
        roots = []
        outline.reloadData()
        guard let root else {
            placeholder.stringValue = text
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
    /// Every row in an expanded folder counts, on screen or scrolled off, so each row's text
    /// is typeset once and cached; `below` limits the pass to the rows a folder just showed,
    /// and nothing is measured while the pane would not use the width.
    private func reportContentWidth(below folder: Node? = nil) {
        guard let onContentWidthChanged else { return }
        guard shouldReportContentWidth?() ?? true else {
            widestRow = nil
            return
        }
        let rowSize = outline.effectiveRowSizeStyle
        let widest: CGFloat
        if let folder, let known = widestRow, known.rowSize == rowSize {
            widest = max(known.width, widestCell(in: rowsShown(below: folder), rowSize: rowSize))
        } else {
            widest = widestCell(in: 0..<outline.numberOfRows, rowSize: rowSize)
        }
        widestRow = (rowSize, widest)
        let scroller =
            scroll.scrollerStyle == .legacy
            ? NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy) : 0
        onContentWidthChanged(widest + Self.cellTrailingInset + scroller)
    }

    /// The rows of `folder`'s subtree on the outline: everything after its row that sits
    /// deeper than it. Empty when the folder is off the outline or collapsed.
    private func rowsShown(below folder: Node) -> Range<Int> {
        let row = outline.row(forItem: folder)
        guard row >= 0, outline.isItemExpanded(folder) else { return 0..<0 }
        let level = outline.level(forRow: row)
        var end = row + 1
        while end < outline.numberOfRows, outline.level(forRow: end) > level {
            end += 1
        }
        return (row + 1)..<end
    }

    /// The widest cell start plus content over `rows`. A cell starts one indentation step
    /// further in per level, so one row's frame gives every row's start.
    private func widestCell(in rows: Range<Int>, rowSize: NSTableView.RowSizeStyle) -> CGFloat {
        guard let first = rows.first else { return 0 }
        let indent = outline.indentationPerLevel
        let base =
            outline.frameOfCell(atColumn: 0, row: first).minX
            - CGFloat(outline.level(forRow: first)) * indent
        var widest: CGFloat = 0
        for row in rows {
            guard let node = outline.item(atRow: row) as? Node else { continue }
            let start = base + CGFloat(outline.level(forRow: row)) * indent
            widest = max(widest, start + contentWidth(of: node, rowSize: rowSize))
        }
        return widest
    }

    private func contentWidth(of node: Node, rowSize: NSTableView.RowSizeStyle) -> CGFloat {
        if let measured = node.measuredWidth, measured.rowSize == rowSize {
            return measured.width
        }
        let width = SidebarCellView.contentWidth(
            label: node.entry.name, badge: Self.badge(for: node.row)?.letter, rowSize: rowSize)
        node.measuredWidth = (rowSize, width)
        return width
    }

    private func load(_ tree: Oid, path: String, into parent: Node?) {
        guard let childrenProvider else { return }
        let generation = generation
        parent?.isLoading = true
        Task {
            do {
                let rows = try await childrenProvider(tree, path)
                guard generation == self.generation else { return }
                let nodes = rows.map { Node(row: $0, parent: parent) }
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
                reportContentWidth(below: parent)
            } catch {
                guard generation == self.generation else { return }
                showLoadFailure(in: parent)
                onError?(String(describing: error))
            }
        }
    }

    /// Keeps a failed load from reading as an empty folder or tree. A folder collapses with
    /// `children` still nil, so expanding it again retries; a root shows the failure where
    /// the rows would be.
    private func showLoadFailure(in parent: Node?) {
        guard let parent else {
            placeholder.stringValue = Self.unreadableTreeText
            placeholder.isHidden = false
            return
        }
        parent.isLoading = false
        if outline.row(forItem: parent) >= 0 {
            outline.collapseItem(parent)
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
        restoringExpansion = true
        for node in expanded where outline.row(forItem: node) >= 0 {
            outline.expandItem(node)
        }
        restoringExpansion = false
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
    /// An inexact path is never matched: its spelling may be a different row's.
    func showOpenFile(_ path: String?, pathIsExact: Bool) {
        guard path != shownFilePath || pathIsExact != shownPathIsExact else { return }
        shownFilePath = path
        shownPathIsExact = pathIsExact
        matchSelectionToShownFile()
    }

    /// Selects the open file's row when it is on the outline, and deselects a file row that
    /// is not the open file. Reads nothing and expands nothing; a selected folder stays
    /// unless the open file has a row to move to.
    private func matchSelectionToShownFile() {
        guard shownPathIsExact else { return }
        let selected = outline.item(atRow: outline.selectedRow) as? Node
        guard selected?.path != shownFilePath || selected?.pathIsExact == false else { return }
        let row = shownFilePath.flatMap { path in
            (0..<outline.numberOfRows).first {
                guard let node = outline.item(atRow: $0) as? Node else { return false }
                return node.pathIsExact && node.path == path
            }
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
        onShowHistory?(node.path, node.pathIsExact)
    }

    @objc private func copyPath(_ sender: Any?) {
        guard let node = actedNode else { return }
        onCopyPath?(node.path, node.pathIsExact)
    }

    @objc private func toggleSelected(_ sender: Any?) {
        // Files open through selection alone; a double-click re-opening one resets the pane.
        guard let node = outline.item(atRow: outline.clickedRow) as? Node,
            node.entry.kind.isDirectory, !node.row.missing
        else { return }
        if outline.isItemExpanded(node) {
            outline.collapseItem(node)
        } else {
            outline.expandItem(node)
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
        // A folder still loading measures its rows once they land, in `load`.
        guard !restoringExpansion, let node = notification.userInfo?["NSObject"] as? Node,
            node.children != nil
        else { return }
        reportContentWidth(below: node)
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
        guard let status = row.missing ? .deleted : row.status, let letter = status.letter
        else { return nil }
        return (letter, StatusPalette.color(for: status))
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !restoringSelection,
            let node = outline.item(atRow: outline.selectedRow) as? Node,
            !node.entry.kind.isDirectory
        else { return }
        onOpenFile?(node.row, node.path, node.pathIsExact)
    }
}
