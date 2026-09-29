import AppKit
import RcGitKit

/// An immutable object handed from a background build to the main actor. `NSAttributedString`
/// is safe to construct off-main and read on-main once no one else mutates it.
private struct BuiltText: @unchecked Sendable {
    let value: NSAttributedString
    let anchors: [String: Int]
}

/// Shows one file: a header with path and size, then read-only monospaced text with line
/// numbers, or a rendered preview for markdown. Uses a TextKit 1 stack with non-contiguous
/// layout so multi-megabyte files open without laying out every line first. The attributed
/// string is built off the main actor, and syntax colors are applied as temporary attributes
/// for the visible range only, so no pass over the whole document ever runs on the main actor.
@MainActor
final class FileViewController: NSViewController {
    var onLoadAnyway: ((TreeEntry, UInt64) -> Void)?
    /// Asked for syntax spans after a text file painted; `nil` means stay plain.
    var highlightProvider: ((TreeEntry, TextDocument) async -> [HighlightSpan]?)?
    /// A markdown link that pointed inside the repository was clicked; the argument is the
    /// repository-relative path it resolved to.
    var onOpenRepositoryPath: ((String) -> Void)?
    /// The repository-relative path of the file on screen, which is what a relative markdown
    /// link is resolved against. `nil` when the opener did not know it.
    var repositoryPath: String?

    /// Files above this size are never offered for display, even with "Load anyway".
    static let hardLimit: UInt64 = 512 * 1024 * 1024
    /// Markdown above this size opens in source mode. The parser reads about 50 MB/s, so the
    /// limit is what keeps TextKit's layout of the rendered result quick, not the parse.
    static let previewLimit = 8 * 1024 * 1024
    /// Syntax colors are applied per chunk of this many UTF-16 units as chunks scroll in.
    static let highlightChunk = 4096

    private let header = NSStackView()
    private let headerSeparator = NSBox()
    private let pathLabel = NSTextField(labelWithString: "")
    private let detailLabel = NSTextField(labelWithString: "")
    private let modePicker = NSSegmentedControl(
        labels: ["Preview", "Source"], trackingMode: .selectOne, target: nil, action: nil)
    private let placeholder = NSTextField(wrappingLabelWithString: "Select a file")
    private let loadAnyway = NSButton(title: "Load anyway", target: nil, action: nil)
    private let scroll = NSScrollView()
    private let textStorage = NSTextStorage()
    private var textView: NSTextView!
    private var ruler: LineNumberRulerView!
    private var tooLargeEntry: (TreeEntry, UInt64)?

    /// Bumped on every content change; every background build checks it before applying.
    private var contentGeneration = 0
    private var current: (entry: TreeEntry, document: TextDocument)?
    /// Whether the markdown pane shows the rendered preview; remembered across files.
    private var markdownPrefersPreview = true
    /// UTF-16 highlight ranges for the whole document, sorted and non-overlapping.
    private var highlightRanges: [(range: NSRange, class: HighlightClass)] = []
    /// Chunks of `Self.highlightChunk` UTF-16 units already colored.
    private var appliedChunks = IndexSet()
    /// Heading anchors of the preview on screen, for `#fragment` links.
    private var anchors: [String: Int] = [:]
    /// A heading a link asked for in a file that was still loading, applied once it paints.
    private var pendingAnchor: String?

