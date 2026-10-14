import Foundation

/// A bounded process runner: merged stdout and stderr streamed one line at a time, a
/// wall-clock limit and an idle limit that kill, a kill on Task cancellation, and a bounded
/// wait for the last bytes. Nothing here waits forever.
///
/// Every kill signals the process group, not the pid: Foundation starts the child as its own
/// group leader, so a script that `exec`s or spawns cargo and swift takes them down with it
/// instead of leaving them holding the build lock.
public enum Shell {
    /// Why a command was stopped before it exited on its own.
    public enum Stop: Sendable, Hashable {
        /// It ran past its wall-clock limit.
        case timedOut
        /// It printed nothing for its idle limit.
        case idle
        /// The Task running it was cancelled.
        case cancelled
    }

    public struct Outcome: Sendable {
        /// The exit status, or the signal number when `reason` is `.uncaughtSignal`.
        public let status: Int32
        /// Whether the process exited or a signal ended it.
        public let reason: Process.TerminationReason
        public let lines: [String]
        /// Why the command was killed; `nil` when it exited on its own.
        public let stopped: Stop?

        /// Whether the process exited on its own with status 0.
        public var succeeded: Bool { reason == .exit && status == 0 }

        /// How a run that did not succeed ended, worded to follow the command's name:
        /// "failed (status 3)" or "was killed by signal 9 (SIGKILL)". A signal number reads
        /// like an exit status, so a build the system killed would otherwise look like one
        /// that failed by itself.
        public var failure: String {
            guard reason == .uncaughtSignal else { return "failed (status \(status))" }
            return "was killed by signal \(status) (\(Shell.signalName(status)))"
        }
    }

    /// The conventional name of a signal number, `SIGKILL` for 9; `SIG<n>` for one not listed.
    static func signalName(_ number: Int32) -> String {
        switch number {
        case SIGHUP: "SIGHUP"
        case SIGINT: "SIGINT"
        case SIGQUIT: "SIGQUIT"
        case SIGABRT: "SIGABRT"
        case SIGKILL: "SIGKILL"
        case SIGBUS: "SIGBUS"
        case SIGSEGV: "SIGSEGV"
        case SIGPIPE: "SIGPIPE"
        case SIGTERM: "SIGTERM"
        default: "SIG\(number)"
        }
    }

    /// How long the last bytes get to arrive after the process exits. A grandchild that
    /// inherited the pipe keeps it open, so this wait is bounded like every other. Bounding
    /// the wait does not end the grandchild or the drain thread: a killed run takes the
    /// grandchild down with its group, and only then does the thread see EOF.
    static let drainGrace = Duration.seconds(2)
    /// How long a killed process group gets to die before `SIGKILL`.
    static let killGrace = Duration.seconds(2)

    /// Runs `executable` with `arguments` in `directory`. A command still running at
    /// `timeout`, silent for `idleTimeout`, or running when the calling Task is cancelled is
    /// killed with its process group, and the outcome says which. `onOutput` gets the
    /// complete lines of each pipe read as one batch, so a burst costs one call, not one
    /// per line.
    public static func run(
        _ executable: String, _ arguments: [String], in directory: String,
        timeout: Duration? = nil, idleTimeout: Duration? = nil,
        onOutput: (@Sendable ([String]) -> Void)? = nil
    ) async -> Outcome {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
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
            collector.take(Data("\(startFailure(executable, in: directory)): \(error)\n".utf8))
            return Outcome(status: -1, reason: .exit, lines: collector.finish(), stopped: nil)
        }

        let drained = drain(pipe.fileHandleForReading, into: collector)
        let killer = GroupKiller(process.processIdentifier, exited: exited)
        let watchdogs = [
            timeout.map { limit in wallClock(limit, killer) },
            idleTimeout.map { limit in idleClock(limit, collector, killer) },
        ].compactMap { $0 }
        await withTaskCancellationHandler {
            await exited.wait()
        } onCancel: {
            killer.stop(.cancelled)
        }
        for watchdog in watchdogs { watchdog.cancel() }
        await killer.finish()
        await drained.wait(upTo: drainGrace)
        return Outcome(
            status: process.terminationStatus, reason: process.terminationReason,
            lines: collector.finish(), stopped: killer.reason)
    }

    /// What a run that could not start blames. A missing working directory fails the start
    /// with the same Cocoa error as a missing executable, so it is named first: blaming the
    /// executable points the reader at the wrong thing.
    static func startFailure(_ executable: String, in directory: String) -> String {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory, isDirectory: &isDirectory),
            isDirectory.boolValue
        else { return "working directory \(directory) does not exist" }
        return "cannot run \(executable)"
    }

    /// Reads the pipe to EOF on a thread of its own. `availableData` blocks, and a blocked
    /// cooperative thread stalls every other task in the app; one reader also keeps this
    /// off the `readabilityHandler`/`readDataToEndOfFile` pair, which read the same
    /// descriptor from two places. The thread ends at EOF, which a grandchild holding the
    /// write end delays until it exits: a kill reaches it through the group, while one left
    /// running by a command that exited on its own keeps this thread until it ends.
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

    /// Stops the command once `limit` has passed. Cancelled the moment it exits on its own.
    private static func wallClock(_ limit: Duration, _ killer: GroupKiller) -> Task<Void, Never> {
        Task {
            // A cancelled sleep is the process exiting first; the guard below handles it.
            try? await Task.sleep(for: limit)
            guard !Task.isCancelled else { return }
            killer.stop(.timedOut)
        }
    }

    /// Stops the command once it has printed nothing for `limit`: a build prints steadily,
    /// while one waiting on a lock prints a line and then nothing. Sleeps until the moment
    /// the limit would run out, and re-arms when output arrived in the meantime.
    private static func idleClock(
        _ limit: Duration, _ collector: LineCollector, _ killer: GroupKiller
    ) -> Task<Void, Never> {
        Task {
            while !Task.isCancelled {
                let deadline = collector.lastActivity + limit
                if ContinuousClock.now >= deadline {
                    killer.stop(.idle)
                    return
                }
                // A cancelled sleep is the process exiting first; the loop condition stops it.
                try? await Task.sleep(until: deadline, clock: .continuous)
            }
        }
    }
}

