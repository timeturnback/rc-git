import AppKit

/// The popover the branch banner opens: the ref list — working tree, branches, remotes, tags —
/// with its filter focused. The arrow keys move the highlight, Return or a click picks, and
/// Escape or a click anywhere else closes. The window controller decides what a pick does and
/// closes the popover.
@MainActor
final class RefPickerController: NSViewController {
    let refs = RefsViewController(behavior: .picker)
    /// Escape, or a pick of the row already browsed.
    var onClose: (() -> Void)?

    private static let size = NSSize(width: 320, height: 420)

    override func loadView() {
        addChild(refs)
        refs.onClose = { [weak self] in self?.onClose?() }
        let container = NSView(frame: NSRect(origin: .zero, size: Self.size))
        refs.view.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(refs.view)
        NSLayoutConstraint.activate([
            refs.view.topAnchor.constraint(equalTo: container.topAnchor),
            refs.view.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            refs.view.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            refs.view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
        ])
        view = container
        preferredContentSize = Self.size
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        refs.focusFilter()
    }
}
