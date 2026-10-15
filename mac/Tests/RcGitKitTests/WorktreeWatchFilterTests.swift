import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct WorktreeWatchFilterTests {
    let workDir = "/repo"
    let gitDir = "/repo/.git"

    @Test func fileEditsInsideTheWorktreeAreRelevant() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/src/main.rs", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/new.txt", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
    }

    @Test func everythingUnderGitDirIsDropped() {
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git/index", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git/refs/heads/main", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
    }

    @Test func anExcludeFileEditIsRelevant() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/.git/info/exclude", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo/.git/info/attributes", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
    }

    @Test func aLinkedWorktreeReadsTheCommonDirExcludeFile() {
        let linked = "/elsewhere/linked"
        let common = "/repo/.git"
        let own = common + "/worktrees/linked"
        func relevant(_ path: String) -> Bool {
            WorktreeWatchFilter.isRelevant(path, workDir: linked, gitDir: own, commonDir: common)
        }
        #expect(relevant("/repo/.git/info/exclude"), "outside the worktree, but its rules apply")
        #expect(relevant("/elsewhere/linked/a.txt"))
        #expect(!relevant("/repo/.git/worktrees/linked/info/exclude"), "git never reads it")
        #expect(!relevant("/repo/.git/info/attributes"))
        #expect(!relevant("/repo/.git/index"))
        #expect(!relevant("/repo/a.txt"), "the main worktree's files are not this list")
    }

    @Test func aDotGitPrefixedSiblingIsNotTheGitDir() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/.github/workflows/ci.yml", workDir: workDir, gitDir: gitDir,
                commonDir: gitDir))
    }

    @Test func pathsOutsideTheWorktreeAreDropped() {
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/elsewhere/file", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/file", workDir: workDir + "/", gitDir: gitDir, commonDir: gitDir),
            "a trailing slash on the configured root is normalized")
    }

    @Test func aSiblingSharingTheWorkDirPrefixIsDropped() {
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/repo2/x", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
    }

    @Test func writesBelowAPrunedIgnoredFolderAreDropped() {
        let ignored: Set<String> = ["target", "tools/build"]
        func relevant(_ path: String) -> Bool {
            WorktreeWatchFilter.isRelevant(
                path, workDir: workDir, gitDir: gitDir, commonDir: gitDir, ignoredRoots: ignored)
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

    @Test func writesInsideANestedRepositoryAreDroppedButItsGitStaysRelevant() {
        let nested: Set<String> = ["vendor/lib"]
        func relevant(_ path: String) -> Bool {
            WorktreeWatchFilter.isRelevant(
                path, workDir: workDir, gitDir: gitDir, commonDir: gitDir, nestedRoots: nested)
        }
        #expect(!relevant("/repo/vendor/lib/a.txt"))
        #expect(!relevant("/repo/vendor/lib/.git/index"))
        #expect(!relevant("/repo/vendor/lib/.git/objects/ab/cdef"))
        #expect(relevant("/repo/vendor/lib/.git"), "removing it makes the files untracked")
        #expect(relevant("/repo/vendor/lib"), "a file replacing the folder is untracked")
        #expect(relevant("/repo/vendor/lib2/a.txt"), "a sibling sharing the prefix is not below it")
        #expect(relevant("/repo/vendor/x"))
    }

    @Test func noIgnoredFoldersKeepsEveryWorktreeWrite() {
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/repo/target/debug/x.o", workDir: workDir, gitDir: gitDir, commonDir: gitDir))
    }

    @Test func realpathEventsMatchACanonicalRoot() {
        let root = "/private/tmp/r"
        #expect(
            WorktreeWatchFilter.isRelevant(
                "/private/tmp/r/a.txt", workDir: root, gitDir: root + "/.git",
                commonDir: root + "/.git"))
        #expect(
            !WorktreeWatchFilter.isRelevant(
                "/private/tmp/r/.git/index", workDir: root, gitDir: root + "/.git",
                commonDir: root + "/.git"))
    }
}
