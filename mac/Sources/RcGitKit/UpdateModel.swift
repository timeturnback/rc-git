import Foundation

/// The stages of a self-update, in the order they run.
public enum UpdateStep: Sendable, Hashable {
    /// Toolchain and checkout checks. Nothing has been touched yet.
    case preflight
    /// Fetch and fast-forward the checkout, through the guard, like any other pull.
    /// `.rebuild` skips it.
    case sync
    /// Build the bundle and put it in place.
    case install
}

/// What an update does, which is decided by the bundle running it.
public enum UpdateMode: Sendable, Hashable {
    /// The release bundle: fetch, fast-forward, rebuild, install into `/Applications`,
    /// relaunch. Refuses a dirty checkout, and stops early when the commit has not moved.
    case update
    /// The dev bundle: rebuild from the working tree as it stands and relaunch. No fetch
    /// and no dirty check — putting work in progress into the running app is the point.
    case rebuild
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

/// Drives one update: preflight, fetch and fast-forward, build, install, relaunch. The
/// bundle's `UpdateMode` decides whether the fetch happens at all. Stops at the first
/// failure and keeps the log, so the window can show what happened.
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
    /// commit this bundle was built from; it means nothing in `.rebuild`, which always
    /// builds.
    public func start(force: Bool = false) async {
        guard !isBusy else { return }
        log = []
        guard let mode = build.updateMode, let root = build.sourceRoot else {
            fail(.preflight, Self.unstampedMessage(build.variant))
            return
        }
        guard let before = await preflight(root: root) else { return }
        switch mode {
        case .update:
            guard await fastForward(from: before, force: force) else { return }
        case .rebuild:
            if !before.dirtyPaths.isEmpty {
                note("uncommitted work goes in: \(Self.list(before.dirtyPaths))")
            }
        }
        await buildAndRelaunch()
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

    /// Checks the toolchain and reads the checkout. `nil` when the update must stop; the
    /// state already says why.
    private func preflight(root: String) async -> SourceState? {
        enter(.preflight)
        note("checkout: \(root)")
        let missing = await environment.missingTools()
        guard missing.isEmpty else {
            fail(
                .preflight,
                "your login shell cannot find \(missing.joined(separator: ", ")); "
                    + "add the toolchain to your shell profile and try again")
            return nil
        }
        guard let state = await readSource(step: .preflight) else { return nil }
        note("on \(state.branch ?? "a detached HEAD")")
        return state
    }

    /// The `.update` half: refuse uncommitted work, fetch and fast-forward, then compare
    /// the result with the commit this bundle was built from. `false` when the update must
    /// stop, either because a step failed or because there is nothing new to build.
    private func fastForward(from before: SourceState, force: Bool) async -> Bool {
        guard before.dirtyPaths.isEmpty else {
            fail(.preflight, "the checkout has uncommitted work: \(Self.list(before.dirtyPaths))")
            return false
        }
        enter(.sync)
        note("fetching and fast-forwarding")
        if let message = await environment.sync() {
            fail(.sync, message)
            return false
        }
        guard let after = await readSource(step: .sync) else { return false }
        note("checkout is at \(String(after.commit.prefix(7)))")
        if !force, after.commit == build.commit {
            state = .upToDate
            onChanged?()
            return false
        }
        return true
    }

    /// Builds the bundle, puts it in place, and launches it.
    private func buildAndRelaunch() async {
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

    /// A bundle with no `RCGitSourceRoot` stamp cannot know what to build; the message
    /// names the script that stamps the variant it claims to be.
    private static func unstampedMessage(_ variant: AppVariant) -> String {
        "this bundle carries no source checkout; rebuild it with "
            + (variant == .dev ? "scripts/build-app.sh --dev" : "scripts/install-app.sh")
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
