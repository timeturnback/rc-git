import AppKit

/// The left panel. History shows the ref list alone. Files and Changes put the mode's section
/// under a branch banner that stands in for the list — the directory tree for Files, the
/// change list and commit box for Changes — so the section gets the height the list took.
@MainActor
final class SidebarViewController: NSViewController {
    let refs = RefsViewController()
    let banner = RefBannerView()

    private let stack = NSStackView()
    private let bannerRow = NSView()
    private let separator = NSBox()
    private var sections: [NSView] = []
    /// The widths `show(section:)` gave the views showing.
    private var widths: [NSLayoutConstraint] = []

    /// Registers a section and hides it. The window controller adds one per mode that has
    /// one, then calls `show(section:)` on every mode switch. `controller` is the owner to
    /// adopt, for a section whose controller lives nowhere else.
    ///
    /// A hidden section leaves the stack, and AppKit drops every constraint an ancestor
    /// holds for a view it removes, so a section carries no constraint to anything outside
    /// itself: its height comes from hugging and from what it constrains internally.
    func add(section: NSView, from controller: NSViewController? = nil) {
        // The sections stack under the banner, so the banner must already be in the stack.
        loadViewIfNeeded()
        if let controller { addChild(controller) }
        section.isHidden = true
        section.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        sections.append(section)
        stack.addArrangedSubview(section)
    }

    /// Shows one registered section under the banner, or the ref list alone for `nil`.
    func show(section: NSView?) {
        loadViewIfNeeded()
        NSLayoutConstraint.deactivate(widths)
        for candidate in sections {
            candidate.isHidden = candidate !== section
        }
        let besideASection = section != nil
        refs.view.isHidden = besideASection
        bannerRow.isHidden = !besideASection
        separator.isHidden = !besideASection
        // Any of these views can be hidden before the stack first lays out — the window opens
        // on Files, which hides the ref list — and a view the stack never laid out while
        // attached never receives the `.width` alignment constraint; unhiding does not make
        // one either. AppKit also drops an ancestor's constraints for a view it detaches, so
        // every view showing gets its width again on every show.
        widths = stack.arrangedSubviews.filter { !$0.isHidden }.map {
            $0.widthAnchor.constraint(equalTo: stack.widthAnchor)
        }
        NSLayoutConstraint.activate(widths)
    }

    override func loadView() {
        separator.boxType = .separator
        banner.translatesAutoresizingMaskIntoConstraints = false
        bannerRow.addSubview(banner)
        NSLayoutConstraint.activate([
            banner.topAnchor.constraint(equalTo: bannerRow.topAnchor, constant: 8),
            banner.bottomAnchor.constraint(equalTo: bannerRow.bottomAnchor, constant: -8),
            banner.leadingAnchor.constraint(equalTo: bannerRow.leadingAnchor, constant: 8),
            banner.trailingAnchor.constraint(equalTo: bannerRow.trailingAnchor, constant: -8),
        ])

        addChild(refs)
        stack.orientation = .vertical
        stack.alignment = .width
        stack.spacing = 0
        // A hidden view must leave the stack, not keep a zero-height frame.
        stack.detachesHiddenViews = true
        stack.addArrangedSubview(bannerRow)
        stack.addArrangedSubview(refs.view)
        stack.addArrangedSubview(separator)
        refs.view.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        view = stack
    }
}
