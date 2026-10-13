import Foundation
import Testing

@testable import RcGitKit

/// The runner behind the updater's toolchain probe and build. `/bin/sh -c` stands in for
/// the app's login `zsh`, so no shell profile is read.
@Suite(.timeLimit(.minutes(1))) struct ShellTests {
    private static let directory = FileManager.default.temporaryDirectory.path

    private static func run(
        _ script: String, timeout: Duration? = nil, idleTimeout: Duration? = nil
    ) async -> (Shell.Outcome, Duration) {
        let clock = ContinuousClock()
        let start = clock.now
        let outcome = await Shell.run(
            "/bin/sh", ["-c", script], in: directory, timeout: timeout, idleTimeout: idleTimeout)
        return (outcome, clock.now - start)
    }

    /// Whether `pid` is gone, polled for up to two seconds: an orphan killed by a signal is
    /// reaped by launchd, not at once.
    private static func isGone(_ pid: Int32) async -> Bool {
        for _ in 0..<40 {
            if Darwin.kill(pid, 0) != 0 { return true }
            try? await Task.sleep(for: .milliseconds(50))
        }
        return false
    }

    @Test func aCommandPastItsLimitIsKilled() async {
        let (outcome, elapsed) = await Self.run("sleep 30", timeout: .milliseconds(500))
        #expect(outcome.stopped == .timedOut)
        #expect(elapsed < .seconds(5))
    }

    /// A build blocked on a lock prints a line and then nothing; the idle clock stops it
    /// long before the wall clock would.
    @Test func aSilentCommandIsStoppedByTheIdleClock() async {
        let (outcome, elapsed) = await Self.run(
            "echo waiting; sleep 30", timeout: .seconds(30), idleTimeout: .seconds(1))
        #expect(outcome.stopped == .idle)
        #expect(outcome.lines == ["waiting"])
        #expect(elapsed < .seconds(6))
    }

    /// Output re-arms the idle clock: 1.6 s of steady output outlives a 1 s idle limit.
    @Test func steadyOutputKeepsTheIdleClockArmed() async {
        let (outcome, _) = await Self.run(
            "for i in 1 2 3 4 5 6 7 8; do echo $i; sleep 0.2; done", idleTimeout: .seconds(1))
        #expect(outcome.stopped == nil)
        #expect(outcome.status == 0)
        #expect(outcome.lines.count == 8)
    }

    /// Cancelling the Task kills the whole process group: the grandchild the script
    /// started dies with it, rather than living on as an orphan holding a lock.
    @Test func cancellingTheTaskKillsTheProcessGroup() async throws {
        let started = Signal()
        let run = Task {
            await Shell.run(
                "/bin/sh", ["-c", "sleep 30 & echo $!; wait"], in: Self.directory,
                onOutput: { _ in started.signal() })
        }
        await started.wait()
        run.cancel()
        let outcome = await run.value
        #expect(outcome.stopped == .cancelled)
        let grandchild = try #require(outcome.lines.first.flatMap { Int32($0) })
        #expect(await Self.isGone(grandchild))
    }

    /// A Task cancelled before the command starts still stops it at once.
    @Test func aCommandStartedFromACancelledTaskIsStopped() async {
        let clock = ContinuousClock()
        let start = clock.now
        let run = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return await Shell.run("/bin/sh", ["-c", "sleep 30"], in: Self.directory)
        }
        let outcome = await run.value
        #expect(outcome.stopped == .cancelled)
        #expect(clock.now - start < .seconds(5))
    }

    /// A grandchild that inherited the pipe keeps it open after the shell exits; the wait
    /// for the last bytes gives up after the drain grace instead of the grandchild's 30 s.
    @Test func aGrandchildHoldingThePipeDoesNotHoldTheRun() async {
        let (outcome, elapsed) = await Self.run("(sleep 30 &); echo hi")
        #expect(outcome.lines == ["hi"])
        #expect(outcome.stopped == nil)
        #expect(elapsed < Shell.drainGrace + .seconds(1))
    }

    @Test func aLastLineWithoutANewlineIsKept() async {
        let (outcome, _) = await Self.run("printf tail")
        #expect(outcome.lines == ["tail"])
        #expect(outcome.status == 0)
    }

    @Test func aFailingCommandReportsItsStatus() async {
        let (outcome, _) = await Self.run("echo nope; exit 3")
        #expect(outcome.status == 3)
        #expect(outcome.lines == ["nope"])
        #expect(outcome.stopped == nil)
    }

    @Test func aCharacterSplitAcrossTwoReadsSurvives() throws {
        let collector = LineCollector(onOutput: nil)
        let bytes = Array("é".utf8)
        try #require(bytes.count == 2)
        collector.take(Data([bytes[0]]))
        collector.take(Data([bytes[1]]))
        collector.take(Data("\n".utf8))
        #expect(collector.finish() == ["é"])
    }
}
