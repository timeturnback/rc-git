import Testing

@testable import RcGitKit

@Suite struct WorktreeWatchFilterTests {
    let workDir = "/repo"
    let gitDir = "/repo/.git"

    @Test func fileEditsInsideTheWorktreeAreRelevant() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/src/main.rs", workDir: workDir, gitDir: gitDir))
        #expect(WorktreeWatchFilter.isRelevant("/repo/new.txt", workDir: workDir, gitDir: gitDir))
        #expect(WorktreeWatchFilter.isRelevant("/repo", workDir: workDir, gitDir: gitDir))
    }

    @Test func everythingUnderGitDirIsDropped() {
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git/index", workDir: workDir, gitDir: gitDir))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git/refs/heads/main", workDir: workDir, gitDir: gitDir))
        #expect(!WorktreeWatchFilter.isRelevant("/repo/.git", workDir: workDir, gitDir: gitDir))
    }

    @Test func aDotGitPrefixedSiblingIsNotTheGitDir() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/.github/workflows/ci.yml", workDir: workDir, gitDir: gitDir))
    }

    @Test func pathsOutsideTheWorktreeAreDropped() {
        #expect(
            !WorktreeWatchFilter.isRelevant("/elsewhere/file", workDir: workDir, gitDir: gitDir))
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/file", workDir: workDir + "/", gitDir: gitDir),
            "a trailing slash on the configured root is normalized")
    }

    @Test func aSiblingSharingTheWorkDirPrefixIsDropped() {
        #expect(!WorktreeWatchFilter.isRelevant("/repo2/x", workDir: workDir, gitDir: gitDir))
    }

    @Test func realpathEventsMatchACanonicalRoot() {
        let root = "/private/tmp/r"
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/private/tmp/r/a.txt", workDir: root, gitDir: root + "/.git"))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/private/tmp/r/.git/index", workDir: root, gitDir: root + "/.git"))
    }
}
