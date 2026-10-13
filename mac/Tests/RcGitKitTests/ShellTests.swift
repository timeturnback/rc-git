import Foundation
import Testing

@testable import RcGitKit

/// The runner behind the updater's toolchain probe and build. `/bin/sh -c` stands in for
/// the app's login `zsh`, so no shell profile is read.
@Suite(.timeLimit(.minutes(1))) struct ShellTests {
    private static let directory = FileManager.default.temporaryDirectory.path

    private static func run(
        _ script: String, timeout: Duration? = nil
    ) async -> (Shell.Outcome, Duration) {
        let clock = ContinuousClock()
        let start = clock.now
        let outcome = await Shell.run("/bin/sh", ["-c", script], in: directory, timeout: timeout)
        return (outcome, clock.now - start)
    }

    @Test func aCommandPastItsLimitIsKilled() async {
        let (outcome, elapsed) = await Self.run("sleep 30", timeout: .milliseconds(500))
        #expect(outcome.timedOut)
        #expect(elapsed < .seconds(5))
    }

    /// A grandchild that inherited the pipe keeps it open after the shell exits; the wait
    /// for the last bytes gives up after the drain grace instead of the grandchild's 30 s.
    @Test func aGrandchildHoldingThePipeDoesNotHoldTheRun() async {
        let (outcome, elapsed) = await Self.run("(sleep 30 &); echo hi")
        #expect(outcome.lines == ["hi"])
        #expect(!outcome.timedOut)
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
        #expect(!outcome.timedOut)
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
