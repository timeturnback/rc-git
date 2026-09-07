import AppKit
import RcGitKit

/// Shows one file: a header with path and size, then read-only monospaced text with line
/// numbers. Uses a TextKit 1 stack with non-contiguous layout so multi-megabyte files open
/// without laying out every line first.
@MainActor
final class FileViewController: NSViewController {
    var onLoadAnyway: ((TreeEntry, UInt64) -> Void)?
    /// Asked for syntax spans after a text file painted; `nil` means stay plain.
    var highlightProvider: ((TreeEntry, TextDocument) async -> [HighlightSpan]?)?

    /// Files above this size are never offered for display, even with "Load anyway".
    static let hardLimit: UInt64 = 512 * 1024 * 1024

    private let pathLabel = NSTextField(labelWithString: "")
    private let detailLabel = NSTextField(labelWithString: "")
    private let placeholder = NSTextField(wrappingLabelWithString: "Select a file")
    private let loadAnyway = NSButton(title: "Load anyway", target: nil, action: nil)
    private let scroll = NSScrollView()
    private let textStorage = NSTextStorage()
    private var textView: NSTextView!
    private var ruler: LineNumberRulerView!
    private var tooLargeEntry: (TreeEntry, UInt64)?
    private var highlightGeneration = 0

    override func loadView() {
        pathLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        pathLabel.lineBreakMode = .byTruncatingHead
        pathLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        detailLabel.font = NSFont.monospacedDigitSystemFont(
            ofSize: NSFont.smallSystemFontSize, weight: .regular)
        detailLabel.textColor = .secondaryLabelColor
        detailLabel.alignment = .right
        detailLabel.setContentHuggingPriority(.required, for: .horizontal)

        let header = NSStackView(views: [pathLabel, detailLabel])
        header.orientation = .horizontal
        header.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 6, right: 10)
        header.setHuggingPriority(.required, for: .vertical)

