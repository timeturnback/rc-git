import Foundation
import Testing

@testable import RcGitKit

/// The guarded write path, driven through the C ABI against a real fixture.
@Suite struct RepositoryGuardTests {
    @Test func createsPlansAndDeletesWithBackups() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }

        try RepositoryGuard.createBranch(
            repoPath: fixture.path, name: "topic", revspec: "main~1")
        #expect(fixture.revParse("refs/heads/topic") == fixture.revParse("main~1"))
        #expect(throws: RepositoryError.self) {
            try RepositoryGuard.createBranch(repoPath: fixture.path, name: "topic", revspec: "main")
        }

        let plan = try RepositoryGuard.planBranchDelete(repoPath: fixture.path, name: "feature")
        #expect(plan.kind == .confirm)
        #expect(plan.risk == .destructive)
        #expect(plan.typedPhrase == "feature")
        #expect(plan.backupRef)
        #expect(!plan.merged)

        #expect(throws: RepositoryError.self) {
            try RepositoryGuard.deleteBranch(
                repoPath: fixture.path, name: "feature", input: "wrong")
        }
        let backup = try RepositoryGuard.deleteBranch(
            repoPath: fixture.path, name: "feature", input: "feature")
        #expect(backup == "refs/rc-git/backup/branch/feature")

        let denied = try RepositoryGuard.planBranchDelete(repoPath: fixture.path, name: "main")
        #expect(denied.kind == .deny)
        #expect(denied.reason?.contains("protected") == true)

        let tagPlan = RepositoryGuard.planTagDelete(name: "v1.0")
        #expect(tagPlan?.typedPhrase == "v1.0")
        let tagBackup = try RepositoryGuard.deleteTag(
            repoPath: fixture.path, name: "v1.0", input: "v1.0")
        #expect(tagBackup == "refs/rc-git/backup/tag/v1.0")
    }

    @Test func plansCarryTheChangesTabPolicy() {
        #expect(RepositoryGuard.planCommit(files: 2)?.kind == .allow)
        #expect(RepositoryGuard.planStashApply()?.kind == .allow)
        let push = RepositoryGuard.planStashPush(files: 3)
        #expect(push?.kind == .confirm)
        #expect(push?.typedPhrase == nil)
        let pop = RepositoryGuard.planStashPop()
        #expect(pop?.kind == .confirm)
        #expect(pop?.backupRef == true)
        let drop = RepositoryGuard.planStashDrop(index: 1)
        #expect(drop?.typedPhrase == "drop")
        #expect(drop?.summary.contains("stash@{1}") == true)
        let discard = RepositoryGuard.planDiscard(files: 4)
        #expect(discard?.kind == .confirm)
        #expect(discard?.typedPhrase == nil)
        #expect(discard?.risk == .destructive)
    }

    @Test func commitsStashesAndDiscardsThroughTheBridge() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let author = CommitIdentity(name: "Bridge User", email: "bridge@example.com")
        try fixture.write("alpha.txt", "bridge edit\n")
        let head = fixture.revParse("HEAD")

        let outcome = try RepositoryGuard.commit(
            repoPath: fixture.path, message: "Bridge commit", author: author,
            expectedHead: head, selections: [.whole(path: "alpha.txt")], input: nil)
        #expect(outcome.branch == "main")
        #expect(outcome.oid == fixture.revParse("HEAD"))
        #expect(
            try fixture.gitOutput(["log", "-1", "--format=%an|%s"]) == "Bridge User|Bridge commit")
        #expect(try fixture.gitOutput(["status", "--porcelain"]).isEmpty)

        try fixture.write("alpha.txt", "stash via bridge\n")
        try RepositoryGuard.stashPush(
            repoPath: fixture.path, paths: ["alpha.txt"], message: "bridged",
            author: author, input: nil)
        #expect(try fixture.gitOutput(["stash", "list", "--format=%gs"]) == "On main: bridged")
        let backup = try RepositoryGuard.stashPop(repoPath: fixture.path, index: 0, input: nil)
        #expect(backup == "refs/rc-git/backup/stash/0")

        try RepositoryGuard.discard(repoPath: fixture.path, paths: ["alpha.txt"], input: nil)
        #expect(try fixture.gitOutput(["status", "--porcelain"]).isEmpty)
    }
}

/// The remote preflight, driven through the C ABI against a real fixture.
@Suite struct RemotePreflightTests {
    @Test func preflightMarshalsUpstreamConfigurationAndCounts() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }

        var status = try RepositoryGuard.remotePreflight(repoPath: fixture.path)
        #expect(!status.hasUpstream)
        #expect(status.branch == "main")

        try fixture.git(["remote", "add", "origin", "https://example.com:8443/repo.git"])
        try fixture.git(["config", "branch.main.remote", "origin"])
        try fixture.git(["config", "branch.main.merge", "refs/heads/main"])
        status = try RepositoryGuard.remotePreflight(repoPath: fixture.path)
        #expect(status.hasUpstream)
        #expect(status.remote == "origin")
        #expect(status.host == "example.com:8443")
        #expect(status.url == "https://example.com:8443/repo.git")
        // The fixture's refs/remotes/origin/main sits at HEAD, so nothing is ahead or behind.
        #expect(status.ahead == 0)
        #expect(status.behind == 0)
    }

    @Test func aSignalledTokenStopsAFetchBeforeItReachesTheNetwork() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.git(["remote", "add", "origin", "https://example.com/repo.git"])
        try fixture.git(["config", "branch.main.remote", "origin"])
        try fixture.git(["config", "branch.main.merge", "refs/heads/main"])

        let cancel = CancelToken()
        #expect(!cancel.isSignalled)
        cancel.signal()
        #expect(cancel.isSignalled)

        // example.com is never dialled: the guard checks the flag before connecting,
        // which is also what proves the pointer crossed the ABI intact.
        do {
            _ = try RepositoryGuard.fetch(
                repoPath: fixture.path, credentials: nil, cancel: cancel)
            Issue.record("expected the fetch to be cancelled")
        } catch {
            guard case .cancelled = error else {
                Issue.record("expected .cancelled, got \(error)")
                return
            }
        }
    }
}