    override func loadView() {
        pathLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        pathLabel.lineBreakMode = .byTruncatingHead
        pathLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        // The name takes the bar's spare width, so the size and the mode picker sit at the
        // trailing edge.
        pathLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        detailLabel.font = NSFont.monospacedDigitSystemFont(
            ofSize: NSFont.smallSystemFontSize, weight: .regular)
        detailLabel.textColor = .secondaryLabelColor
        detailLabel.alignment = .right
        detailLabel.setContentHuggingPriority(.required, for: .horizontal)
        modePicker.segmentStyle = .roundRect
        modePicker.controlSize = .small
        modePicker.target = self
        modePicker.action = #selector(modeChanged(_:))
        modePicker.isHidden = true
        modePicker.setContentHuggingPriority(.required, for: .horizontal)

        for view in [pathLabel, detailLabel, modePicker] {
            header.addArrangedSubview(view)
        }
        header.orientation = .horizontal
        header.distribution = .fill
        header.alignment = .centerY
        header.spacing = 8
        header.edgeInsets = NSEdgeInsets(top: 6, left: Style.margin, bottom: 6, right: Style.margin)
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
        textView.delegate = self
        // The string already carries the link color and underline; leaving these empty stops
        // NSTextView painting its own over them, and keeps the pointing-hand cursor.
        textView.linkTextAttributes = [.cursor: NSCursor.pointingHand]

        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        ruler = LineNumberRulerView(scrollView: scroll, textView: textView)
        scroll.verticalRulerView = ruler
        scroll.hasVerticalRuler = true
        scroll.rulersVisible = true
        NotificationCenter.default.addObserver(
            self, selector: #selector(visibleRegionChanged(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)

        placeholder.alignment = .center
        placeholder.textColor = .secondaryLabelColor
        placeholder.translatesAutoresizingMaskIntoConstraints = false
        loadAnyway.target = self
        loadAnyway.action = #selector(loadAnywayPressed(_:))
        loadAnyway.translatesAutoresizingMaskIntoConstraints = false
        loadAnyway.isHidden = true

        headerSeparator.boxType = .separator

        // Pinned rather than stacked: the header hides while no file is open, and a view a
        // stack detaches before its first layout never gets its width back.
        let root = NSView()
        for view in [header, headerSeparator, scroll] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(view)
        }
        root.addSubview(placeholder)
        root.addSubview(loadAnyway)
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: root.topAnchor),
            header.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            headerSeparator.topAnchor.constraint(equalTo: header.bottomAnchor),
            headerSeparator.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            headerSeparator.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: headerSeparator.bottomAnchor),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            placeholder.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            placeholder.widthAnchor.constraint(lessThanOrEqualTo: root.widthAnchor, constant: -40),
            loadAnyway.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            loadAnyway.topAnchor.constraint(equalTo: placeholder.bottomAnchor, constant: 12),
        ])
        view = root
        render(.empty)
    }

    // Computed and nonisolated: the source build runs off the main actor, and NSFont itself
    // caches instances.
    private nonisolated static var font: NSFont {
        NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    }

    func render(_ state: FileState) {
        tooLargeEntry = nil
        loadAnyway.isHidden = true
        // No file, no bar: an empty strip over "Select a file" reads as a missing title.
        header.isHidden = state == .empty
        headerSeparator.isHidden = state == .empty
        switch state {
        case .empty:
            current = nil
            modePicker.isHidden = true
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
            current = (entry, document)
            presentContent()
        case let .binary(entry, size):
            current = nil
            modePicker.isHidden = true
            pathLabel.stringValue = entry.name
            detailLabel.stringValue = ByteFormat.string(size)
            showPlaceholder("\(entry.name) is a binary file (\(ByteFormat.string(size))).")
        case let .tooLarge(entry, size, limit):
            current = nil
            modePicker.isHidden = true
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
            current = nil
            modePicker.isHidden = true
            pathLabel.stringValue = entry.name
            detailLabel.stringValue = ""
            showPlaceholder(message)
        }
    }

    @objc private func loadAnywayPressed(_ sender: Any?) {
        guard let (entry, size) = tooLargeEntry else { return }
        onLoadAnyway?(entry, size)
    }

    @objc private func modeChanged(_ sender: Any?) {
        markdownPrefersPreview = modePicker.selectedSegment == 0
        presentContent()
    }

    // MARK: Content

    private static func isMarkdown(_ name: String) -> Bool {
        let lowered = name.lowercased()
        return lowered.hasSuffix(".md") || lowered.hasSuffix(".markdown")
    }

    /// Shows the current document in the chosen mode. The attributed string is built off the
    /// main actor; a stale build is dropped by generation.
    private func presentContent() {
        guard let (entry, document) = current else { return }
        contentGeneration += 1
        let generation = contentGeneration
        clearHighlights()
        let previewable =
            Self.isMarkdown(entry.name) && document.byteCount <= Self.previewLimit
        modePicker.isHidden = !previewable
        let preview = previewable && markdownPrefersPreview
        modePicker.selectedSegment = preview ? 0 : 1
        let text = document.text
        Task { [weak self] in
            let built = await Task.detached(priority: .userInitiated) { () -> BuiltText in
                if preview {
                    let rendered = MarkdownRenderer.render(text)
                    return BuiltText(value: rendered.text, anchors: rendered.anchors)
                }
                let attributes: [NSAttributedString.Key: Any] = [
                    .font: FileViewController.font,
                    .foregroundColor: NSColor.textColor,
                ]
                return BuiltText(
                    value: NSAttributedString(string: text, attributes: attributes), anchors: [:])
            }.value
            guard let self, generation == self.contentGeneration else { return }
            self.anchors = built.anchors
            self.show(built.value, asPreview: preview, lineStarts: document.utf16LineStarts)
            if !preview {
                self.fetchHighlights(entry: entry, document: document, generation: generation)
            }
            self.applyPendingAnchor()
        }
    }

    private func show(_ text: NSAttributedString, asPreview: Bool, lineStarts: [Int]) {
        placeholder.isHidden = true
        scroll.isHidden = false
        if asPreview {
            configureForPreview()
        } else {
            configureForSource()
        }
        textStorage.setAttributedString(text)
        ruler.setLineStarts(asPreview ? [] : lineStarts)
        textView.setSelectedRange(NSRange(location: 0, length: 0))
        textView.scroll(.zero)
    }

    /// Source mode: no wrapping, one fragment per line, the ruler visible.
    private func configureForSource() {
        scroll.rulersVisible = true
        scroll.hasHorizontalScroller = true
        textView.isHorizontallyResizable = true
        textView.autoresizingMask = []
        if let container = textView.textContainer {
            container.widthTracksTextView = false
            container.size = NSSize(
                width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        }
    }

    /// Preview mode: wrapped prose, no ruler. The ruler assumes one fragment per line, which
    /// wrapping breaks, so it is hidden here.
    private func configureForPreview() {
        scroll.rulersVisible = false
        scroll.hasHorizontalScroller = false
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        if let container = textView.textContainer {
            container.widthTracksTextView = true
            container.size = NSSize(
                width: scroll.contentSize.width, height: CGFloat.greatestFiniteMagnitude)
        }
        textView.setFrameSize(
            NSSize(width: scroll.contentSize.width, height: textView.frame.height))
    }

    private func showPlaceholder(_ message: String) {
        contentGeneration += 1
        anchors = [:]
        pendingAnchor = nil
        clearHighlights()
        placeholder.stringValue = message
        placeholder.isHidden = false
        scroll.isHidden = true
        textStorage.setAttributedString(NSAttributedString(string: ""))
        ruler.setLineStarts([])
    }

    // MARK: Highlighting

    /// Fetches spans in the background, maps them to UTF-16 off the main actor, and colors
    /// only what is visible; scrolling colors the rest chunk by chunk. The plain paint never
    /// waits on the parse.
    private func fetchHighlights(entry: TreeEntry, document: TextDocument, generation: Int) {
        guard let highlightProvider else { return }
        Task { [weak self] in
            guard let spans = await highlightProvider(entry, document), !spans.isEmpty else {
                return
            }
            let text = document.text
            let ranges = await Task.detached(priority: .utility) {
                HighlightMapper.utf16Ranges(for: spans, in: text)
            }.value
            guard let self, generation == self.contentGeneration else { return }
            self.highlightRanges = ranges
            self.appliedChunks.removeAll()
            self.applyVisibleHighlights()
        }
    }

    private func clearHighlights() {
        highlightRanges = []
        appliedChunks.removeAll()
        if let layoutManager = textView?.layoutManager, textStorage.length > 0 {
            layoutManager.removeTemporaryAttribute(
                .foregroundColor,
                forCharacterRange: NSRange(location: 0, length: textStorage.length)
            )
        }
    }

    @objc private func visibleRegionChanged(_ notification: Notification) {
        applyVisibleHighlights()
    }

    /// Colors the chunks covering the visible range plus one screen of margin. Temporary
    /// attributes change no layout, so applying them costs only the visible spans.
    private func applyVisibleHighlights() {
        guard !highlightRanges.isEmpty, let layoutManager = textView.layoutManager,
            let container = textView.textContainer, textStorage.length > 0
        else { return }
        let visible = textView.visibleRect
        let expanded = visible.insetBy(dx: 0, dy: -visible.height).intersection(textView.bounds)
        guard !expanded.isEmpty else { return }
        let glyphRange = layoutManager.glyphRange(forBoundingRect: expanded, in: container)
        let charRange = layoutManager.characterRange(
            forGlyphRange: glyphRange, actualGlyphRange: nil)
        guard charRange.length > 0 else { return }
        let firstChunk = charRange.location / Self.highlightChunk
        let lastChunk = (NSMaxRange(charRange) - 1) / Self.highlightChunk
        for chunk in firstChunk...lastChunk where !appliedChunks.contains(chunk) {
            appliedChunks.insert(chunk)
            applyChunk(chunk, layoutManager: layoutManager)
        }
    }

    /// Applies every stored range touching one chunk. Ranges are sorted and non-overlapping,
    /// so a binary search finds the first candidate and a linear walk covers the rest.
    private func applyChunk(_ chunk: Int, layoutManager: NSLayoutManager) {
        let chunkStart = chunk * Self.highlightChunk
        let chunkEnd = min(chunkStart + Self.highlightChunk, textStorage.length)
        guard chunkStart < chunkEnd else { return }
        var low = 0
        var high = highlightRanges.count
        while low < high {
            let mid = (low + high) / 2
            if NSMaxRange(highlightRanges[mid].range) <= chunkStart {
                low = mid + 1
            } else {
                high = mid
            }
        }
        var index = low
        while index < highlightRanges.count {
            let (range, highlightClass) = highlightRanges[index]
            if range.location >= chunkEnd { break }
            if NSMaxRange(range) <= textStorage.length {
                layoutManager.addTemporaryAttribute(
                    .foregroundColor, value: HighlightPalette.color(for: highlightClass),
                    forCharacterRange: range)
            }
            index += 1
        }
    }
}

