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
    /// Whether the status scan covered the whole working tree. A scan stopped at its cap
    /// may have missed a dirty path, so it never counts as clean.
    public let scanComplete: Bool

    public init(
        commit: String, branch: String?, dirtyPaths: [String], scanComplete: Bool = true
    ) {
        self.commit = commit
        self.branch = branch
        self.dirtyPaths = dirtyPaths
        self.scanComplete = scanComplete
    }

    /// The state a working-tree status scan describes: its dirty paths, and whether it
    /// finished.
    public init(commit: String, branch: String?, report: StatusReport) {
        self.init(
            commit: commit, branch: branch, dirtyPaths: Self.dirtyPaths(in: report),
            scanComplete: !report.truncated)
    }

    /// The paths an update treats as uncommitted work: modified or deleted tracked files.
    public static func dirtyPaths(in report: StatusReport) -> [String] {
        report.entries
            .filter { $0.status == .modified || $0.status == .deleted }
            .map(\.path)
    }

    /// No uncommitted work, and a scan that covered the whole tree to say so.
    public var isClean: Bool { dirtyPaths.isEmpty && scanComplete }
}

/// What reading a checkout produced: its state, or why it could not be read.
public enum SourceStateResult: Sendable, Hashable {
    case state(SourceState)
    case failure(String)
}

/// What probing the toolchain produced.
public enum ToolchainCheck: Sendable, Hashable {
    /// Every tool the build needs is on the login shell's `PATH`.
    case ready
    /// Tools the login shell cannot find.
    case missing([String])
    /// The probe itself did not answer: the shell could not run, it failed, or it ran past
    /// its limit and was killed. A probe that hangs is a failure, not a wait.
    case failure(String)
}

