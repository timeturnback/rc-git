import Foundation

/// The stages of a self-update, in the order they run.
public enum UpdateStep: Sendable, Hashable {
    /// Toolchain and checkout checks. Nothing has been touched yet.
    case preflight
    /// Fetch and fast-forward the checkout, through the guard, like any other pull.
    case sync
    /// Build the release bundle and swap it into `/Applications`.
    case install
}

/// Where a self-update stands.
public enum UpdateState: Sendable, Hashable {
    case idle
    case running(UpdateStep)
    /// The checkout is already at the commit this bundle was built from.
    case upToDate
    /// The new bundle is in place; the app is relaunching into it.
    case installed
    case failed(step: UpdateStep, message: String)
}

/// The commit a checkout is on, the branch it is on, and the tracked paths that differ
/// from it.
public struct SourceState: Sendable, Hashable {
    public let commit: String
    /// Short branch name, or `nil` on a detached HEAD. Logged before the build, because
    /// the update ships whatever the checkout has checked out.
    public let branch: String?
    /// Modified or deleted tracked files. Untracked and ignored files are not listed:
    /// `RCGit.app` and `build/` are build output and must not block an update.
    public let dirtyPaths: [String]

    public init(commit: String, branch: String?, dirtyPaths: [String]) {
        self.commit = commit
        self.branch = branch
        self.dirtyPaths = dirtyPaths
    }
}

/// What reading a checkout produced: its state, or why it could not be read.
public enum SourceStateResult: Sendable, Hashable {
    case state(SourceState)
    case failure(String)
}

/// Everything a self-update touches outside the model: the toolchain, the checkout, the
/// build script, and the relaunch. The app supplies the real implementation; tests supply
/// a double, so the state machine is tested without building anything.
///
/// Every call reports failure as a message rather than throwing: the model turns it into
/// `UpdateState.failed` with the step it belongs to.
public protocol UpdateEnvironment: Sendable {
    /// Tools the build needs that the login shell cannot find. Empty when all are there.
    func missingTools() async -> [String]
    /// The checkout's commit and dirty paths, or why they could not be read.
    func sourceState() async -> SourceStateResult
    /// Fetches and fast-forwards the checkout. `nil` on success.
    func sync() async -> String?
    /// Builds and installs the release bundle, streaming the script's output. `nil` on
    /// success.
    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String?
    /// Launches the installed bundle and quits this instance. `nil` on success.
    func relaunch() async -> String?
}

/// Drives one self-update: preflight, fetch and fast-forward, build, install, relaunch.
/// Stops at the first failure and keeps the log, so the window can show what happened.
///
/// Open tabs are not this model's concern — they live in `UserDefaults` under the bundle
/// identifier, which an update does not change, so the relaunched app reopens them.
@MainActor
public final class UpdateModel {
    /// Kept bounded like every other buffer in the app: a build log is not a reason to
    /// grow without limit.
    public static let maxLogLines = 5000

    public private(set) var state: UpdateState = .idle
    public private(set) var log: [String] = []
    public var onChanged: (() -> Void)?

    private let build: AppBuild
    private let environment: UpdateEnvironment

    public init(build: AppBuild, environment: UpdateEnvironment) {
        self.build = build
        self.environment = environment
    }

    public var isBusy: Bool {
        if case .running = state { return true }
        return false
    }

    /// Runs the whole update. `force` builds even when the checkout is already at the
    /// commit this bundle was built from.
    public func start(force: Bool = false) async {
        guard !isBusy else { return }
        log = []
        guard build.canSelfUpdate, let root = build.sourceRoot else {
            fail(
                .preflight,
                build.variant == .dev
                    ? "this is the dev bundle; update the release app from it instead"
                    : "this bundle carries no source checkout; reinstall with "
                        + "scripts/install-app.sh")
            return
        }

        enter(.preflight)
        note("checkout: \(root)")
        let missing = await environment.missingTools()
        guard missing.isEmpty else {
            fail(
                .preflight,
                "your login shell cannot find \(missing.joined(separator: ", ")); "
                    + "add the toolchain to your shell profile and try again")
            return
        }
        guard let before = await readSource(step: .preflight) else { return }
        guard before.dirtyPaths.isEmpty else {
            fail(.preflight, "the checkout has uncommitted work: \(Self.list(before.dirtyPaths))")
            return
        }
        note("on \(before.branch ?? "a detached HEAD")")

        enter(.sync)
        note("fetching and fast-forwarding")
        if let message = await environment.sync() {
            fail(.sync, message)
            return
        }
        guard let after = await readSource(step: .sync) else { return }
        note("checkout is at \(String(after.commit.prefix(7)))")
        if !force, after.commit == build.commit {
            state = .upToDate
            onChanged?()
            return
        }

        enter(.install)
        if let message = await environment.install(onOutput: { [weak self] line in
            Task { @MainActor in self?.note(line) }
        }) {
            fail(.install, message)
            return
        }
        if let message = await environment.relaunch() {
            fail(.install, message)
            return
        }
        state = .installed
        onChanged?()
    }

    /// Returns terminal states to `.idle` so the window can start a second attempt.
    public func reset() {
        guard !isBusy else { return }
        state = .idle
        onChanged?()
    }

    /// `a.rs, b.rs and 3 more` — enough to recognize the work without a wall of paths.
    public static func list(_ paths: [String], limit: Int = 3) -> String {
        guard paths.count > limit else { return paths.joined(separator: ", ") }
        let shown = paths.prefix(limit).joined(separator: ", ")
        return "\(shown) and \(paths.count - limit) more"
    }

    private func readSource(step: UpdateStep) async -> SourceState? {
        switch await environment.sourceState() {
        case let .state(state):
            return state
        case let .failure(message):
            fail(step, message)
            return nil
        }
    }

    private func enter(_ step: UpdateStep) {
        state = .running(step)
        onChanged?()
    }

    private func fail(_ step: UpdateStep, _ message: String) {
        note("error: \(message)")
        state = .failed(step: step, message: message)
        onChanged?()
    }

    private func note(_ line: String) {
        log.append(line)
        if log.count > Self.maxLogLines {
            log.removeFirst(log.count - Self.maxLogLines)
        }
        onChanged?()
    }
}
