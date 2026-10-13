import AppKit
import Foundation
import RcGitKit

/// The real `UpdateEnvironment`: a login shell for the toolchain and the build, the core
/// for the checkout's state, `SyncModel` for the fetch and fast-forward — the same guarded
/// path the Changes tab pulls through — and `NSWorkspace` for the relaunch. A `.rebuild`
/// never reaches `sync`, so the dev loop needs no network and no credentials.
struct AppUpdateEnvironment: UpdateEnvironment {
    /// What the build needs and macOS does not ship.
    static let requiredTools = ["cargo", "swift", "cbindgen"]
    /// A probe that cannot answer in this long is stuck, not slow: it is three `command -v`
    /// builtins in a login shell. Killing it turns a window that spins forever into one
    /// that names what to try in a terminal.
    static let toolchainTimeoutSeconds = 20
    /// A build that prints nothing for this long is waiting, not working: cargo and swift
    /// print steadily, while one blocked on another build's lock prints a line and stops.
    static let buildIdleLimitMinutes = 10
    /// The most a build may take in all, idle or not: several times a cold release build.
    static let buildLimitMinutes = 45

    let sourceRoot: String
    /// What this update produces, which decides the script that runs and whether the
    /// checkout is fetched first.
    let mode: UpdateMode
    /// The bundle to launch once the build finishes.
    let relaunchPath: String
    /// The window sheets attach to, resolved when a prompt is actually needed.
    let window: @MainActor @Sendable () -> NSWindow?
    /// Runs on the main actor just before the new bundle is launched.
    let handOffState: @MainActor @Sendable () -> Void
    /// Runs on the main actor when the launch failed and this process keeps running.
    let reclaimState: @MainActor @Sendable () -> Void

    /// Runs `script` through a login `zsh`, so the toolchain on `PATH` is the one the
    /// user's terminal has — rustup and Homebrew both install themselves into a shell
    /// profile, and an app launched from the Dock inherits none of it.
    private static func login(
        _ script: String, in directory: String, timeout: Duration? = nil,
        idleTimeout: Duration? = nil, onOutput: (@Sendable (String) -> Void)? = nil
    ) async -> Shell.Outcome {
        await Shell.run(
            "/bin/zsh", ["-lc", script], in: directory, timeout: timeout,
            idleTimeout: idleTimeout, onOutput: onOutput)
    }

    func toolchain() async -> ToolchainCheck {
        let tools = Self.requiredTools.joined(separator: " ")
        let outcome = await Self.login(
            "for tool in \(tools); do command -v $tool >/dev/null || echo $tool; done",
            in: sourceRoot, timeout: .seconds(Self.toolchainTimeoutSeconds))
        if outcome.stopped != nil {
            return .failure(
                "the toolchain check did not answer in \(Self.toolchainTimeoutSeconds)s and was "
                    + "killed; your login shell is hanging — run "
                    + "`zsh -lc 'command -v \(Self.requiredTools[0])'` in a terminal to see where")
        }
        let missing = outcome.lines.filter { Self.requiredTools.contains($0) }
        guard missing.isEmpty else { return .missing(missing) }
        guard outcome.succeeded else {
            let lastLine = outcome.lines.last(where: { !$0.isEmpty }) ?? "no output"
            return .failure(
                "your login shell \(outcome.failure) during the toolchain check: \(lastLine)")
        }
        return .ready
    }

    /// `refs/heads/master` as `master`; `nil` on a detached HEAD.
    static func shortBranch(_ refName: String?) -> String? {
        guard let refName else { return nil }
        let prefix = "refs/heads/"
        return refName.hasPrefix(prefix) ? String(refName.dropFirst(prefix.count)) : refName
    }

