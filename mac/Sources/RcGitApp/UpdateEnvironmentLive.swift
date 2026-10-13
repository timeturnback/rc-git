import AppKit
import Foundation
import RcGitKit

/// Runs shell work for the updater. Commands go through a login `zsh` so the toolchain on
/// `PATH` is the one the user's terminal has — rustup and Homebrew both install themselves
/// into a shell profile, and an app launched from the Dock inherits none of it.
enum Shell {
    struct Outcome: Sendable {
        let status: Int32
        let lines: [String]
        /// Whether the command ran past its limit and was killed.
        let timedOut: Bool
    }

    /// How long the last bytes get to arrive after the process exits. A grandchild that
    /// inherited the pipe keeps it open, so this wait is bounded like every other.
    private static let drainGrace = Duration.seconds(2)
    /// How long a killed process gets to die before `SIGKILL`.
    private static let killGrace = Duration.seconds(2)

    /// Runs `script`, streaming merged stdout and stderr one line at a time. With a
    /// `timeout`, a command still running at the limit is killed and the outcome says so:
    /// nothing here waits forever.
    static func run(
        _ script: String, in directory: String, timeout: Duration? = nil,
        onOutput: (@Sendable (String) -> Void)? = nil
    ) async -> Outcome {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/zsh")
        process.arguments = ["-lc", script]
        process.currentDirectoryURL = URL(fileURLWithPath: directory)
        process.standardInput = FileHandle.nullDevice
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        let collector = LineCollector(onOutput: onOutput)
        let exited = Signal()
        process.terminationHandler = { _ in exited.signal() }
        do {
            try process.run()
        } catch {
            collector.take(Data("cannot run /bin/zsh: \(error)\n".utf8))
            return Outcome(status: -1, lines: collector.finish(), timedOut: false)
        }

        let drained = drain(pipe.fileHandleForReading, into: collector)
        let expired = Flag()
        let watchdog = timeout.map { limit in
            kill(process.processIdentifier, after: limit, recording: expired)
        }
        await exited.wait()
        watchdog?.cancel()
        await drained.wait(upTo: drainGrace)
        return Outcome(
            status: process.terminationStatus, lines: collector.finish(), timedOut: expired.isSet)
    }

    /// Reads the pipe to EOF on a thread of its own. `availableData` blocks, and a blocked
    /// cooperative thread stalls every other task in the app; one reader also keeps this
    /// off the `readabilityHandler`/`readDataToEndOfFile` pair, which read the same
    /// descriptor from two places. The thread ends at EOF.
    private static func drain(_ handle: FileHandle, into collector: LineCollector) -> Signal {
        let finished = Signal()
        Thread.detachNewThread {
            while true {
                let data = handle.availableData
                if data.isEmpty { break }
                collector.take(data)
            }
            finished.signal()
        }
        return finished
    }

    /// Kills `pid` once `limit` has passed: `SIGTERM`, then `SIGKILL` for a process that
    /// ignores it. Cancelled the moment the process exits on its own.
    private static func kill(
        _ pid: Int32, after limit: Duration, recording expired: Flag
    ) -> Task<Void, Never> {
        Task {
            try? await Task.sleep(for: limit)
            guard !Task.isCancelled else { return }
            expired.set()
            Darwin.kill(pid, SIGTERM)
            try? await Task.sleep(for: killGrace)
            guard !Task.isCancelled else { return }
            Darwin.kill(pid, SIGKILL)
        }
    }
}

/// A one-shot flag, set from whichever thread gets there first.
private final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false

    func set() {
        lock.lock()
        value = true
        lock.unlock()
    }

    var isSet: Bool {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

/// Splits streamed bytes into lines. Buffers on `Data`, not `String`, so a multi-byte
/// character split across two reads survives. Reads arrive on a background queue.
private final class LineCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer = Data()
    private var lines: [String] = []
    private let onOutput: (@Sendable (String) -> Void)?

    init(onOutput: (@Sendable (String) -> Void)?) {
        self.onOutput = onOutput
    }

    func take(_ data: Data) {
        guard !data.isEmpty else { return }
        var complete: [String] = []
        lock.lock()
        buffer.append(data)
        while let newline = buffer.firstIndex(of: 0x0A) {
            complete.append(String(decoding: buffer[buffer.startIndex..<newline], as: UTF8.self))
            buffer.removeSubrange(buffer.startIndex...newline)
        }
        lines.append(contentsOf: complete)
        lock.unlock()
        for line in complete { onOutput?(line) }
    }

    /// Flushes a last line without a newline and returns everything collected.
    func finish() -> [String] {
        lock.lock()
        var trailing: String?
        if !buffer.isEmpty {
            trailing = String(decoding: buffer, as: UTF8.self)
            buffer.removeAll()
            lines.append(trailing ?? "")
        }
        let collected = lines
        lock.unlock()
        if let trailing { onOutput?(trailing) }
        return collected
    }
}

