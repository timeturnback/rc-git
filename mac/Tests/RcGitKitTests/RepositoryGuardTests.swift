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
}
