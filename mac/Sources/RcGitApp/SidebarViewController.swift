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
        for candidate in sections {
            candidate.isHidden = candidate !== section
        }
        separator.isHidden = section == nil
        refsHeight?.isActive = section != nil
    }

    override func loadView() {
        separator.boxType = .separator
        separator.isHidden = true

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