// MARK: - Markdown links

extension FileViewController: NSTextViewDelegate {
    func textView(_ textView: NSTextView, clickedOnLink link: Any, at charIndex: Int) -> Bool {
        open(destination: Self.destination(of: link))
        return true
    }

    /// Adds the link commands above the text view's own menu when the click landed on a link,
    /// so a destination can be opened, followed in a browser, or copied without leaving the
    /// pane.
    func textView(
        _ view: NSTextView, menu: NSMenu, for event: NSEvent, at charIndex: Int
    ) -> NSMenu? {
        guard charIndex < textStorage.length,
            let attribute = textStorage.attribute(.link, at: charIndex, effectiveRange: nil)
        else { return menu }
        let destination = Self.destination(of: attribute)
        guard let target = MarkdownLinkTarget.resolve(destination, from: repositoryPath) else {
            return menu
        }
        var items: [NSMenuItem] = []
        switch target {
        case .repositoryPath:
            items.append(item("Open File", #selector(openLink(_:)), destination))
        case .fragment:
            items.append(item("Go to Section", #selector(openLink(_:)), destination))
        case .external:
            items.append(item("Open in Browser", #selector(openLink(_:)), destination))
        }
        items.append(item("Copy Link", #selector(copyLink(_:)), Self.copyText(for: target)))
        items.append(.separator())
        for (offset, entry) in items.enumerated() { menu.insertItem(entry, at: offset) }
        return menu
    }

    private func item(_ title: String, _ action: Selector, _ payload: String) -> NSMenuItem {
        let entry = NSMenuItem(title: title, action: action, keyEquivalent: "")
        entry.target = self
        entry.representedObject = payload
        return entry
    }

    @objc private func openLink(_ sender: NSMenuItem) {
        open(destination: sender.representedObject as? String ?? "")
    }

    @objc private func copyLink(_ sender: NSMenuItem) {
        guard let value = sender.representedObject as? String else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    /// Follows one markdown destination: a URL goes to the browser, a repository path is
    /// handed up so the pane can show that file, and a bare fragment scrolls this one.
    private func open(destination: String) {
        guard let target = MarkdownLinkTarget.resolve(destination, from: repositoryPath) else {
            NSSound.beep()
            return
        }
        switch target {
        case let .external(url):
            NSWorkspace.shared.open(url)
        case let .repositoryPath(path, fragment):
            pendingAnchor = fragment
            onOpenRepositoryPath?(path)
        case let .fragment(name):
            pendingAnchor = name
            applyPendingAnchor()
        }
    }

    /// What "Copy Link" puts on the pasteboard: the absolute URL for the web, and the
    /// repository-relative path for anything inside the repository.
    private static func copyText(for target: MarkdownLinkTarget) -> String {
        switch target {
        case let .external(url): return url.absoluteString
        case let .repositoryPath(path, fragment):
            return fragment.map { "\(path)#\($0)" } ?? path
        case let .fragment(name): return "#\(name)"
        }
    }

    private static func destination(of link: Any) -> String {
        if let text = link as? String { return text }
        if let url = link as? URL { return url.absoluteString }
        return ""
    }

    /// Scrolls to the heading a `#fragment` asked for, once the file that has it is on screen.
    /// The request survives the load, so a link into another file lands on its section.
    private func applyPendingAnchor() {
        guard let name = pendingAnchor else { return }
        pendingAnchor = nil
        guard let location = anchors[MarkdownDocument.anchor(for: name)] else { return }
        guard let layoutManager = textView.layoutManager, let container = textView.textContainer,
            location < textStorage.length
        else { return }
        let glyphs = layoutManager.glyphRange(
            forCharacterRange: NSRange(location: location, length: 1), actualCharacterRange: nil)
        let rect = layoutManager.boundingRect(forGlyphRange: glyphs, in: container)
        textView.setSelectedRange(NSRange(location: location, length: 0))
        textView.scroll(NSPoint(x: 0, y: max(rect.minY - 8, 0)))
    }
}
