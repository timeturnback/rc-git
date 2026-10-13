import Foundation

/// A bounded process runner: merged stdout and stderr streamed one line at a time, a
/// timeout that kills, and a bounded wait for the last bytes. Nothing here waits forever.
public enum Shell {
    public struct Outcome: Sendable {
        public let status: Int32
        public let lines: [String]
        /// Whether the command ran past its limit and was killed.
        public let timedOut: Bool
    }

    /// How long the last bytes get to arrive after the process exits. A grandchild that
    /// inherited the pipe keeps it open, so this wait is bounded like every other.
    static let drainGrace = Duration.seconds(2)
    /// How long a killed process gets to die before `SIGKILL`.
    private static let killGrace = Duration.seconds(2)

    /// Runs `executable` with `arguments` in `directory`. With a `timeout`, a command still
    /// running at the limit is killed and the outcome says so.
    public static func run(
        _ executable: String, _ arguments: [String], in directory: String,
        timeout: Duration? = nil, onOutput: (@Sendable (String) -> Void)? = nil
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
            collector.take(Data("cannot run \(executable): \(error)\n".utf8))
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
            // A cancelled sleep is the process exiting first; the guard below handles it.
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

/// A one-shot flag, set from whichever thread gets there first. Thread-safe through `lock`.
final class Flag: @unchecked Sendable {
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
/// character split across two reads survives. Reads arrive on a background thread;
/// `lock` guards the buffer and the lines.
final class LineCollector: @unchecked Sendable {
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
/// starts, so an event that happens immediately cannot be missed. Thread-safe through `lock`.
final class Signal: @unchecked Sendable {
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
            // A cancelled sleep means the wait already ended; the guard below handles it.
            try? await Task.sleep(for: limit)
            guard !Task.isCancelled else { return }
            self.signal()
        }
        await wait()
        timer.cancel()
    }
}
