import Foundation
import Testing

@testable import RcGitKit

/// Drives `RefWatcher` and `WorktreeWatcher` against real FSEvents on a `GitFixture`, which
/// lives under `/var/folders` and is reported by FSEvents as `/private/var/folders`.
@Suite struct RefWatcherTests {
    private static let bound: DispatchTimeInterval = .seconds(5)

    @Test func aRefUpdateUnderTheTemporaryDirectoryFiresOnChange() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repository = try Repository(path: fixture.path)
        let fired = DispatchSemaphore(value: 0)
        let watcher = RefWatcher(gitDir: repository.gitDirectory, latency: 0.05) {
            fired.signal()
        }
        try watcher.start()
        defer { watcher.stop() }

        try fixture.git(["update-ref", "refs/heads/x", "HEAD"])

        #expect(fired.wait(timeout: .now() + Self.bound) == .success)
    }

    @Test func aWorktreeEditUnderTheTemporaryDirectoryFiresOnChange() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repository = try Repository(path: fixture.path)
        let workDir = try #require(repository.workingDirectory)
        let fired = DispatchSemaphore(value: 0)
        let watcher = WorktreeWatcher(
            workDir: workDir, gitDir: repository.gitDirectory, latency: 0.05
        ) {
            fired.signal()
        }
        try watcher.start()
        defer { watcher.stop() }

        try fixture.write("edited.txt", "edited\n")

        #expect(fired.wait(timeout: .now() + Self.bound) == .success)
    }

    @Test func aRepositoryReachedThroughASymlinkFiresOnChange() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let link = FileManager.default.temporaryDirectory
            .appendingPathComponent("rc-git-link-\(UUID().uuidString)").path
        try FileManager.default.createSymbolicLink(
            atPath: link, withDestinationPath: fixture.path)
        defer { try? FileManager.default.removeItem(atPath: link) }
        let fired = DispatchSemaphore(value: 0)
        let watcher = RefWatcher(gitDir: link + "/.git", latency: 0.05) { fired.signal() }
        try watcher.start()
        defer { watcher.stop() }

        try fixture.git(["update-ref", "refs/heads/x", "HEAD"])

        #expect(fired.wait(timeout: .now() + Self.bound) == .success)
    }

    @Test func canonicalResolvesSymlinksAndKeepsPrivate() throws {
        let fixture = try GitFixture.emptyDirectory()
        defer { fixture.remove() }
        let link = FileManager.default.temporaryDirectory
            .appendingPathComponent("rc-git-link-\(UUID().uuidString)").path
        try FileManager.default.createSymbolicLink(
            atPath: link, withDestinationPath: fixture.path)
        defer { try? FileManager.default.removeItem(atPath: link) }

        let resolved = RefWatcher.canonical(link)
        #expect(resolved.hasPrefix("/private/"))
        #expect(resolved == WorktreeWatcher.canonical(fixture.path))
        #expect(RefWatcher.canonical("/no/such/dir") == "/no/such/dir")
    }
}