/// Everything a self-update touches outside the model: the toolchain, the checkout, the
/// build script, and the relaunch. The app supplies the real implementation; tests supply
/// a double, so the state machine is tested without building anything.
///
/// Every call reports failure as a message rather than throwing: the model turns it into
/// `UpdateState.failed` with the step it belongs to.
public protocol UpdateEnvironment: Sendable {
    /// Whether the login shell can find every tool the build needs. Bounded by the
    /// implementation: it answers rather than hanging.
    func toolchain() async -> ToolchainCheck
    /// The checkout's commit and dirty paths, or why they could not be read.
    func sourceState() async -> SourceStateResult
    /// Fetches and fast-forwards the checkout. `nil` on success. Stops early when the
    /// calling Task is cancelled.
    func sync() async -> String?
    /// Builds and installs the release bundle, streaming the script's output. `nil` on
    /// success. Bounded by the implementation, and stops — killing the build — when the
    /// calling Task is cancelled.
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
    /// The attempt under way, kept so `cancel()` can stop it.
    private var running: Task<Void, Never>?
    /// Counts attempts, so a line a cancelled build prints late never lands in the next
    /// attempt's log.
    private var attempt = 0

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
        guard !isBusy, running == nil else { return }
        attempt += 1
        log = []
        // Cleared inside the task, in the same turn that settles the state, so the model is
        // never idle while still holding a finished attempt.
        let task = Task {
            await run(force: force)
            running = nil
        }
        running = task
        await task.value
    }

    /// Stops the attempt under way: the fetch or the build is killed, and the update ends
    /// `.failed` with "cancelled", so Try Again starts clean. A no-op when nothing runs.
    public func cancel() {
        running?.cancel()
    }

    /// Whether the button stops the update rather than starting one: while the fetch or
    /// the build runs. Preflight is a bounded probe and is left to finish.
    nonisolated public static func isCancellable(state: UpdateState) -> Bool {
        switch state {
        case .running(.sync), .running(.install): true
        case .running(.preflight), .idle, .upToDate, .installed, .failed: false
        }
    }

    /// What the window's button does: cancel while the fetch or the build runs, otherwise
    /// `proceed()`. The shortcut calls `proceed()` alone, so it never cancels.
    public func pressAction() async {
        if Self.isCancellable(state: state) {
            cancel()
        } else {
            await proceed()
        }
    }

    private func run(force: Bool) async {
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

    /// Whether `proceed()` would start anything: the bundle can rebuild itself, and the
    /// update is neither running nor already relaunching.
    public var canProceed: Bool { Self.canProceed(build: build, state: state) }

    /// The one rule behind `canProceed`, so the window's button and the model agree.
    nonisolated public static func canProceed(build: AppBuild, state: UpdateState) -> Bool {
        guard build.updateMode != nil else { return false }
        switch state {
        case .idle, .upToDate, .failed: return true
        case .running, .installed: return false
        }
    }

    /// What the window's button and its shortcut do: start from `.idle`, rebuild anyway
    /// from `.upToDate`, start over from `.failed`, and nothing otherwise.
    public func proceed() async {
        guard canProceed else { return }
        switch state {
        case .upToDate:
            await start(force: true)
        case .failed:
            reset()
            await start()
        case .idle:
            await start()
        case .running, .installed:
            return
        }
    }

    /// `a.rs, b.rs and 3 more` — enough to recognize the work without a wall of paths.
    public static func list(_ paths: [String], limit: Int = 3) -> String {
        guard paths.count > limit else { return paths.joined(separator: ", ") }
        let shown = paths.prefix(limit).joined(separator: ", ")
        return "\(shown) and \(paths.count - limit) more"
    }

    /// Checks the toolchain and reads the checkout. `nil` when the update must stop; the
    /// state already says why.
    ///
    /// Both halves are logged before they run, not after: the log is the only thing that
    /// says which one an update is sitting in.
    private func preflight(root: String) async -> SourceState? {
        enter(.preflight)
        note("checkout: \(root)")
        note("checking the toolchain")
        switch await environment.toolchain() {
        case .ready:
            break
        case let .missing(tools):
            fail(
                .preflight,
                "your login shell cannot find \(tools.joined(separator: ", ")); "
                    + "add the toolchain to your shell profile and try again")
            return nil
        case let .failure(message):
            fail(.preflight, message)
            return nil
        }
        note("reading the checkout")
        guard let state = await readSource(step: .preflight) else { return nil }
        note("on \(state.branch ?? "a detached HEAD")")
        return state
    }

    /// The `.update` half: refuse uncommitted work, fetch and fast-forward, then compare
    /// the result with the commit this bundle was built from. `false` when the update must
    /// stop, either because a step failed or because there is nothing new to build.
    private func fastForward(from before: SourceState, force: Bool) async -> Bool {
        guard before.isClean else {
            fail(.preflight, Self.uncleanMessage(before))
            return false
        }
        enter(.sync)
        note("fetching and fast-forwarding")
        let message = await environment.sync()
        guard !stoppedByCancel(.sync) else { return false }
        if let message {
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
        let current = attempt
        let message = await environment.install(onOutput: { [weak self] line in
            Task { @MainActor in
                guard let self, self.attempt == current else { return }
                self.note(line)
            }
        })
        guard !stoppedByCancel(.install) else { return }
        if let message {
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

    /// Why a checkout that is not clean stops the update: the paths when there are any,
    /// otherwise the scan that could not finish.
    private static func uncleanMessage(_ state: SourceState) -> String {
        guard state.dirtyPaths.isEmpty else {
            return "the checkout has uncommitted work: \(list(state.dirtyPaths))"
        }
        return "the status scan stopped at its cap before covering the checkout, so "
            + "uncommitted work cannot be ruled out; run scripts/install-app.sh in a terminal"
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

    /// Ends the update as cancelled when `cancel()` landed during `step`. Checked after the
    /// step returns, so a build that finished at the moment of the cancel still never
    /// relaunches.
    private func stoppedByCancel(_ step: UpdateStep) -> Bool {
        guard Task.isCancelled else { return false }
        fail(step, "cancelled")
        return true
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
