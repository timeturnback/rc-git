import AppKit

/// The sheet the branch banner opens: the ref list — working tree, branches, remotes, tags —
/// with its filter focused. The arrow keys move the highlight, Return or a click picks, and
/// Escape or Cancel closes. The window controller decides what a pick does and ends the sheet.
@MainActor
final class RefPickerController: NSViewController {
    let refs = RefsViewController(behavior: .picker)
    /// Escape, Cancel, or a pick of the row already browsed.
    var onClose: (() -> Void)?

    private static let size = NSSize(width: 360, height: 440)

    override func loadView() {
        addChild(refs)
        refs.onClose = { [weak self] in self?.onClose?() }
        let cancel = NSButton(title: "Cancel", target: self, action: #selector(cancelClicked(_:)))
        cancel.keyEquivalent = "\u{1b}"
        let buttons = NSStackView(views: [NSView(), cancel])
        buttons.orientation = .horizontal
        buttons.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 12, right: 12)
        let stack = NSStackView(views: [refs.view, buttons])
        stack.orientation = .vertical
        stack.alignment = .width
        stack.spacing = 8
        stack.frame = NSRect(origin: .zero, size: Self.size)
        refs.view.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        view = stack
        preferredContentSize = Self.size
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        refs.focusFilter()
    }

    @objc private func cancelClicked(_ sender: Any?) {
        onClose?()
    }
}
