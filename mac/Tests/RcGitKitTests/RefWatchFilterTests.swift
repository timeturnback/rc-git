import Testing

@testable import RcGitKit

@Suite struct RefWatchFilterTests {
    let gitDir = "/tmp/repo/.git"

    @Test func refPathsAreRelevant() {
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/HEAD", gitDir: gitDir))
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/packed-refs", gitDir: gitDir))
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/refs", gitDir: gitDir))
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/refs/", gitDir: gitDir))
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/refs/heads/main", gitDir: gitDir))
        #expect(
            RefWatchFilter.isRelevant("/tmp/repo/.git/refs/remotes/origin/main", gitDir: gitDir))
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/", gitDir: gitDir))
    }

    @Test func objectAndIndexChurnIsIgnored() {
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git/objects/ab/cdef", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git/index", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git/logs/HEAD", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git/COMMIT_EDITMSG", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git/refs/heads/main.lock", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git/HEAD.lock", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/other/.git/HEAD", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.github/refs", gitDir: gitDir))
    }

    @Test func trailingSlashOnGitDirIsTolerated() {
        #expect(RefWatchFilter.isRelevant("/tmp/repo/.git/HEAD", gitDir: gitDir + "/"))
    }

    @Test func aSiblingSharingTheGitDirPrefixIsDropped() {
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.gitrefs/heads/x", gitDir: gitDir))
        #expect(!RefWatchFilter.isRelevant("/tmp/repo/.git2/HEAD", gitDir: gitDir))
    }

    @Test func realpathEventsMatchACanonicalRoot() {
        let canonical = "/private/tmp/repo/.git"
        #expect(
            RefWatchFilter.isRelevant("/private/tmp/repo/.git/refs/heads/main", gitDir: canonical))
        #expect(!RefWatchFilter.isRelevant("/private/tmp/repo/.git/index", gitDir: canonical))
    }
}