/// A one-shot signal, awaited once. Both users install it before the thing that fires it
/// starts, so an event that happens immediately cannot be missed.
private final class Signal: @unchecked Sendable {
    private let lock = NSLock()
    private var signaled = false
    private var continuation: CheckedContinuation<Void, Never>?

    func signal() {
        lock.lock()
        signaled = true
        let waiting = continuation
        continuation = nil
        lock.unlock()
        waiting?.resume()
    }

    func wait() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            lock.lock()
            if signaled {
                lock.unlock()
                continuation.resume()
                return
            }
            self.continuation = continuation
            lock.unlock()
        }
    }

    /// Waits, giving up after `limit`. Giving up signals the signal, which is one-shot and
    /// has no other waiter, so nothing is left dangling.
    func wait(upTo limit: Duration) async {
        let timer = Task {
            try? await Task.sleep(for: limit)
            guard !Task.isCancelled else { return }
            self.signal()
        }
        await wait()
        timer.cancel()
    }
}

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

    let sourceRoot: String
    /// What this update produces, which decides the script that runs and whether the
    /// checkout is fetched first.
    let mode: UpdateMode
    /// The bundle to launch once the build finishes.
    let relaunchPath: String
    /// The window sheets attach to, resolved when a prompt is actually needed.
    let window: @MainActor @Sendable () -> NSWindow?
    /// Runs on the main actor just before the new bundle is launched.
    let willRelaunch: @MainActor @Sendable () -> Void

    func toolchain() async -> ToolchainCheck {
        let tools = Self.requiredTools.joined(separator: " ")
        let outcome = await Shell.run(
            "for tool in \(tools); do command -v $tool >/dev/null || echo $tool; done",
            in: sourceRoot, timeout: .seconds(Self.toolchainTimeoutSeconds))
        if outcome.timedOut {
            return .failure(
                "the toolchain check did not answer in \(Self.toolchainTimeoutSeconds)s and was "
                    + "killed; your login shell is hanging — run "
                    + "`zsh -lc 'command -v \(Self.requiredTools[0])'` in a terminal to see where")
        }
        let missing = outcome.lines.filter { Self.requiredTools.contains($0) }
        guard missing.isEmpty else { return .missing(missing) }
        guard outcome.status == 0 else {
            let lastLine = outcome.lines.last(where: { !$0.isEmpty }) ?? "no output"
            return .failure(
                "your login shell failed the toolchain check "
                    + "(status \(outcome.status)): \(lastLine)"
            )
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
                let status = try repository.worktreeStatus(tree: repository.treeOf(commit))
                let dirty = status.entries
                    .filter { $0.status == .modified || $0.status == .deleted }
                    .map(\.path)
                return .state(
                    SourceState(
                        commit: commit.hex,
                        branch: AppUpdateEnvironment.shortBranch(head.refName),
                        dirtyPaths: dirty))
            } catch {
                return .failure("cannot read the checkout at \(root): \(error)")
            }
        }.value
    }

    func sync() async -> String? {
        let model = await MainActor.run { makeSyncModel() }
        let reconcile = ReconcileCommand.rebase(workDir: sourceRoot)
        await model.refreshStatus()
        await model.pull()
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
        let outcome = await Shell.run(command, in: sourceRoot, onOutput: onOutput)
        guard outcome.status != 0 else { return nil }
        let lastLine = outcome.lines.last(where: { !$0.isEmpty }) ?? "no output"
        return "\(script.path)\(script.arguments) failed (status \(outcome.status)): \(lastLine)"
    }

    func relaunch() async -> String? {
        await MainActor.run { willRelaunch() }
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
        await MainActor.run { NSApp.terminate(nil) }
        return nil
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
