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
        #expect(try fixture.revParse("refs/heads/topic") == fixture.revParse("main~1"))
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
        let entry = StashEntry(index: 1, oid: .unhashed, message: "On main: wip")
        let pop = RepositoryGuard.planStashPop(entry)
        #expect(pop?.kind == .confirm)
        #expect(pop?.backupRef == true)
        #expect(pop?.summary.contains("stash@{1} \"On main: wip\"") == true)
        let drop = RepositoryGuard.planStashDrop(entry)
        #expect(drop?.typedPhrase == "drop")
        #expect(drop?.summary.contains("stash@{1} \"On main: wip\"") == true)
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
        let head = try fixture.revParse("HEAD")

        let outcome = try RepositoryGuard.commit(
            repoPath: fixture.path, message: "Bridge commit", fallback: author,
            expectedHead: try commitID(head), selections: [.whole(path: "alpha.txt")], input: nil)
        #expect(outcome.branch == "main")
        #expect(try outcome.oid.hex == fixture.revParse("HEAD"))
        #expect(
            try fixture.gitOutput(["log", "-1", "--format=%an|%ae|%s"])
                == "Bridge User|bridge@example.com|Bridge commit")
        #expect(try fixture.gitOutput(["status", "--porcelain"]).isEmpty)

        try fixture.write("alpha.txt", "stash via bridge\n")
        try RepositoryGuard.stashPush(
            repoPath: fixture.path, paths: ["alpha.txt"], message: "bridged",
            fallback: author, input: nil)
        #expect(try fixture.gitOutput(["stash", "list", "--format=%gs"]) == "On main: bridged")
        #expect(
            try fixture.gitOutput(["log", "-1", "--format=%an|%ae", "refs/stash"])
                == "Bridge User|bridge@example.com")
        let shown = try #require(try Repository(path: fixture.path).stashList().first)
        let elsewhere = StashEntry(index: 0, oid: try commitID(head), message: shown.message)
        #expect(throws: RepositoryError.self) {
            try RepositoryGuard.stashDrop(repoPath: fixture.path, entry: elsewhere, input: "drop")
        }
        #expect(try fixture.gitOutput(["stash", "list", "--format=%gs"]) == "On main: bridged")
        let backup = try RepositoryGuard.stashPop(repoPath: fixture.path, entry: shown, input: nil)
        #expect(backup == "refs/rc-git/backup/stash/0")

        try RepositoryGuard.discard(repoPath: fixture.path, paths: ["alpha.txt"], input: nil)
        #expect(try fixture.gitOutput(["status", "--porcelain"]).isEmpty)
    }

    @Test func hunkSelectionsCommitAndDiscardThroughTheBridge() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let author = CommitIdentity(name: "Bridge User", email: "bridge@example.com")
        let lines = (1...20).map { "l\($0)\n" }
        try fixture.write("hunky.txt", lines.joined())
        try fixture.commit("Add hunky", date: "2024-01-05T00:00:00+0000")
        var edited = lines
        edited[17] = "L18a\nL18b\n"
        edited[0] = "l1\nTOP1\nTOP2\n"
        try fixture.write("hunky.txt", edited.joined())

        let stamps = try worktreeStamps(fixture, "hunky.txt")
        try #require(stamps.count == 2)
        #expect(stamps[1] == HunkStamp(oldStart: 15, oldLines: 6, newStart: 17, newLines: 7))
        _ = try RepositoryGuard.commit(
            repoPath: fixture.path, message: "Second hunk", fallback: author,
            expectedHead: try commitID(fixture.revParse("HEAD")),
            selections: [.hunks(path: "hunky.txt", stamps: [stamps[1]])], input: nil)
        let committed = try fixture.gitOutput(["show", "HEAD:hunky.txt"])
        #expect(committed.contains("L18a\nL18b"))
        #expect(!committed.contains("TOP1"))

        let remaining = try worktreeStamps(fixture, "hunky.txt")
        try #require(remaining.count == 1)
        try RepositoryGuard.discardHunks(
            repoPath: fixture.path, path: "hunky.txt", stamps: remaining, input: nil)
        #expect(try fixture.gitOutput(["status", "--porcelain"]).isEmpty)
    }

    @Test func anEmptyHunkSelectionThrowsAndCommitsNothing() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write("alpha.txt", "never committed\n")
        let head = try fixture.revParse("HEAD")

        do {
            _ = try RepositoryGuard.commit(
                repoPath: fixture.path, message: "Nothing",
                fallback: CommitIdentity(name: "Bridge User", email: "bridge@example.com"),
                expectedHead: try commitID(head),
                selections: [.hunks(path: "alpha.txt", stamps: [])], input: nil)
            Issue.record("expected an empty hunk selection to be refused")
        } catch let error as RepositoryError {
            #expect(error.message.contains("no hunks selected for 'alpha.txt'"))
        }
        #expect(try fixture.revParse("HEAD") == head)
        #expect(try fixture.gitOutput(["status", "--porcelain"]) == "M alpha.txt")
    }

    private func worktreeStamps(_ fixture: GitFixture, _ path: String) throws -> [HunkStamp] {
        let repository = try Repository(path: fixture.path)
        let old = try repository.resolve("HEAD:\(path)")
        guard case let .text(hunks) = try repository.diffWorktreeFile(path: path, old: old) else {
            throw RepositoryError("expected a text diff of '\(path)'")
        }
        return hunks.map(\.stamp)
    }

    @Test func aSavedFallbackNeverOverridesTheRepositoryEmail() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        #expect(try RepositoryGuard.configuredIdentity(repoPath: fixture.path) == nil)
        try fixture.git(["config", "user.name", "Work Me"])
        try fixture.git(["config", "user.email", "work@example.com"])
        #expect(
            try RepositoryGuard.configuredIdentity(repoPath: fixture.path)
                == CommitIdentity(name: "Work Me", email: "work@example.com"))

        let saved = CommitIdentity(name: "Saved Me", email: "saved@example.com")
        try fixture.write("alpha.txt", "work edit\n")
        _ = try RepositoryGuard.commit(
            repoPath: fixture.path, message: "Work commit", fallback: saved,
            expectedHead: try commitID(fixture.revParse("HEAD")),
            selections: [.whole(path: "alpha.txt")],
            input: nil)
        #expect(
            try fixture.gitOutput(["log", "-1", "--format=%ae|%ce"])
                == "work@example.com|work@example.com")

        try fixture.write("alpha.txt", "stash edit\n")
        try RepositoryGuard.stashPush(
            repoPath: fixture.path, paths: ["alpha.txt"], message: nil, fallback: saved,
            input: nil)
        #expect(
            try fixture.gitOutput(["log", "-1", "--format=%ae", "stash@{0}"]) == "work@example.com")
    }

    @Test func aMissingIdentityThrowsItsOwnTypedError() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write("alpha.txt", "nobody's edit\n")
        let head = try fixture.revParse("HEAD")

        let commitError = #expect(throws: IdentityRequiredError.self) {
            _ = try RepositoryGuard.commit(
                repoPath: fixture.path, message: "Nobody", fallback: nil,
                expectedHead: try commitID(head),
                selections: [.whole(path: "alpha.txt")], input: nil)
        }
        #expect(commitError?.message.contains("no author identity") == true)
        #expect(try fixture.revParse("HEAD") == head)

        #expect(throws: IdentityRequiredError.self) {
            try RepositoryGuard.stashPush(
                repoPath: fixture.path, paths: ["alpha.txt"], message: nil, fallback: nil,
                input: nil)
        }
        #expect(try fixture.gitOutput(["stash", "list"]).isEmpty)
    }

    @Test func configuredIdentityNamesAMissingRepository() throws {
        #expect(throws: RepositoryError.self) {
            try RepositoryGuard.configuredIdentity(repoPath: "/nonexistent/rc-git-repo")
        }
    }

    @Test func checkoutSwitchesBranchesAndRefusesADirtyPath() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }

        let outcome = try RepositoryGuard.checkout(repoPath: fixture.path, branch: "feature")
        #expect(outcome.branch == "feature")
        #expect(!outcome.alreadyOnBranch)
        #expect(outcome.filesChanged > 0)
        #expect(try outcome.newHead.hex == fixture.revParse("feature"))
        #expect(try fixture.gitOutput(["rev-parse", "--abbrev-ref", "HEAD"]) == "feature")
        #expect(try fixture.gitOutput(["status", "--porcelain"]).isEmpty)

        let again = try RepositoryGuard.checkout(repoPath: fixture.path, branch: "feature")
        #expect(again.alreadyOnBranch)
        #expect(again.filesChanged == 0)

        // feature.txt exists only on `feature`, so switching away must delete it.
        try fixture.write("feature.txt", "work in progress\n")
        #expect(throws: SyncError.self) {
            try RepositoryGuard.checkout(repoPath: fixture.path, branch: "main")
        }
        #expect(try fixture.gitOutput(["rev-parse", "--abbrev-ref", "HEAD"]) == "feature")
    }

    private func commitID(_ hex: String) throws -> Oid {
        try #require(Oid(hex: hex))
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
        #expect(!status.usesSsh)
        // The fixture's refs/remotes/origin/main sits at HEAD, so nothing is ahead or behind.
        #expect(status.ahead == 0)
        #expect(status.behind == 0)
    }

    /// Points `main` at an unreachable origin; the tests below never touch the network.
    private func configureUpstream(_ fixture: GitFixture) throws {
        try fixture.git(["remote", "add", "origin", "https://127.0.0.1:9/repo.git"])
        try fixture.git(["config", "branch.main.remote", "origin"])
        try fixture.git(["config", "branch.main.merge", "refs/heads/main"])
    }

    /// Moves `refs/remotes/origin/main` one commit (adding `news.txt`) past `main`.
    private func advanceTrackingRef(_ fixture: GitFixture) throws {
        try fixture.write("news.txt", "news\n")
        try fixture.commit("Upstream", date: "2024-03-01T00:00:00+0000")
        try fixture.git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        try fixture.git(["reset", "-q", "--hard", "HEAD~1"])
    }

    @Test func countsCrossTheBridgeUnswapped() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try configureUpstream(fixture)
        try fixture.git(["commit", "--allow-empty", "-q", "-m", "local"])

        let status = try RepositoryGuard.remotePreflight(repoPath: fixture.path)
        #expect(status.ahead == 1)
        #expect(status.behind == 0)
    }

    @Test func pullFastForwardAdvancesToTheTrackingRef() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try configureUpstream(fixture)
        try advanceTrackingRef(fixture)
        #expect(try RepositoryGuard.remotePreflight(repoPath: fixture.path).behind == 1)

        let outcome = try RepositoryGuard.pullFastForward(repoPath: fixture.path)
        let newHead = try #require(outcome.newHead)
        #expect(try newHead.hex == fixture.revParse("refs/remotes/origin/main"))
        #expect(outcome.filesChanged == 1)
        #expect(outcome.commitsAdvanced)
        #expect(!outcome.alreadyUpToDate)

        let again = try RepositoryGuard.pullFastForward(repoPath: fixture.path)
        #expect(again.newHead == nil)
        #expect(!again.commitsAdvanced)
        #expect(again.alreadyUpToDate)
    }

    @Test func aDivergedPullThrowsDiverged() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try configureUpstream(fixture)
        try advanceTrackingRef(fixture)
        try fixture.write("local.txt", "local\n")
        try fixture.commit("Local", date: "2024-03-02T00:00:00+0000")
        let head = try fixture.revParse("HEAD")

        do {
            _ = try RepositoryGuard.pullFastForward(repoPath: fixture.path)
            Issue.record("a diverged pull returned instead of throwing")
        } catch {
            guard case .diverged(let message) = error else {
                Issue.record("expected .diverged, got \(error)")
                return
            }
            #expect(message.contains("1 ahead and 1 behind"), "\(message)")
        }
        #expect(try fixture.revParse("HEAD") == head)
    }

    @Test func anScpLikeOriginReportsSshThoughItsUrlLostTheUser() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.git(["remote", "add", "origin", "git@example.com:x/y.git"])
        try fixture.git(["config", "branch.main.remote", "origin"])
        try fixture.git(["config", "branch.main.merge", "refs/heads/main"])

        let status = try RepositoryGuard.remotePreflight(repoPath: fixture.path)
        #expect(status.usesSsh)
        #expect(status.url == "example.com:x/y.git")
        #expect(status.host == "example.com")
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

