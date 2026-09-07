import AppKit

/// Accepts a folder dragged onto the window and reports its path. Wraps the window's real
/// content so the whole surface is a drop target; everything else passes straight through.
final class RepositoryDropView: NSView {
    var onDropDirectory: ((String) -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        registerForDraggedTypes([.fileURL])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        directory(from: sender) == nil ? [] : .copy
    }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        guard let path = directory(from: sender) else { return false }
        onDropDirectory?(path)
        return true
    }

    private func directory(from info: any NSDraggingInfo) -> String? {
        let options: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        guard
            let urls = info.draggingPasteboard.readObjects(
                forClasses: [NSURL.self], options: options) as? [URL]
        else { return nil }
        for url in urls {
            var isDirectory: ObjCBool = false
            if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory),
                isDirectory.boolValue
            {
                return url.path
            }
        }
        return nil
    }
}

/// Hosts one child view controller inside a `RepositoryDropView`, so a window can take
/// folder drops without any pane knowing about them.
@MainActor
final class DropHostViewController: NSViewController {
    private let child: NSViewController
    private let dropView = RepositoryDropView()

    var onDropDirectory: ((String) -> Void)? {
        get { dropView.onDropDirectory }
        set { dropView.onDropDirectory = newValue }
    }

    init(hosting child: NSViewController) {
        self.child = child
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func loadView() {
        addChild(child)
        child.view.translatesAutoresizingMaskIntoConstraints = false
        dropView.addSubview(child.view)
        NSLayoutConstraint.activate([
            child.view.topAnchor.constraint(equalTo: dropView.topAnchor),
            child.view.bottomAnchor.constraint(equalTo: dropView.bottomAnchor),
            child.view.leadingAnchor.constraint(equalTo: dropView.leadingAnchor),
            child.view.trailingAnchor.constraint(equalTo: dropView.trailingAnchor),
        ])
        view = dropView
    }
}