/// Kills one process group, once: `SIGTERM`, then `SIGKILL` for whatever is left after the
/// grace. The first `stop` records why; later ones are no-ops. Thread-safe through `lock`.
final class GroupKiller: @unchecked Sendable {
    private let pid: Int32
    private let exited: Signal
    private let lock = NSLock()
    private var stopReason: Shell.Stop?
    private var task: Task<Void, Never>?

    /// `exited` fires once the leader is reaped, after which its pid may belong to anyone.
    init(_ pid: Int32, exited: Signal) {
        self.pid = pid
        self.exited = exited
    }

    var reason: Shell.Stop? {
        lock.lock()
        defer { lock.unlock() }
        return stopReason
    }

    func stop(_ reason: Shell.Stop) {
        lock.lock()
        defer { lock.unlock() }
        guard stopReason == nil else { return }
        stopReason = reason
        task = Task.detached { [self] in
            signal(SIGTERM)
            let clock = ContinuousClock()
            let deadline = clock.now + Shell.killGrace
            while clock.now < deadline, isAlive {
                // Cancellation is never requested on this task; the sleep cannot fail early.
                try? await Task.sleep(for: .milliseconds(50))
            }
            signal(SIGKILL)
        }
    }

    /// Waits for a kill under way to finish, so nothing it signalled is still alive when the
    /// run returns. Returns at once when nothing was stopped.
    func finish() async {
        let running = lock.withLock { task }
        await running?.value
    }

    /// Whether anything the kill targets is left: the group, or a leader not yet reaped.
    private var isAlive: Bool {
        Darwin.kill(-pid, 0) == 0 || !exited.fired
    }

    /// Signals the group. The group outlives its leader while a grandchild is in it, and a
    /// group id is not reused while the group exists, so a signal sent after the leader
    /// exited reaches only what it left behind. A leader that moved itself into another
    /// group leaves its own empty, so `ESRCH` falls back to the pid while it is unreaped.
    private func signal(_ signal: Int32) {
        guard Darwin.kill(-pid, signal) != 0, errno == ESRCH, !exited.fired else { return }
        // ESRCH here means the leader died since the check, which is what the kill is for.
        _ = Darwin.kill(pid, signal)
    }
}

/// Splits streamed bytes into lines. Buffers on `Data`, not `String`, so a multi-byte
/// character split across two reads survives. Reads arrive on a background thread;
/// `lock` guards the buffer, the lines and the clock.
final class LineCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer = Data()
    private var lines: [String] = []
    private var lastRead = ContinuousClock.now
    private let onOutput: (@Sendable ([String]) -> Void)?

    init(onOutput: (@Sendable ([String]) -> Void)?) {
        self.onOutput = onOutput
    }

    func take(_ data: Data) {
        guard !data.isEmpty else { return }
        var complete: [String] = []
        lock.lock()
        lastRead = .now
        buffer.append(data)
        while let newline = buffer.firstIndex(of: 0x0A) {
            complete.append(String(decoding: buffer[buffer.startIndex..<newline], as: UTF8.self))
            buffer.removeSubrange(buffer.startIndex...newline)
        }
        lines.append(contentsOf: complete)
        lock.unlock()
        if !complete.isEmpty { onOutput?(complete) }
    }

    /// When the last bytes arrived, or when the collector was made if none have.
    var lastActivity: ContinuousClock.Instant {
        lock.lock()
        defer { lock.unlock() }
        return lastRead
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
        if let trailing { onOutput?([trailing]) }
        return collected
    }
}

/// A one-shot signal, awaited once. Both users install it before the thing that fires it
/// starts, so an event that happens immediately cannot be missed. Thread-safe through `lock`.
final class Signal: @unchecked Sendable {
    private let lock = NSLock()
    private var signaled = false
    private var continuation: CheckedContinuation<Void, Never>?

    /// Whether `signal` has been called.
    var fired: Bool {
        lock.withLock { signaled }
    }

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
            // A cancelled sleep means the wait already ended; the guard below handles it.
            try? await Task.sleep(for: limit)
            guard !Task.isCancelled else { return }
            self.signal()
        }
        await wait()
        timer.cancel()
    }
}