    func sourceState() async -> SourceStateResult {
        let root = sourceRoot
        return await Task.detached(priority: .userInitiated) {
            do {
                let repository = try Repository(path: root)
                let head = try repository.head()
                guard let commit = head.target else {
                    return .failure("the checkout at \(root) has no commits")
                }
                let report = try repository.worktreeStatus(tree: repository.treeOf(commit))
                return .state(
                    SourceState(
                        commit: commit.hex,
                        branch: AppUpdateEnvironment.shortBranch(head.refName),
                        report: report))
            } catch {
                return .failure("cannot read the checkout at \(root): \(error)")
            }
        }.value
    }

    /// A cancel reaches the pull through `SyncModel.cancelRunning`, the same stop the
    /// Changes tab's Cancel uses.
    func sync() async -> String? {
        let model = await MainActor.run { makeSyncModel() }
        let reconcile = ReconcileCommand.rebase(workDir: sourceRoot)
        await withTaskCancellationHandler {
            await model.refreshStatus()
            guard !Task.isCancelled else { return }
            await model.pull()
        } onCancel: {
            Task { @MainActor in model.cancelRunning() }
        }
        return await MainActor.run {
            switch model.state {
            case .done:
                return nil
            case let .diverged(message):
                return "\(message); run `\(reconcile)` in a terminal, then update again"
            case let .failed(_, message):
                return message
            default:
                return "the pull did not finish"
            }
        }
    }

    /// The script this mode runs, relative to the checkout, and the arguments it takes.
    private var buildScript: (path: String, arguments: String) {
        switch mode {
        case .update: ("scripts/install-app.sh", "")
        case .rebuild: ("scripts/build-app.sh", " --dev")
        }
    }

    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? {
        let script = buildScript
        let command = "exec \(ShellWord.quote("\(sourceRoot)/\(script.path)"))\(script.arguments)"
        let outcome = await Self.login(
            command, in: sourceRoot, timeout: .seconds(Self.buildLimitMinutes * 60),
            idleTimeout: .seconds(Self.buildIdleLimitMinutes * 60), onOutput: onOutput)
        let lastLine = outcome.lines.last(where: { !$0.isEmpty }) ?? "no output"
        switch outcome.stopped {
        case .idle:
            return "stopped after \(Self.buildIdleLimitMinutes) min without output; "
                + "last line: \(lastLine)"
        case .timedOut:
            return "stopped after \(Self.buildLimitMinutes) min, the build's limit; "
                + "last line: \(lastLine)"
        case .cancelled:
            return "cancelled"
        case nil:
            guard !outcome.succeeded else { return nil }
            return "\(script.path)\(script.arguments) \(outcome.failure): \(lastLine)"
        }
    }

    func handOff() async {
        await MainActor.run { handOffState() }
    }

    func reclaim() async {
        await MainActor.run { reclaimState() }
    }

    func relaunch() async -> String? {
        let target = URL(fileURLWithPath: relaunchPath)
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.createsNewApplicationInstance = true
        configuration.activates = true
        do {
            _ = try await NSWorkspace.shared.openApplication(
                at: target, configuration: configuration)
        } catch {
            return "built, but could not launch \(target.path): \(error)"
        }
        return nil
    }

    /// `terminate` never returns here (the delegate answers `.terminateNow`), so it is
    /// deferred a run-loop turn to let the window draw `.installed` first.
    func quit() async {
        await MainActor.run {
            NSApp.perform(#selector(NSApplication.terminate(_:)), with: nil, afterDelay: 0)
        }
    }

    @MainActor
    private func makeSyncModel() -> SyncModel {
        let model = SyncModel(
            repoPath: sourceRoot, performer: GuardSyncPerformer(),
            store: AppDelegate.credentialStore)
        let window = window
        model.onCredentialsNeeded = { host, kind, key, resume in
            SyncPrompts.credentials(
                on: window(), host: host, kind: kind, key: key, completion: resume)
        }
        model.onHostKeyNeeded = { host, hostKey, resume in
            SyncPrompts.hostKey(on: window(), host: host, hostKey: hostKey, completion: resume)
        }
        return model
    }
}
