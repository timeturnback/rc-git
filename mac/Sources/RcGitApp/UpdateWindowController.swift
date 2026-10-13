import AppKit
import RcGitKit

/// The update window: what is installed, what the update will do, a live build log, and
/// one button. On the release bundle that button fetches, fast-forwards, rebuilds,
/// installs, and relaunches; on the dev bundle it rebuilds the working tree as it stands
/// and relaunches, which is the edit-run loop without a terminal.
///
/// It is a window rather than a sheet because the update outlives every repository
/// window: the last step quits the app and launches the new bundle, which reopens the
/// tabs from `UserDefaults`.
@MainActor
final class UpdateWindowController: NSWindowController {
    private let build: AppBuild
    private let model: UpdateModel
    private let installedLabel = NSTextField(labelWithString: "")
    private let sourceLabel = NSTextField(labelWithString: "")
    private let statusLabel = NSTextField(labelWithString: "")
    private let spinner = NSProgressIndicator()
    private let actionButton = NSButton(title: "", target: nil, action: nil)
    private let logView = NSTextView()

    /// `handOffState` runs just before the new bundle is launched, so the tabs the
    /// relaunched app reopens are the ones on screen now. It flushes them: this process is
    /// still alive when the new one reads them. `reclaimState` undoes it when the launch
    /// fails and this process keeps running.
    init(
        build: AppBuild, handOffState: @escaping @MainActor @Sendable () -> Void,
        reclaimState: @escaping @MainActor @Sendable () -> Void
    ) {
        self.build = build
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 620, height: 440),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.tabbingMode = .disallowed
        var environment: AppUpdateEnvironment?
        if let sourceRoot = build.sourceRoot, let mode = build.updateMode,
            let relaunchPath = build.relaunchPath
        {
            environment = AppUpdateEnvironment(
                sourceRoot: sourceRoot,
                mode: mode,
                relaunchPath: relaunchPath,
                window: { [weak window] in window },
                handOffState: handOffState,
                reclaimState: reclaimState)
        }
        model = UpdateModel(
            build: build, environment: environment ?? UnavailableUpdateEnvironment())
        super.init(window: window)
        window.title = UpdateCopy(build: build, state: model.state).title
        window.contentMinSize = NSSize(width: 460, height: 320)
        window.contentView = buildContentView()
        window.center()
        window.setFrameAutosaveName("rc-git.update.v1")
        model.onChanged = { [weak self] in self?.render() }
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used; the window is built in code")
    }

    // MARK: Actions

    /// Does what the button does; the model decides what that is in each state.
    func start() {
        Task { await model.proceed() }
    }

    /// The button: cancels the running fetch or build, otherwise does what `start()` does.
    @objc private func runUpdate(_ sender: Any?) {
        Task { await model.pressAction() }
    }

    // MARK: Views

    private func buildContentView() -> NSView {
        installedLabel.font = .systemFont(ofSize: 12, weight: .semibold)
        sourceLabel.font = .systemFont(ofSize: 11)
        sourceLabel.textColor = .secondaryLabelColor
        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.lineBreakMode = .byTruncatingTail
        statusLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false

        actionButton.bezelStyle = .rounded
        actionButton.target = self
        actionButton.action = #selector(runUpdate(_:))

        let header = NSStackView(views: [installedLabel, sourceLabel])
        header.orientation = .vertical
        header.alignment = .leading
        header.spacing = 2

        // An empty view with no hugging is the spacer that pushes the button to the right.
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let footer = NSStackView(views: [spinner, statusLabel, spacer, actionButton])
        footer.orientation = .horizontal
        footer.spacing = 8
        footer.alignment = .centerY

        let log = logScrollView()
        let content = NSStackView(views: [header, log, footer])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 10
        content.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        content.setHuggingPriority(.defaultLow, for: .horizontal)
        for view in [header, log, footer] {
            view.widthAnchor.constraint(equalTo: content.widthAnchor, constant: -32)
                .isActive = true
        }
        return content
    }

    private func logScrollView() -> NSScrollView {
        logView.isEditable = false
        logView.isRichText = false
        logView.drawsBackground = true
        logView.backgroundColor = .textBackgroundColor
        logView.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        logView.textContainerInset = NSSize(width: 6, height: 6)
        logView.isVerticallyResizable = true
        logView.isHorizontallyResizable = false
        logView.minSize = NSSize(width: 0, height: 0)
        logView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: .greatestFiniteMagnitude)
        logView.autoresizingMask = [.width]
        logView.textContainer?.widthTracksTextView = true
        logView.textContainer?.containerSize = NSSize(
            width: 0, height: CGFloat.greatestFiniteMagnitude)

        let scroll = NSScrollView()
        scroll.documentView = logView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 240).isActive = true
        return scroll
    }

    // MARK: Rendering

    private func render() {
        let copy = UpdateCopy(build: build, state: model.state)
        installedLabel.stringValue = "\(copy.heading): \(build.versionLine)\(builtSuffix())"
        sourceLabel.stringValue = copy.source
        renderState(copy)
        renderLog()
    }

    private func renderState(_ copy: UpdateCopy) {
        if copy.spinning {
            spinner.startAnimation(nil)
        } else {
            spinner.stopAnimation(nil)
        }
        statusLabel.stringValue = copy.status
        statusLabel.toolTip = copy.statusToolTip
        actionButton.title = copy.actionTitle
        actionButton.isEnabled = copy.actionEnabled
        actionButton.keyEquivalent = copy.actionCancels ? "\u{1b}" : "\r"
    }

    /// Replaces the whole log and pins the view to the bottom: the log only ever grows,
    /// and a few thousand lines of build output cost less to set than to splice.
    private func renderLog() {
        let text = model.log.joined(separator: "\n")
        guard logView.string != text else { return }
        logView.string = text
        logView.scrollToEndOfDocument(nil)
    }

    private func builtSuffix() -> String {
        guard let date = build.buildDate else { return "" }
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return " · built \(formatter.string(from: date))"
    }
}

/// Stands in when the bundle carries no source checkout: every call reports why, and the
/// model stops at preflight before reaching any of them.
private struct UnavailableUpdateEnvironment: UpdateEnvironment {
    private static let reason = "this bundle carries no source checkout"

    func toolchain() async -> ToolchainCheck { .failure(Self.reason) }
    func sourceState() async -> SourceStateResult { .failure(Self.reason) }
    func sync() async -> String? { Self.reason }
    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? { Self.reason }
    func handOff() async {}
    func reclaim() async {}
    func relaunch() async -> String? { Self.reason }
    func quit() async {}
}
