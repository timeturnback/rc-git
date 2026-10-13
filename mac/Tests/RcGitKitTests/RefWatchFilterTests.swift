import Testing

@testable import RcGitKit

/// The main worktree's layout, where the worktree's git dir is also the common dir.
private func mainLayout(_ path: String, gitDir: String) -> Bool {
    RefWatchFilter.isRelevant(path, gitDir: gitDir, commonDir: gitDir)
}

@Suite struct RefWatchFilterTests {
    let gitDir = "/tmp/repo/.git"

    @Test func refPathsAreRelevant() {
        #expect(mainLayout("/tmp/repo/.git/HEAD", gitDir: gitDir))
        #expect(mainLayout("/tmp/repo/.git/packed-refs", gitDir: gitDir))
        #expect(mainLayout("/tmp/repo/.git/refs", gitDir: gitDir))
        #expect(mainLayout("/tmp/repo/.git/refs/", gitDir: gitDir))
        #expect(mainLayout("/tmp/repo/.git/refs/heads/main", gitDir: gitDir))
        #expect(
            mainLayout("/tmp/repo/.git/refs/remotes/origin/main", gitDir: gitDir))
        #expect(mainLayout("/tmp/repo/.git/", gitDir: gitDir))
    }

    @Test func objectAndIndexChurnIsIgnored() {
        #expect(!mainLayout("/tmp/repo/.git/objects/ab/cdef", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/index", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/logs/HEAD", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/COMMIT_EDITMSG", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/refs/heads/main.lock", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/HEAD.lock", gitDir: gitDir))
        #expect(!mainLayout("/tmp/other/.git/HEAD", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.github/refs", gitDir: gitDir))
    }

    @Test func configEditsAreRelevantButItsLockIsNot() {
        #expect(mainLayout("/tmp/repo/.git/config", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/config.lock", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git/configx", gitDir: gitDir))
    }

    @Test func trailingSlashOnGitDirIsTolerated() {
        #expect(mainLayout("/tmp/repo/.git/HEAD", gitDir: gitDir + "/"))
    }

    @Test func aSiblingSharingTheGitDirPrefixIsDropped() {
        #expect(!mainLayout("/tmp/repo/.gitrefs/heads/x", gitDir: gitDir))
        #expect(!mainLayout("/tmp/repo/.git2/HEAD", gitDir: gitDir))
    }

    @Test func realpathEventsMatchACanonicalRoot() {
        let canonical = "/private/tmp/repo/.git"
        #expect(
            mainLayout("/private/tmp/repo/.git/refs/heads/main", gitDir: canonical))
        #expect(!mainLayout("/private/tmp/repo/.git/index", gitDir: canonical))
    }

    @Test func aLinkedWorktreeSeesTheSharedRefsAndItsOwnHead() {
        let common = "/repo/.git"
        let linked = "/repo/.git/worktrees/linked"
        func relevant(_ path: String) -> Bool {
            RefWatchFilter.isRelevant(path, gitDir: linked, commonDir: common)
        }
        #expect(
            RefWatchFilter.isRelevant(
                "/repo/.git/refs/heads/feat", gitDir: "/repo/.git/worktrees/feat",
                commonDir: common))
        #expect(relevant("/repo/.git/refs/heads/wt"))
        #expect(relevant("/repo/.git/refs/stash"))
        #expect(relevant("/repo/.git/packed-refs"))
        #expect(relevant("/repo/.git/config"))
        #expect(relevant("/repo/.git"))
        #expect(relevant("/repo/.git/worktrees/linked/HEAD"))
        #expect(relevant("/repo/.git/worktrees/linked/refs/bisect/bad"))
    }

    @Test func aLinkedWorktreeIgnoresOtherWorktreesAndItsOwnChurn() {
        let common = "/repo/.git"
        let linked = "/repo/.git/worktrees/linked"
        func relevant(_ path: String) -> Bool {
            RefWatchFilter.isRelevant(path, gitDir: linked, commonDir: common)
        }
        #expect(!relevant("/repo/.git/worktrees/other/HEAD"))
        #expect(!relevant("/repo/.git/worktrees/other/refs/bisect/bad"))
        #expect(!relevant("/repo/.git/worktrees/linked/index"))
        #expect(!relevant("/repo/.git/worktrees/linked/HEAD.lock"))
        #expect(!relevant("/repo/.git/worktrees/linked/logs/HEAD"))
        #expect(!relevant("/repo/.git/worktrees/linked"))
        #expect(!relevant("/repo/.git/worktrees"))
        #expect(!relevant("/repo/.git/worktrees/linkedrefs"))
        #expect(!relevant("/repo/.git/worktrees/linkedrefs/heads/x"))
        #expect(!relevant("/repo/.git/refs/heads/wt.lock"))
        #expect(!relevant("/repo/.git/packed-refs.lock"))
        #expect(!relevant("/repo/.git/objects/ab/cdef"))
        #expect(!relevant("/repo/.git/index"))
        #expect(
            !RefWatchFilter.isRelevant(
                "/repo/.git/worktrees/wtrefs", gitDir: "/repo/.git/worktrees/wt",
                commonDir: common))
    }
}