        let layoutManager = NSLayoutManager()
        layoutManager.allowsNonContiguousLayout = true
        let container = NSTextContainer(
            size: NSSize(
                width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = false
        container.lineFragmentPadding = 8
        layoutManager.addTextContainer(container)
        textStorage.addLayoutManager(layoutManager)
        textView = NSTextView(frame: .zero, textContainer: container)
        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = false
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true
        textView.isHorizontallyResizable = true
        textView.isVerticallyResizable = true
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.autoresizingMask = []
        textView.textContainerInset = NSSize(width: 0, height: 6)
        textView.font = FileViewController.font
        textView.backgroundColor = .textBackgroundColor
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticLinkDetectionEnabled = false

        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        ruler = LineNumberRulerView(scrollView: scroll, textView: textView)
        scroll.verticalRulerView = ruler
        scroll.hasVerticalRuler = true
        scroll.rulersVisible = true

        placeholder.alignment = .center
        placeholder.textColor = .secondaryLabelColor
        placeholder.translatesAutoresizingMaskIntoConstraints = false
        loadAnyway.target = self
        loadAnyway.action = #selector(loadAnywayPressed(_:))
        loadAnyway.translatesAutoresizingMaskIntoConstraints = false
        loadAnyway.isHidden = true

        let separator = NSBox()
        separator.boxType = .separator

        let stack = NSStackView(views: [header, separator, scroll])
        stack.orientation = .vertical
        stack.spacing = 0
        stack.translatesAutoresizingMaskIntoConstraints = false

        let root = NSView()
        root.addSubview(stack)
        root.addSubview(placeholder)
        root.addSubview(loadAnyway)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: root.topAnchor),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            placeholder.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            placeholder.widthAnchor.constraint(lessThanOrEqualTo: root.widthAnchor, constant: -40),
            loadAnyway.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            loadAnyway.topAnchor.constraint(equalTo: placeholder.bottomAnchor, constant: 12),
        ])
        view = root
        render(.empty)
    }

    private static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)

    func render(_ state: FileState) {
        tooLargeEntry = nil
        loadAnyway.isHidden = true
        switch state {
        case .empty:
            showPlaceholder("Select a file")
            pathLabel.stringValue = ""
            detailLabel.stringValue = ""
        case let .loading(entry):
            pathLabel.stringValue = entry.name
            detailLabel.stringValue = "Loading…"
        case let .text(entry, document):
            pathLabel.stringValue = entry.name
            var details =
                "\(document.lineCount) lines · \(ByteFormat.string(UInt64(document.byteCount)))"
            if document.isLossy { details += " · not valid UTF-8" }
            detailLabel.stringValue = details
            showText(document.text)
            applyHighlightsLater(entry: entry, document: document)
        case let .binary(entry, size):
            pathLabel.stringValue = entry.name
            detailLabel.stringValue = ByteFormat.string(size)
            showPlaceholder("\(entry.name) is a binary file (\(ByteFormat.string(size))).")
        case let .tooLarge(entry, size, limit):
            pathLabel.stringValue = entry.name
            detailLabel.stringValue = ByteFormat.string(size)
            showPlaceholder(
                "\(entry.name) is \(ByteFormat.string(size)), over the \(ByteFormat.string(limit)) limit."
            )
            if size <= FileViewController.hardLimit {
                tooLargeEntry = (entry, size)
                loadAnyway.isHidden = false
            }
        case let .failed(entry, message):
            pathLabel.stringValue = entry.name
            detailLabel.stringValue = ""
            showPlaceholder(message)
        }
    }

    @objc private func loadAnywayPressed(_ sender: Any?) {
        guard let (entry, size) = tooLargeEntry else { return }
        onLoadAnyway?(entry, size)
    }

    /// Fetches spans in the background and colors the text if it is still on screen. The
    /// plain paint above never waits on the parse.
    private func applyHighlightsLater(entry: TreeEntry, document: TextDocument) {
        highlightGeneration += 1
        let generation = highlightGeneration
        guard let highlightProvider else { return }
        Task { [weak self] in
            guard let spans = await highlightProvider(entry, document), !spans.isEmpty else {
                return
            }
            let text = document.text
            let ranges = await Task.detached(priority: .utility) {
                HighlightMapper.utf16Ranges(for: spans, in: text)
            }.value
            guard let self, generation == self.highlightGeneration else { return }
            let storage = self.textStorage
            guard storage.length > 0 else { return }
            storage.beginEditing()
            for (range, highlightClass) in ranges where NSMaxRange(range) <= storage.length {
                storage.addAttribute(
                    .foregroundColor, value: Self.color(for: highlightClass), range: range)
            }
            storage.endEditing()
        }
    }

    private static func color(for highlightClass: HighlightClass) -> NSColor {
        switch highlightClass {
        case .attribute: return .systemBrown
        case .comment: return .secondaryLabelColor
        case .constant: return .systemOrange
        case .function: return .systemTeal
        case .keyword: return .systemPurple
        case .number: return .systemOrange
        case .operator: return .labelColor
        case .property: return .systemIndigo
        case .punctuation: return .labelColor
        case .string: return .systemRed
        case .type: return .systemBlue
        case .variable: return .labelColor
        }
    }

    private func showPlaceholder(_ message: String) {
        placeholder.stringValue = message
        placeholder.isHidden = false
        scroll.isHidden = true
        textStorage.setAttributedString(NSAttributedString(string: ""))
        ruler.update()
    }

    private func showText(_ text: String) {
        placeholder.isHidden = true
        scroll.isHidden = false
        let attributes: [NSAttributedString.Key: Any] = [
            .font: FileViewController.font,
            .foregroundColor: NSColor.textColor,
        ]
        textStorage.setAttributedString(NSAttributedString(string: text, attributes: attributes))
        textView.setSelectedRange(NSRange(location: 0, length: 0))
        textView.scroll(.zero)
        ruler.update()
    }
}
