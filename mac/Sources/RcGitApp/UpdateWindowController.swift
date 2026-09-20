import AppKit
import RcGitKit

/// The "Update rc-git" window: what is installed, what the update will do, a live build
/// log, and one button that fetches, fast-forwards, rebuilds, installs, and relaunches.
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
    private let actionButton = NSButton(title: "Update and Relaunch", target: nil, action: nil)
    private let logView = NSTextView()

    /// `saveState` runs just before the new bundle is launched, so the tabs the relaunched
    /// app reopens are the ones on screen now.
    init(build: AppBuild, saveState: @escaping @MainActor @Sendable () -> Void) {
        self.build = build
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 620, height: 440),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        var environment: AppUpdateEnvironment?
        if let sourceRoot = build.sourceRoot {
            environment = AppUpdateEnvironment(
                sourceRoot: sourceRoot,
                window: { [weak window] in window },
                willRelaunch: saveState)
        }
        model = UpdateModel(
            build: build, environment: environment ?? UnavailableUpdateEnvironment())
        super.init(window: window)
        window.title = "Update rc-git"
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

    @objc private func runUpdate(_ sender: Any?) {
        let force = model.state == .upToDate
        if case .failed = model.state { model.reset() }
        Task { await model.start(force: force) }
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
        actionButton.keyEquivalent = "\r"
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
        installedLabel.stringValue = "Installed: \(build.versionLine)\(builtSuffix())"
        sourceLabel.stringValue =
            build.sourceRoot.map { "Builds from \($0), fast-forwarded from its upstream first." }
            ?? "This bundle was not built by scripts/build-app.sh, so it cannot update itself."
        renderState()
        renderLog()
    }

    private func renderState() {
        switch model.state {
        case .idle:
            spinner.stopAnimation(nil)
            statusLabel.stringValue = "Fetch, rebuild, install, relaunch. Tabs come back."
            actionButton.title = "Update and Relaunch"
            actionButton.isEnabled = build.canSelfUpdate
        case let .running(step):
            spinner.startAnimation(nil)
            statusLabel.stringValue =
                switch step {
                case .preflight: "Checking the toolchain and the checkout…"
                case .sync: "Fetching and fast-forwarding the checkout…"
                case .install: "Building and installing — this takes a few minutes…"
                }
            actionButton.isEnabled = false
        case .upToDate:
            spinner.stopAnimation(nil)
            statusLabel.stringValue = "Already at the commit this build came from."
            actionButton.title = "Rebuild Anyway"
            actionButton.isEnabled = true
        case .installed:
            spinner.startAnimation(nil)
            statusLabel.stringValue = "Installed. Relaunching…"
            actionButton.isEnabled = false
        case let .failed(step, message):
            spinner.stopAnimation(nil)
            statusLabel.stringValue = "\(Self.name(of: step)) failed: \(message)"
            statusLabel.toolTip = message
            actionButton.title = "Try Again"
            actionButton.isEnabled = build.canSelfUpdate
        }
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

    private static func name(of step: UpdateStep) -> String {
        switch step {
        case .preflight: "Preflight"
        case .sync: "Fetch"
        case .install: "Install"
        }
    }
}

/// Stands in when the bundle carries no source checkout: every call reports why, and the
/// model stops at preflight before reaching any of them.
private struct UnavailableUpdateEnvironment: UpdateEnvironment {
    private static let reason = "this bundle carries no source checkout"

    func missingTools() async -> [String] { [] }
    func sourceState() async -> SourceStateResult { .failure(Self.reason) }
    func sync() async -> String? { Self.reason }
    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? { Self.reason }
    func relaunch() async -> String? { Self.reason }
}
