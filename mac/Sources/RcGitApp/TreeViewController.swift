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

    final class Node {
        let row: BrowseRow
        let path: String
        var children: [Node]?
        var isLoading = false

        var entry: TreeEntry { row.entry }

        init(row: BrowseRow, parentPath: String) {
            self.row = row
            self.path = parentPath.isEmpty ? row.entry.name : parentPath + "/" + row.entry.name
        }
    }

    private let outline = NSOutlineView()
    private let placeholder = NSTextField(labelWithString: "Open a repository (⌘O)")
    private var roots: [Node] = []
    private var generation = 0

    override func loadView() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.dataSource = self
        outline.delegate = self
        outline.rowSizeStyle = .small
        outline.indentationPerLevel = 14
        outline.autoresizesOutlineColumn = true
        outline.allowsEmptySelection = true
        outline.target = self
        outline.doubleAction = #selector(toggleSelected(_:))
        outline.menu = buildMenu()

        let scroll = NSScrollView()
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

    /// Row chrome outside the label: disclosure triangle, icon, cell insets, and room for
    /// the vertical scroller.
    private static let rowChromeWidth: CGFloat = 46

    /// The width that shows every row currently on the outline in full. Only expanded rows
    /// are measured, so the cost follows what is on screen, not repository size.
    private func reportContentWidth() {
        guard let onContentWidthChanged else { return }
        var widest: CGFloat = 0
        for row in 0..<outline.numberOfRows {
            guard let node = outline.item(atRow: row) as? Node else { continue }
            let indent = CGFloat(outline.level(forRow: row)) * outline.indentationPerLevel
            widest = max(widest, indent + Self.label(for: node.row).size().width)
        }
        onContentWidthChanged(widest + Self.rowChromeWidth)
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
                    parent.isLoading = false
                    outline.reloadItem(parent, reloadChildren: true)
                } else {
                    roots = nodes
                    outline.reloadData()
                }
                reportContentWidth()
            } catch {
                guard generation == self.generation else { return }
                parent?.isLoading = false
                onError?(String(describing: error))
            }
        }
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
        return node.children?.count ?? 0
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let node = item as? Node else { return roots[index] }
        return node.children?[index] as Any
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
        let cell: NSTableCellView
        if let reused = outline.makeView(withIdentifier: id, owner: self) as? NSTableCellView {
            cell = reused
        } else {
            cell = NSTableCellView()
            cell.identifier = id
            let imageView = NSImageView()
            imageView.translatesAutoresizingMaskIntoConstraints = false
            let label = NSTextField(labelWithString: "")
            label.lineBreakMode = .byTruncatingMiddle
            label.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
            label.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(imageView)
            cell.addSubview(label)
            cell.imageView = imageView
            cell.textField = label
            NSLayoutConstraint.activate([
                imageView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                imageView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                imageView.widthAnchor.constraint(equalToConstant: 16),
                imageView.heightAnchor.constraint(equalToConstant: 16),
                label.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 4),
                label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
                label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
        }
        cell.imageView?.image = Icons.image(for: node.entry)
        cell.textField?.attributedStringValue = Self.label(for: node.row)
        cell.toolTip = node.path
        return cell
    }

    /// The row label: the name, plus a colored status letter for working-tree rows.
    static func label(for row: BrowseRow) -> NSAttributedString {
        let font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        let name = NSMutableAttributedString(
            string: row.entry.name,
            attributes: [
                .font: font,
                .foregroundColor: row.missing ? NSColor.secondaryLabelColor : NSColor.labelColor,
            ])
        let badge: (String, NSColor)? =
            if row.missing {
                ("D", .systemRed)
            } else {
                switch row.status {
                case .modified: ("M", .systemOrange)
                case .untracked: ("U", .systemGreen)
                case .ignored: ("I", .tertiaryLabelColor)
                case .deleted: ("D", .systemRed)
                case .unchanged, nil: nil
                }
            }
        if row.status == .ignored || row.missing {
            name.addAttribute(
                .foregroundColor, value: NSColor.secondaryLabelColor,
                range: NSRange(location: 0, length: name.length))
        }
        if let (letter, color) = badge {
            name.append(
                NSAttributedString(
                    string: "  " + letter,
                    attributes: [
                        .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .bold),
                        .foregroundColor: color,
                    ]))
        }
        return name
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard let node = outline.item(atRow: outline.selectedRow) as? Node,
            !node.entry.kind.isDirectory
        else { return }
        onOpenFile?(node.row, node.path)
    }
}
