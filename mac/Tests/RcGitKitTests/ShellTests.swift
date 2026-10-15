import Foundation
import Testing

@testable import RcGitKit

/// The runner behind the updater's toolchain probe and build. `/bin/sh -c` stands in for
/// the app's login `zsh`, so no shell profile is read.
///
/// A spawn takes seconds on a loaded machine with endpoint agents, so every limit a child
/// must outrun before it prints leaves seconds for its start, elapsed times run from the
/// child's first line rather than from the spawn, and scripts sleep in one process instead of
/// spawning `sleep` per step.
@Suite(.timeLimit(.minutes(1))) struct ShellTests {
    private static let directory = FileManager.default.temporaryDirectory.path

    private static func run(
        _ script: String, timeout: Duration? = nil, idleTimeout: Duration? = nil
    ) async -> (Shell.Outcome, Duration) {
        await launch("/bin/sh", ["-c", script], timeout: timeout, idleTimeout: idleTimeout)
    }

    /// Runs a command and times it from its first line of output, which is when it reports it
    /// started; a command that never prints is timed from the spawn.
    private static func launch(
        _ executable: String, _ arguments: [String], timeout: Duration? = nil,
        idleTimeout: Duration? = nil
    ) async -> (Shell.Outcome, Duration) {
        let spawned = ContinuousClock.now
        let started = OneShot<ContinuousClock.Instant>()
        let outcome = await Shell.run(
            executable, arguments, in: directory, timeout: timeout, idleTimeout: idleTimeout,
            onOutput: { _ in started.fulfil(.now) })
        let finished = ContinuousClock.now
        started.fulfil(spawned)
        return (outcome, finished - (await started.value()))
    }

    /// Whether `pid` is gone, polled for up to ten seconds: an orphan killed by a signal is
    /// reaped by launchd, not at once, and later still on a loaded machine.
    private static func isGone(_ pid: Int32) async -> Bool {
        for _ in 0..<200 {
            if Darwin.kill(pid, 0) != 0 { return true }
            try? await Task.sleep(for: .milliseconds(50))
        }
        return false
    }

    /// The bounds on elapsed time sit halfway to the 60 s sleep, far from the kill's few
    /// seconds, so a loaded machine cannot flip them.
    @Test func aCommandPastItsLimitIsKilled() async {
        let (outcome, elapsed) = await Self.run(
            "echo started; exec sleep 60", timeout: .seconds(5))
        #expect(outcome.stopped == .timedOut)
        #expect(elapsed < .seconds(30))
    }

    /// A build blocked on a lock prints a line and then nothing; the idle clock stops it
    /// long before the wall clock would.
    @Test func aSilentCommandIsStoppedByTheIdleClock() async {
        let (outcome, elapsed) = await Self.run(
            "echo waiting; exec sleep 60", timeout: .seconds(60), idleTimeout: .seconds(5))
        #expect(outcome.stopped == .idle)
        #expect(outcome.lines == ["waiting"])
        #expect(elapsed < .seconds(30))
    }

    /// Output re-arms the idle clock: 8 s of steady output outlives a 5 s idle limit. One
    /// perl process sleeps between lines, where a `sleep` spawn per line could outlast it.
    @Test func steadyOutputKeepsTheIdleClockArmed() async {
        let script = "$| = 1; for (1..10) { print qq($_\\n); select(undef, undef, undef, 0.8) }"
        let (outcome, _) = await Self.launch(
            "/usr/bin/perl", ["-e", script], idleTimeout: .seconds(5))
        #expect(outcome.stopped == nil)
        #expect(outcome.status == 0)
        #expect(outcome.lines.count == 10)
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

    /// The shell dies on `SIGTERM` and its grandchild ignores it; the leader's exit does not
    /// call off the `SIGKILL` the group gets after the grace.
    @Test func aTimeoutKillsAGrandchildThatIgnoresSigterm() async throws {
        let (outcome, elapsed) = await Self.run(
            "(trap '' TERM; exec sleep 60) & echo $!; wait", timeout: .seconds(5))
        #expect(outcome.stopped == .timedOut)
        let grandchild = try #require(outcome.lines.first.flatMap { Int32($0) })
        #expect(await Self.isGone(grandchild))
        #expect(elapsed < .seconds(30))
    }

    /// A leader that moved itself into another group leaves its own group empty; the kill
    /// falls back to the pid instead of finding no group and letting it run.
    @Test func aLeaderThatLeftItsGroupIsStillKilled() async {
        let script =
            "$| = 1; setpgid(0, getpgrp(getppid())) or die $!; "
            + "print qq(moved\\n); sleep 60"
        let (outcome, elapsed) = await Self.launch(
            "/usr/bin/perl", ["-MPOSIX", "-e", script], timeout: .seconds(5))
        #expect(outcome.lines == ["moved"])
        #expect(outcome.stopped == .timedOut)
        #expect(elapsed < .seconds(30))
    }

    /// A Task cancelled before the command starts still stops it at once. Nothing prints, so
    /// the bound counts the spawn too and sits halfway to the 30 s sleep.
    @Test func aCommandStartedFromACancelledTaskIsStopped() async {
        let clock = ContinuousClock()
        let start = clock.now
        let run = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return await Shell.run("/bin/sh", ["-c", "sleep 30"], in: Self.directory)
        }
        let outcome = await run.value
        #expect(outcome.stopped == .cancelled)
        #expect(clock.now - start < .seconds(15))
    }

    /// A grandchild that inherited the pipe keeps it open after the shell exits; the wait
    /// for the last bytes gives up after the drain grace instead of the grandchild's 45 s.
    /// The bound sits far from both ends so a loaded machine cannot flip it.
    @Test func aGrandchildHoldingThePipeDoesNotHoldTheRun() async throws {
        let (outcome, elapsed) = await Self.run("(sleep 45 & echo $!); echo hi")
        let grandchild = try #require(outcome.lines.first.flatMap { Int32($0) })
        defer { Darwin.kill(grandchild, SIGKILL) }
        #expect(outcome.lines.dropFirst() == ["hi"])
        #expect(outcome.stopped == nil)
        #expect(elapsed < .seconds(20))
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
        #expect(!outcome.succeeded)
        #expect(outcome.failure == "failed (status 3)")
    }

    /// A signal number is not an exit status: a build the system killed says so instead of
    /// reading like a script that exited with 9.
    @Test func aCommandKilledByASignalSaysSo() async {
        let (outcome, _) = await Self.run("echo dying; kill -9 $$")
        #expect(outcome.reason == .uncaughtSignal)
        #expect(outcome.status == SIGKILL)
        #expect(outcome.lines == ["dying"])
        #expect(outcome.stopped == nil)
        #expect(!outcome.succeeded)
        #expect(outcome.failure == "was killed by signal 9 (SIGKILL)")
    }

    /// A checkout that moved fails the start the same way a missing executable does; the
    /// outcome blames the directory, not the shell.
    @Test func aMissingWorkingDirectoryIsNamed() async throws {
        let missing = Self.directory + "/rc-git-moved-\(UUID().uuidString)"
        let outcome = await Shell.run("/bin/sh", ["-c", "true"], in: missing)
        #expect(outcome.status == -1)
        #expect(!outcome.succeeded)
        let line = try #require(outcome.lines.first)
        #expect(line.hasPrefix("working directory \(missing) does not exist: "))
    }

    @Test func aMissingExecutableIsNamed() async throws {
        let outcome = await Shell.run("/nonexistent/rc-git-shell", [], in: Self.directory)
        #expect(outcome.status == -1)
        let line = try #require(outcome.lines.first)
        #expect(line.hasPrefix("cannot run /nonexistent/rc-git-shell: "))
    }

    @Test func anUnlistedSignalIsNamedByItsNumber() {
        #expect(Shell.signalName(SIGTERM) == "SIGTERM")
        #expect(Shell.signalName(31) == "SIG31")
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