/// A failed or refused action is titled by what was attempted, never as a failed read.
@Suite struct GuardFailureTitleTests {
    @Test func namesTheVerbInLowerCase() {
        #expect(
            RepositoryGuard.failureTitle(verb: "Delete Branch") == "rc-git could not delete branch")
        #expect(RepositoryGuard.failureTitle(verb: "Commit") == "rc-git could not commit")
    }

    @Test func neverReadsAsAFailedRead() {
        for verb in ["Create Branch", "Delete Tag", "Stash", "Discard Hunk", "Commit"] {
            #expect(RepositoryGuard.failureTitle(verb: verb) != RepositoryGuard.readFailureTitle)
        }
    }
}

/// A ref-name sheet enables Create only once a name is left after trimming.
@Suite struct EnteredRefNameTests {
    @Test func aRefNameSheetNeedsANameLeftAfterTrimming() {
        #expect(RepositoryGuard.enteredRefName("") == nil)
        #expect(RepositoryGuard.enteredRefName("  \t\n") == nil)
        #expect(RepositoryGuard.enteredRefName(" topic\n") == "topic")
        #expect(RepositoryGuard.enteredRefName("v1.0") == "v1.0")
    }
}

/// A token must not leak through any way Swift turns a value into text.
@Suite struct CredentialsRedactionTests {
    private enum Holder {
        case signIn(Credentials)
    }

    @Test func everyTextFormOmitsTheToken() {
        let credentials = Credentials(username: "octocat", token: "TOKEN-VALUE")
        let renderings = [
            "\(credentials)",
            String(describing: credentials),
            String(reflecting: credentials),
            "\(Holder.signIn(credentials))",
            String(reflecting: Holder.signIn(credentials)),
            "\([credentials])",
            "\(Optional(credentials) as Any)",
            String(describing: Mirror(reflecting: credentials).children.map(\.value)),
        ]
        for rendering in renderings {
            #expect(!rendering.contains("TOKEN-VALUE"), "\(rendering)")
            #expect(rendering.contains("<redacted>"), "\(rendering)")
        }
        #expect("\(credentials)".contains("octocat"))
    }
}
