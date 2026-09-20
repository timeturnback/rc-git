import AppKit

/// The left panel: the ref list on top, and under it the section that belongs to the mode
/// showing — the directory tree for Files, the change list and commit box for Changes.
/// History has no section, so the refs fill the panel.
@MainActor
final class SidebarViewController: NSViewController {
    let refs = RefsViewController()

    /// The refs give the rest of the panel to the section beside them. Dropped when no
    /// section shows, so the list fills the panel on its own.
    private static let refsHeightBesideASection: CGFloat = 240

    private let stack = NSStackView()
    private let separator = NSBox()
    private var sections: [NSView] = []
    private var refsHeight: NSLayoutConstraint?
    private var sectionWidth: NSLayoutConstraint?

    /// Registers a section and hides it. The window controller adds one per mode that has
    /// one, then calls `show(section:)` on every mode switch. `controller` is the owner to
    /// adopt, for a section whose controller lives nowhere else.
    ///
    /// A hidden section leaves the stack, and AppKit drops every constraint an ancestor
    /// holds for a view it removes, so a section carries no constraint to anything outside
    /// itself: its height comes from hugging and from what it constrains internally.
    func add(section: NSView, from controller: NSViewController? = nil) {
        // The sections stack under the refs, so the refs must already be in the stack.
        loadViewIfNeeded()
        if let controller { addChild(controller) }
        section.isHidden = true
        section.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        sections.append(section)
        stack.addArrangedSubview(section)
    }

    /// Shows one registered section, or none at all.
    func show(section: NSView?) {
        loadViewIfNeeded()
        sectionWidth?.isActive = false
        sectionWidth = nil
        for candidate in sections {
            candidate.isHidden = candidate !== section
        }
        separator.isHidden = section == nil
        fitRefsToWholeRows()
        refsHeight?.isActive = section != nil
        guard let section else { return }
        // A section joins the stack hidden, so it is detached before the stack ever lays out
        // and never receives the `.width` alignment constraint the stack gives a view it laid
        // out — it would size to its content's own width and sit against the trailing edge.
        // The stack never makes that constraint later either, and AppKit drops an ancestor's
        // constraints for a view it detaches, so the width is given again on every show.
        let width = section.widthAnchor.constraint(equalTo: stack.widthAnchor)
        width.isActive = true
        sectionWidth = width
    }

    /// Ends the ref list on a row boundary, so the section under it never starts across half
    /// a branch. The rows change with the repository, so this runs again on every reload.
    private func fitRefsToWholeRows() {
        refsHeight?.constant = refs.heightEndingOnARow(atMost: Self.refsHeightBesideASection)
    }

    override func loadView() {
        separator.boxType = .separator
        separator.isHidden = true

        refs.onRowsChanged = { [weak self] in self?.fitRefsToWholeRows() }
        addChild(refs)
        stack.orientation = .vertical
        stack.alignment = .width
        stack.spacing = 0
        // A hidden section must leave the stack, not keep a zero-height frame.
        stack.detachesHiddenViews = true
        stack.addArrangedSubview(refs.view)
        stack.addArrangedSubview(separator)
        refs.view.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        let height = refs.view.heightAnchor.constraint(
            equalToConstant: Self.refsHeightBesideASection)
        // Low, so a short window shrinks the list instead of breaking the layout.
        height.priority = NSLayoutConstraint.Priority(rawValue: 300)
        refsHeight = height
        view = stack
    }
}
