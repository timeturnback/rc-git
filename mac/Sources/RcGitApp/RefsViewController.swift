import AppKit
import RcGitKit

/// The working tree, branches, remote branches, and tags, with a filter field. History shows
/// it in the sidebar; Files shows it in the ref picker popover.
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

    /// How a row gets picked. The sidebar picks as the selection moves. The picker only
    /// highlights as it moves — arrowing down from the filter must not close the sheet — and
    /// picks on a click or Return.
    enum Behavior {
        case sidebar
        case picker
    }

    var onSelect: ((RefEntry) -> Void)?
    var onSelectWorkingTree: (() -> Void)?
    var onRefAction: ((RefMenuAction, RefEntry) -> Void)?
    /// The picker is done with nothing to change: Escape, or a pick of the row already browsed.
    var onClose: (() -> Void)?

    private final class Section {
        let title: String
        let refs: [RefEntry]
        let broken: [BrokenRow]
        init(title: String, refs: [RefEntry], broken: [BrokenRow] = []) {
            self.title = title
            self.refs = refs
            self.broken = broken
        }
    }

    /// A ref whose target could not be read: listed so it does not look missing, never picked.
    private final class BrokenRow {
        let ref: BrokenRef
        init(_ ref: BrokenRef) { self.ref = ref }
    }

    /// The one pseudo-row above the sections: the files on disk.
    private final class WorkingTreeRow {}

    /// The picker's highlight marks where Return goes, so it draws in the accent color the
    /// way a menu's does, although keyboard focus stays in the filter field.
    private final class PickerRowView: NSTableRowView {
        override var isEmphasized: Bool {
            get { true }
            set {}
        }
    }

    private let behavior: Behavior
    private let outline = NSOutlineView()
    private let search = NSSearchField()
    private var sections: [Section] = []
    private var listing = RefListing(refs: [])
    private var head: Head?
    private var selected: RefEntry?
    private let workingTreeRow = WorkingTreeRow()
    private var showsWorkingTree = false
    /// Whether the Working Tree row is in the list now: shown, and kept by the filter.
    private var listsWorkingTree = false
    private var workingTreeSelected = false
    private var isReloading = false

    init(behavior: Behavior = .sidebar) {
        self.behavior = behavior
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func loadView() {
        search.placeholderString = "Filter refs"
        search.delegate = self

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("ref"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.dataSource = self
        outline.delegate = self
        Style.sidebarList(outline)
        outline.floatsGroupRows = false
        outline.autoresizesOutlineColumn = true
        outline.indentationPerLevel = 8
        let menu = NSMenu()
        menu.delegate = self
        outline.menu = menu
        if behavior == .picker {
            outline.target = self
            outline.action = #selector(rowClicked(_:))
            // Typing stays in the filter; the arrow keys reach the list through it.
            outline.refusesFirstResponder = true
            // The popover draws its own material; a sidebar's would show as a box inside it.
            outline.backgroundColor = .clear
        }

        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.autohidesScrollers = true

        // The list runs edge to edge: the source list insets its rows by `Style.margin`
        // itself, and the filter above keeps the same margin so the two line up.
        let stack = NSStackView(views: [search, scroll])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 2
        stack.edgeInsets = NSEdgeInsets(top: Style.barPadding, left: 0, bottom: 0, right: 0)
        stack.setHuggingPriority(.defaultLow, for: .vertical)
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)
        stack.setVisibilityPriority(.mustHold, for: scroll)
        NSLayoutConstraint.activate([
            search.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -2 * Style.margin),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor),
        ])
        view = stack
    }

    /// Puts the cursor in the filter, for the picker as it opens.
    func focusFilter() {
        view.window?.makeFirstResponder(search)
    }

    /// Shows `listing`. A reload that changes nothing on screen does nothing: ref events and
    /// selection moves call it far more often than the refs change.
    func reload(
        _ listing: RefListing, selected: RefEntry?, head: Head?,
        showsWorkingTree: Bool = false, workingTreeSelected: Bool = false
    ) {
        guard
            listing != self.listing || selected != self.selected || head != self.head
                || showsWorkingTree != self.showsWorkingTree
                || workingTreeSelected != self.workingTreeSelected
        else { return }
        self.listing = listing
        self.selected = selected
        self.head = head
        self.showsWorkingTree = showsWorkingTree
        self.workingTreeSelected = workingTreeSelected
        rebuild()
    }

    private func rebuild() {
        let filter = search.stringValue
        listsWorkingTree = showsWorkingTree && RefListing.workingTreeMatches(filter)
        sections = listing.sections(matching: filter).map {
            Section(title: $0.title, refs: $0.refs)
        }
        let broken = listing.unreadable(matching: filter)
        if !broken.isEmpty {
            sections.append(
                Section(
                    title: RefListing.unreadableTitle, refs: [],
                    broken: broken.map(BrokenRow.init)))
        }

        isReloading = true
        outline.reloadData()
        for section in sections { outline.expandItem(section) }
        if behavior == .picker, RefListing.query(from: filter) != nil {
            // Typing narrows the list; Return takes the best match, not the row browsed now. A
            // filter only the Working Tree row matches highlights that row.
            let best = listing.bestMatch(for: filter).flatMap { rowOf($0) }
            highlight(row: best ?? workingTreeRowIndex)
        } else if workingTreeSelected, let row = workingTreeRowIndex {
            outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else if let selected, let row = rowOf(selected) {
            outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else {
            outline.deselectAll(nil)
        }
        isReloading = false
    }

    /// The Working Tree row's index, or `nil` when the list does not show it.
    private var workingTreeRowIndex: Int? {
        let row = outline.row(forItem: workingTreeRow)
        return row >= 0 ? row : nil
    }

    private func firstRow(where matches: (Any?) -> Bool) -> Int? {
        (0..<outline.numberOfRows).first { matches(outline.item(atRow: $0)) }
    }

    private func highlight(row: Int?) {
        guard let row else {
            outline.deselectAll(nil)
            return
        }
        outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        outline.scrollRowToVisible(row)
    }

    private func isPickable(_ item: Any?) -> Bool {
        item is RefEntry || item is WorkingTreeRow
    }

    /// Whether `ref` is what the window browses now, so picking it changes nothing.
    private func isCurrent(_ ref: RefEntry) -> Bool {
        !workingTreeSelected && ref.fullName == selected?.fullName
    }

    // MARK: Keyboard

    /// Moves the highlight to the next pickable row, stepping over group headers. In the
    /// sidebar the highlight is the selection, so this browses the row.
    private func moveHighlight(by step: Int) {
        var row = outline.selectedRow + step
        while outline.item(atRow: row) != nil, !isPickable(outline.item(atRow: row)) {
            row += step
        }
        guard isPickable(outline.item(atRow: row)) else { return }
        highlight(row: row)
    }

    /// Picks a row for the picker. The row already browsed closes it with nothing to reload,
    /// so the tree keeps its expanded folders.
    private func pick(row: Int) {
        let item = outline.item(atRow: row)
        if item is WorkingTreeRow {
            if workingTreeSelected { onClose?() } else { onSelectWorkingTree?() }
        } else if let ref = item as? RefEntry {
            if isCurrent(ref) { onClose?() } else { onSelect?(ref) }
        }
    }

    @objc private func rowClicked(_ sender: Any?) {
        pick(row: outline.clickedRow)
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

    /// Keys typed in the filter: the arrows move the highlight, which in the sidebar browses
    /// as it moves. Return picks in the picker; in the sidebar it browses the best match and
    /// moves focus to the list. Escape closes the picker and clears the sidebar's filter.
    func control(
        _ control: NSControl, textView: NSTextView, doCommandBy selector: Selector
    ) -> Bool {
        switch selector {
        case #selector(NSResponder.moveDown(_:)):
            moveHighlight(by: 1)
        case #selector(NSResponder.moveUp(_:)):
            moveHighlight(by: -1)
        case #selector(NSResponder.insertNewline(_:)):
            if behavior == .picker { pickHighlighted() } else { browseBestMatch() }
        case #selector(NSResponder.cancelOperation(_:)):
            if behavior == .picker { onClose?() } else { clearFilter() }
        default:
            return false
        }
        return true
    }

    /// Picks the highlighted row, or the first row when nothing is highlighted and no filter is
    /// typed. A filter that highlights nothing matched nothing, so Return does nothing.
    private func pickHighlighted() {
        let highlighted = outline.selectedRow >= 0 ? outline.selectedRow : nil
        let unfiltered = RefListing.query(from: search.stringValue) == nil
        if let row = highlighted ?? (unfiltered ? firstRow(where: isPickable) : nil) {
            pick(row: row)
        }
    }

    /// Selects the filter's best match, or the first pickable row, which browses it, then
    /// hands the keyboard to the list.
    private func browseBestMatch() {
        let best = listing.bestMatch(for: search.stringValue).flatMap { rowOf($0) }
        if let row = best ?? firstRow(where: isPickable) {
            highlight(row: row)
        }
        view.window?.makeFirstResponder(outline)
    }

    private func clearFilter() {
        search.stringValue = ""
        rebuild()
    }

    // MARK: NSMenuDelegate

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard let ref = clickedRef else { return }
        // A ref whose name is not valid UTF-8 shows a U+FFFD spelling that could name another
        // ref, so nothing that takes the name is offered.
        guard ref.hasExactName else {
            let note = NSMenuItem(
                title: "Name Is Not Valid UTF-8", action: nil, keyEquivalent: "")
            note.isEnabled = false
            menu.addItem(note)
            return
        }
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
        if item == nil { return sections.count + (listsWorkingTree ? 1 : 0) }
        if let section = item as? Section { return section.refs.count + section.broken.count }
        return 0
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        if let section = item as? Section {
            if index < section.refs.count { return section.refs[index] }
            return section.broken[index - section.refs.count]
        }
        if listsWorkingTree {
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
        isPickable(item)
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any)
        -> NSView?
    {
        if let section = item as? Section {
            let cell = headerCell()
            cell.textField?.stringValue = section.title
            return cell
        }
        if item is WorkingTreeRow {
            let cell = refCell()
            cell.imageView?.image = Icons.workingTree
            cell.label.stringValue = "Working Tree"
            cell.isEmphasized = false
            cell.toolTip = "Files on disk, compared with HEAD"
            return cell
        }
        if let row = item as? BrokenRow {
            let cell = refCell()
            cell.imageView?.image = Icons.unreadableRef
            cell.label.stringValue = row.ref.fullName
            cell.isEmphasized = false
            cell.toolTip = row.ref.error
            return cell
        }
        guard let ref = item as? RefEntry else { return nil }
        let cell = refCell()
        cell.imageView?.image = Icons.symbol(for: ref.kind)
        cell.label.stringValue = ref.shortName
        cell.isEmphasized = head?.refName == ref.fullName
        cell.toolTip = ref.fullName + "\n" + ref.target.hex
        return cell
    }

    func outlineView(_ outlineView: NSOutlineView, rowViewForItem item: Any) -> NSTableRowView? {
        behavior == .picker ? PickerRowView() : nil
    }

    func outlineView(
        _ outlineView: NSOutlineView, typeSelectStringFor tableColumn: NSTableColumn?, item: Any
    ) -> String? {
        if item is WorkingTreeRow { return "Working Tree" }
        if let row = item as? BrokenRow { return row.ref.fullName }
        return (item as? RefEntry)?.shortName
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        // The picker's selection is only a highlight; a click or Return picks.
        guard !isReloading, behavior == .sidebar else { return }
        if outline.item(atRow: outline.selectedRow) is WorkingTreeRow {
            guard !workingTreeSelected else { return }
            workingTreeSelected = true
            selected = nil
            onSelectWorkingTree?()
            return
        }
        guard let ref = outline.item(atRow: outline.selectedRow) as? RefEntry,
            !isCurrent(ref)
        else { return }
        workingTreeSelected = false
        selected = ref
        onSelect?(ref)
    }

    private func refCell() -> SidebarCellView {
        let id = NSUserInterfaceItemIdentifier("ref")
        return outline.makeView(withIdentifier: id, owner: self) as? SidebarCellView
            ?? SidebarCellView(identifier: id)
    }

    /// A section title. The source list styles a group row's `textField` itself.
    private func headerCell() -> NSTableCellView {
        let id = NSUserInterfaceItemIdentifier("header")
        if let cell = outline.makeView(withIdentifier: id, owner: self) as? NSTableCellView {
            return cell
        }
        let cell = NSTableCellView()
        cell.identifier = id
        let label = NSTextField(labelWithString: "")
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(label)
        cell.textField = label
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: cell.leadingAnchor),
            label.trailingAnchor.constraint(lessThanOrEqualTo: cell.trailingAnchor),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}
