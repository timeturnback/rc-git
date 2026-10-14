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

    @Test func anExcludeFileEditIsRelevant() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/.git/info/exclude", workDir: workDir, gitDir: gitDir))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git/info/attributes", workDir: workDir, gitDir: gitDir))
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

    @Test func writesBelowAPrunedIgnoredFolderAreDropped() {
        let ignored: Set<String> = ["target", "tools/build"]
        func relevant(_ path: String) -> Bool {
            WorktreeWatchFilter.isRelevant(
                path, workDir: workDir, gitDir: gitDir, ignoredRoots: ignored)
        }
        #expect(!relevant("/repo/target/debug/x.o"))
        #expect(!relevant("/repo/target/x"))
        #expect(!relevant("/repo/tools/build/out/y.o"))
        #expect(relevant("/repo/target"), "a file replacing the folder may not be ignored")
        #expect(relevant("/repo/target2/x"), "a sibling sharing the prefix is not below it")
        #expect(relevant("/repo/tools/x"))
        #expect(relevant("/repo/src/target/x"), "only the pruned path, not every folder so named")
        #expect(relevant("/repo/.gitignore"), "an ignore-rule edit can un-ignore the folder")
        #expect(relevant("/repo"))
    }

    @Test func noIgnoredFoldersKeepsEveryWorktreeWrite() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/target/debug/x.o", workDir: workDir, gitDir: gitDir))
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
