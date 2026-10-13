/// Every string and control state the update window shows, decided for one bundle and one
/// `UpdateState`. The window assigns these fields and decides nothing.
public struct UpdateCopy: Sendable, Hashable {
    /// The window title.
    public let title: String
    /// The word before the version line: what the version describes.
    public let heading: String
    /// What the button will build from.
    public let source: String
    /// The line beside the button.
    public let status: String
    /// The full failure message, which the status line may truncate; `nil` outside
    /// `.failed`, so no earlier failure lingers on a later state.
    public let statusToolTip: String?
    public let actionTitle: String
    public let actionEnabled: Bool
    /// Whether work is under way: running a step, or relaunching.
    public let spinning: Bool

    public init(build: AppBuild, state: UpdateState) {
        let rebuild = build.updateMode == .rebuild
        title = rebuild ? "Rebuild \(build.name)" : "Update \(build.name)"
        heading = rebuild ? "Running" : "Installed"
        source = Self.source(build: build, rebuild: rebuild)
        status = Self.status(state, rebuild: rebuild)
        if case let .failed(_, message) = state {
            statusToolTip = message
        } else {
            statusToolTip = nil
        }
        actionTitle = Self.actionTitle(state, rebuild: rebuild)
        actionEnabled = UpdateModel.canProceed(build: build, state: state)
        switch state {
        case .running, .installed: spinning = true
        case .idle, .upToDate, .failed: spinning = false
        }
    }

    private static func source(build: AppBuild, rebuild: Bool) -> String {
        guard let sourceRoot = build.sourceRoot else {
            return "This bundle was not built by scripts/build-app.sh, so it cannot rebuild itself."
        }
        return rebuild
            ? "Rebuilds \(sourceRoot) as it stands now — no fetch, uncommitted work included."
            : "Builds from \(sourceRoot), fast-forwarded from its upstream first."
    }

    private static func status(_ state: UpdateState, rebuild: Bool) -> String {
        switch state {
        case .idle:
            rebuild
                ? "Rebuild and relaunch. Tabs come back."
                : "Fetch, rebuild, install, relaunch. Tabs come back."
        case let .running(step): running(step, rebuild: rebuild)
        case .upToDate: "Already at the commit this build came from."
        case .installed: "\(rebuild ? "Built" : "Installed"). Relaunching…"
        case let .failed(step, message): "\(name(of: step, rebuild: rebuild)) failed: \(message)"
        }
    }

    private static func actionTitle(_ state: UpdateState, rebuild: Bool) -> String {
        switch state {
        case .upToDate: "Rebuild Anyway"
        case .failed: "Try Again"
        case .idle, .running, .installed: rebuild ? "Rebuild and Relaunch" : "Update and Relaunch"
        }
    }

    private static func running(_ step: UpdateStep, rebuild: Bool) -> String {
        switch step {
        case .preflight: "Checking the toolchain and the checkout…"
        case .sync: "Fetching and fast-forwarding the checkout…"
        case .install:
            rebuild
                ? "Building the dev bundle — this takes a moment…"
                : "Building and installing — this takes a few minutes…"
        }
    }

    private static func name(of step: UpdateStep, rebuild: Bool) -> String {
        switch step {
        case .preflight: "Preflight"
        case .sync: "Fetch"
        case .install: rebuild ? "Build" : "Install"
        }
    }
}
