import AppKit
import RcGitKit

/// Sidebar listing the working tree, branches, remote branches, and tags, with a filter field.
@MainActor
final class RefsViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate,
    NSSearchFieldDelegate, NSMenuDelegate
{
    /// Mutating requests from the context menu; the window controller runs them through the
    /// guard and its confirmation flow.
    enum RefMenuAction {
        case newBranch
        case newTag
        case deleteBranch
        case deleteTag
    }

    var onSelect: ((RefEntry) -> Void)?
    var onSelectWorkingTree: (() -> Void)?
    var onRefAction: ((RefMenuAction, RefEntry) -> Void)?

    private final class Section {
        let title: String
        let refs: [RefEntry]
        init(title: String, refs: [RefEntry]) {
            self.title = title
            self.refs = refs
        }
    }

    /// The one pseudo-row above the sections: the files on disk.
    private final class WorkingTreeRow {}

    private let outline = NSOutlineView()
    private let search = NSSearchField()
    private var sections: [Section] = []
    private var listing = RefListing(refs: [])
    private var head: Head?
    private var selected: RefEntry?
    private let workingTreeRow = WorkingTreeRow()
    private var showsWorkingTree = false
    private var workingTreeSelected = false
    private var isReloading = false

    override func loadView() {
        search.placeholderString = "Filter refs"
        search.delegate = self
        search.controlSize = .small
        search.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("ref"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.dataSource = self
        outline.delegate = self
        outline.style = .sourceList
        outline.floatsGroupRows = false
        outline.rowSizeStyle = .default
        outline.autoresizesOutlineColumn = true
        outline.indentationPerLevel = 8
        let menu = NSMenu()
        menu.delegate = self
        outline.menu = menu

        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.autohidesScrollers = true

        let stack = NSStackView(views: [search, scroll])
        stack.orientation = .vertical
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(top: 8, left: 8, bottom: 0, right: 8)
        stack.setHuggingPriority(.defaultLow, for: .vertical)
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)
        stack.setVisibilityPriority(.mustHold, for: scroll)
        view = stack
    }

    func reload(
        _ listing: RefListing, selected: RefEntry?, head: Head?,
        showsWorkingTree: Bool = false, workingTreeSelected: Bool = false
    ) {
        self.listing = listing
        self.selected = selected
        self.head = head
        self.showsWorkingTree = showsWorkingTree
        self.workingTreeSelected = workingTreeSelected
        rebuild()
    }

    private func rebuild() {
        let filter = search.stringValue.trimmingCharacters(in: .whitespaces).lowercased()
        func matching(_ kind: RefKind) -> [RefEntry] {
            listing.refs
                .filter {
                    $0.kind == kind
                        && (filter.isEmpty || $0.shortName.lowercased().contains(filter))
                }
                .sorted { $0.shortName.localizedStandardCompare($1.shortName) == .orderedAscending }
        }
        sections = [
            Section(title: "Branches", refs: matching(.localBranch)),
            Section(title: "Remotes", refs: matching(.remoteBranch)),
            Section(title: "Tags", refs: matching(.tag)),
            Section(title: "Other", refs: matching(.other)),
        ].filter { !$0.refs.isEmpty }

        isReloading = true
        outline.reloadData()
        outline.expandItem(nil, expandChildren: true)
        if workingTreeSelected, showsWorkingTree {
            let row = outline.row(forItem: workingTreeRow)
            if row >= 0 {
                outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            }
        } else if let selected, let row = rowOf(selected) {
            outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else {
            outline.deselectAll(nil)
        }
        isReloading = false
    }

    private func rowOf(_ ref: RefEntry) -> Int? {
        for section in sections {
            if let entry = section.refs.first(where: { $0.fullName == ref.fullName }) {
                let row = outline.row(forItem: entry)
                return row >= 0 ? row : nil
            }
        }
        return nil
    }

    // MARK: Context menu

    private var clickedRef: RefEntry? {
        outline.item(atRow: outline.clickedRow) as? RefEntry
    }

    @objc private func newBranchClicked(_ sender: Any?) {
        if let ref = clickedRef { onRefAction?(.newBranch, ref) }
    }

    @objc private func newTagClicked(_ sender: Any?) {
        if let ref = clickedRef { onRefAction?(.newTag, ref) }
    }

    @objc private func deleteBranchClicked(_ sender: Any?) {
        if let ref = clickedRef { onRefAction?(.deleteBranch, ref) }
    }

    @objc private func deleteTagClicked(_ sender: Any?) {
        if let ref = clickedRef { onRefAction?(.deleteTag, ref) }
    }

    private func menuItem(_ title: String, _ action: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        return item
    }

    // MARK: NSSearchFieldDelegate

    func controlTextDidChange(_ notification: Notification) {
        rebuild()
    }

    // MARK: NSMenuDelegate

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard let ref = clickedRef else { return }
        menu.addItem(
            menuItem(
                "New Branch from “\(ref.shortName)”…", #selector(newBranchClicked(_:))))
        menu.addItem(
            menuItem("New Tag at “\(ref.shortName)”…", #selector(newTagClicked(_:))))
        switch ref.kind {
        case .localBranch:
            menu.addItem(.separator())
            menu.addItem(
                menuItem(
                    "Delete Branch “\(ref.shortName)”…", #selector(deleteBranchClicked(_:))))
        case .tag:
            menu.addItem(.separator())
            menu.addItem(
                menuItem("Delete Tag “\(ref.shortName)”…", #selector(deleteTagClicked(_:))))
        case .remoteBranch, .other:
            break
        }
    }

    // MARK: NSOutlineViewDataSource

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        if item == nil { return sections.count + (showsWorkingTree ? 1 : 0) }
        if let section = item as? Section { return section.refs.count }
        return 0
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        if let section = item as? Section { return section.refs[index] }
        if showsWorkingTree {
            return index == 0 ? workingTreeRow : sections[index - 1]
        }
        return sections[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        item is Section
    }

    // MARK: NSOutlineViewDelegate

    func outlineView(_ outlineView: NSOutlineView, isGroupItem item: Any) -> Bool {
        item is Section
    }

    func outlineView(_ outlineView: NSOutlineView, shouldSelectItem item: Any) -> Bool {
        item is RefEntry || item is WorkingTreeRow
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any)
        -> NSView?
    {
        if let section = item as? Section {
            let cell = reusableCell(identifier: "header", image: false)
            cell.textField?.stringValue = section.title
            return cell
        }
        if item is WorkingTreeRow {
            let cell = reusableCell(identifier: "ref", image: true)
            cell.imageView?.image = NSImage(
                systemSymbolName: "internaldrive", accessibilityDescription: nil)
            cell.textField?.stringValue = "Working Tree"
            cell.textField?.font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
            cell.toolTip = "Files on disk, compared with HEAD"
            return cell
        }
        guard let ref = item as? RefEntry else { return nil }
        let cell = reusableCell(identifier: "ref", image: true)
        cell.imageView?.image = Icons.symbol(for: ref.kind)
        cell.textField?.stringValue = ref.shortName
        let isHead = head?.refName == ref.fullName
        cell.textField?.font =
            isHead
            ? NSFont.boldSystemFont(ofSize: NSFont.systemFontSize)
            : NSFont.systemFont(ofSize: NSFont.systemFontSize)
        cell.toolTip = ref.fullName + "\n" + ref.target.hex
        return cell
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading else { return }
        if outline.item(atRow: outline.selectedRow) is WorkingTreeRow {
            guard !workingTreeSelected else { return }
            workingTreeSelected = true
            selected = nil
            onSelectWorkingTree?()
            return
        }
        guard let ref = outline.item(atRow: outline.selectedRow) as? RefEntry else { return }
        guard ref.fullName != selected?.fullName || workingTreeSelected else { return }
        workingTreeSelected = false
        selected = ref
        onSelect?(ref)
    }

    private func reusableCell(identifier: String, image: Bool) -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier(identifier)
        if let cell = outline.makeView(withIdentifier: id, owner: self) as? NSTableCellView {
            return cell
        }
        let cell = NSTableCellView()
        cell.identifier = id
        let label = NSTextField(labelWithString: "")
        label.lineBreakMode = .byTruncatingMiddle
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(label)
        cell.textField = label
        var leading: NSLayoutXAxisAnchor = cell.leadingAnchor
        var inset: CGFloat = 4
        if image {
            let imageView = NSImageView()
            imageView.translatesAutoresizingMaskIntoConstraints = false
            imageView.symbolConfiguration = NSImage.SymbolConfiguration(
                pointSize: 13, weight: .regular)
            cell.addSubview(imageView)
            cell.imageView = imageView
            NSLayoutConstraint.activate([
                imageView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                imageView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                imageView.widthAnchor.constraint(equalToConstant: 18),
            ])
            leading = imageView.trailingAnchor
            inset = 5
        }
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leading, constant: inset),
            label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}
