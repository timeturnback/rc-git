import Foundation
import Testing

@testable import RcGitKit

/// Drives `RefWatcher` and `WorktreeWatcher` against real FSEvents on a `GitFixture`, which
/// lives under `/var/folders` and is reported by FSEvents as `/private/var/folders`.
@Suite(.timeLimit(.minutes(1))) struct RefWatcherTests {
    private static let bound: DispatchTimeInterval = .seconds(5)

    @Test func aRefUpdateUnderTheTemporaryDirectoryFiresOnChange() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let repository = try Repository(path: fixture.path)
        let fired = OneShot<Bool>()
        let watcher = RefWatcher(
            gitDir: repository.gitDirectory, commonDir: repository.commonDirectory, latency: 0.05
        ) {
            fired.fulfil(true)
        }
        try watcher.start()
        defer { watcher.stop() }

        try await fixture.git(["update-ref", "refs/heads/x", "HEAD"])

        #expect(await fired.value(within: Self.bound, else: false))
    }

    @Test func aCommitInALinkedWorktreeFiresOnChange() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let linkedPath = outside.path + "/linked"
        try await fixture.git(["worktree", "add", "-q", "-b", "wt", linkedPath])
        let linked = GitFixture(path: linkedPath)
        let repository = try Repository(path: linkedPath)
        #expect(repository.commonDirectory != repository.gitDirectory)
        let fired = OneShot<Bool>()
        let watcher = RefWatcher(
            gitDir: repository.gitDirectory, commonDir: repository.commonDirectory, latency: 0.05
        ) {
            fired.fulfil(true)
        }
        try watcher.start()
        defer { watcher.stop() }

        try await linked.git(["commit", "-q", "--allow-empty", "-m", "x"])

        #expect(await fired.value(within: Self.bound, else: false))
    }

    @Test func aWorktreeEditUnderTheTemporaryDirectoryFiresOnChange() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let repository = try Repository(path: fixture.path)
        let workDir = try #require(repository.workingDirectory)
        let fired = OneShot<Bool>()
        let watcher = WorktreeWatcher(
            workDir: workDir, gitDir: repository.gitDirectory,
            commonDir: repository.commonDirectory, latency: 0.05
        ) {
            fired.fulfil(true)
        }
        try watcher.start()
        defer { watcher.stop() }

        try fixture.write("edited.txt", "edited\n")

        #expect(await fired.value(within: Self.bound, else: false))
    }

    @Test func aCommonDirExcludeEditFiresOnChangeForALinkedWorktree() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let linkedPath = outside.path + "/linked"
        try await fixture.git(["worktree", "add", "-q", "-b", "wt", linkedPath])
        let repository = try Repository(path: linkedPath)
        let workDir = try #require(repository.workingDirectory)
        let fired = OneShot<Bool>()
        let watcher = WorktreeWatcher(
            workDir: workDir, gitDir: repository.gitDirectory,
            commonDir: repository.commonDirectory, latency: 0.05
        ) {
            fired.fulfil(true)
        }
        #expect(watcher.watchedPaths.count == 2)
        try watcher.start()
        defer { watcher.stop() }

        try FileManager.default.createDirectory(
            atPath: repository.commonDirectory + "/info", withIntermediateDirectories: true)
        try Data("*.log\n".utf8).write(
            to: URL(fileURLWithPath: repository.commonDirectory + "/info/exclude"))

        #expect(await fired.value(within: Self.bound, else: false))
    }

    @Test func aRepositoryReachedThroughASymlinkFiresOnChange() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let link = FileManager.default.temporaryDirectory
            .appendingPathComponent("rc-git-link-\(UUID().uuidString)").path
        try FileManager.default.createSymbolicLink(
            atPath: link, withDestinationPath: fixture.path)
        defer { try? FileManager.default.removeItem(atPath: link) }
        let fired = OneShot<Bool>()
        let watcher = RefWatcher(gitDir: link + "/.git", commonDir: link + "/.git", latency: 0.05) {
            fired.fulfil(true)
        }
        try watcher.start()
        defer { watcher.stop() }

        try await fixture.git(["update-ref", "refs/heads/x", "HEAD"])

        #expect(await fired.value(within: Self.bound, else: false))
    }
}
