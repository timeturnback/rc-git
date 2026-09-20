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
    }

    /// Runs `script`, streaming merged stdout and stderr one line at a time.
    static func run(
        _ script: String, in directory: String,
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
        let exit = ExitSignal()
        process.terminationHandler = { _ in exit.signal() }
        pipe.fileHandleForReading.readabilityHandler = { handle in
            collector.take(handle.availableData)
        }
        do {
            try process.run()
        } catch {
            pipe.fileHandleForReading.readabilityHandler = nil
            collector.take(Data("cannot run /bin/zsh: \(error)\n".utf8))
            return Outcome(status: -1, lines: collector.finish())
        }
        await exit.wait()
        pipe.fileHandleForReading.readabilityHandler = nil
        collector.take(pipe.fileHandleForReading.readDataToEndOfFile())
        return Outcome(status: process.terminationStatus, lines: collector.finish())
    }

    /// Single-quotes a path for the shell, so a directory with a space still runs.
    static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
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

/// One-shot process-exit signal. The termination handler is installed before the process
/// runs, so a process that exits immediately cannot be missed.
private final class ExitSignal: @unchecked Sendable {
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
}

/// The real `UpdateEnvironment`: a login shell for the toolchain and the build, the core
/// for the checkout's state, `SyncModel` for the fetch and fast-forward — the same guarded
/// path the Changes tab pulls through — and `NSWorkspace` for the relaunch.
struct AppUpdateEnvironment: UpdateEnvironment {
    /// What the build needs and macOS does not ship.
    static let requiredTools = ["cargo", "swift", "cbindgen"]

    let sourceRoot: String
    /// The window sheets attach to, resolved when a prompt is actually needed.
    let window: @MainActor @Sendable () -> NSWindow?
    /// Runs on the main actor just before the new bundle is launched.
    let willRelaunch: @MainActor @Sendable () -> Void

    func missingTools() async -> [String] {
        let tools = Self.requiredTools.joined(separator: " ")
        let outcome = await Shell.run(
            "for tool in \(tools); do command -v $tool >/dev/null || echo $tool; done",
            in: sourceRoot)
        return outcome.lines.filter { Self.requiredTools.contains($0) }
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
        await model.refreshStatus()
        await model.pull()
        return await MainActor.run {
            switch model.state {
            case .done:
                return nil
            case let .diverged(message):
                return "\(message); reconcile the checkout in git, then update again"
            case let .failed(_, message):
                return message
            default:
                return "the pull did not finish"
            }
        }
    }

    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? {
        let script = Shell.quote("\(sourceRoot)/scripts/install-app.sh")
        let outcome = await Shell.run("exec \(script)", in: sourceRoot, onOutput: onOutput)
        guard outcome.status != 0 else { return nil }
        let lastLine = outcome.lines.last(where: { !$0.isEmpty }) ?? "no output"
        return "scripts/install-app.sh failed (status \(outcome.status)): \(lastLine)"
    }

    func relaunch() async -> String? {
        await MainActor.run { willRelaunch() }
        let installed = URL(fileURLWithPath: AppBuild.installedPath)
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.createsNewApplicationInstance = true
        configuration.activates = true
        do {
            _ = try await NSWorkspace.shared.openApplication(
                at: installed, configuration: configuration)
        } catch {
            return "installed, but could not launch \(installed.path): \(error)"
        }
        await MainActor.run { NSApp.terminate(nil) }
        return nil
    }

    @MainActor
    private func makeSyncModel() -> SyncModel {
        let model = SyncModel(
            repoPath: sourceRoot, performer: GuardSyncPerformer(),
            store: KeychainCredentialStore())
        let window = window
        model.onCredentialsNeeded = { host, kind, resume in
            SyncPrompts.credentials(on: window(), host: host, kind: kind, completion: resume)
        }
        model.onHostKeyNeeded = { host, hostKey, resume in
            SyncPrompts.hostKey(on: window(), host: host, hostKey: hostKey, completion: resume)
        }
        return model
    }
}
