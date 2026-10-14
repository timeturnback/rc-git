import Foundation
import Testing

@testable import RcGitKit

/// Drives the real Rust core through the C ABI against a repository built with the git CLI.
@Suite struct RepositoryIntegrationTests {
    @Test func openingANonRepositoryThrowsWithThePath() throws {
        let fixture = try GitFixture.emptyDirectory()
        defer { fixture.remove() }
        #expect(throws: RepositoryError.self) {
            try Repository(path: fixture.path)
        }
        do {
            _ = try Repository(path: fixture.path)
        } catch let error as RepositoryError {
            #expect(error.message.contains(fixture.path))
        } catch {
            Issue.record("unexpected error \(error)")
        }
    }

    @Test func commonDirectoryIsTheSharedGitDirOfALinkedWorktree() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let main = try Repository(path: fixture.path)
        #expect(main.commonDirectory == main.gitDirectory)

        let linkedPath = outside.path + "/linked"
        try fixture.git(["worktree", "add", "-q", "-b", "wt", linkedPath])
        let linked = try Repository(path: linkedPath)
        let expected = try GitFixture(path: linkedPath).gitOutput([
            "rev-parse", "--path-format=absolute", "--git-common-dir",
        ])
        #expect(
            RefWatcher.canonical(linked.commonDirectory)
                == RefWatcher.canonical(
                    expected.trimmingCharacters(in: .whitespacesAndNewlines)))
        #expect(
            RefWatcher.canonical(linked.gitDirectory)
                != RefWatcher.canonical(linked.commonDirectory))
    }

    @Test func brokenRefIsReportedWithoutHidingTheRest() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write(".git/refs/heads/x", "not a ref\n")
        let listing = try Repository(path: fixture.path).refs()
        #expect(listing.broken.count == 1)
        let broken = try #require(listing.broken.first)
        #expect(broken.fullName == "refs/heads/x")
        #expect(!broken.error.isEmpty)
        #expect(listing.unreadable(matching: "").map(\.fullName) == ["refs/heads/x"])
        let names = Set(listing.refs.map(\.shortName))
        #expect(names == ["main", "feature", "origin/main", "v0.9", "v1.0"])
    }

    @Test func headCrossesDetachedAndUnbornAndABareRepositoryHasNoWorkingDirectory() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.git(["checkout", "-q", "--detach", "HEAD"])
        let detached = try Repository(path: fixture.path).head()
        #expect(detached.refName == nil)
        #expect(try detached.target?.hex == fixture.revParse("HEAD"))

        let unbornFixture = try GitFixture.emptyDirectory()
        defer { unbornFixture.remove() }
        try unbornFixture.git(["init", "-q", "-b", "work"])
        let unborn = try Repository(path: unbornFixture.path).head()
        #expect(unborn.target == nil)
        #expect(unborn.refName == "refs/heads/work")

        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let barePath = outside.path + "/bare.git"
        try fixture.git(["clone", "--bare", "--quiet", ".", barePath])
        let bare = try Repository(path: barePath)
        #expect(bare.workingDirectory == nil)
    }

    @Test func readsRefsTreesBlobsAndHistory() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        #expect(repo.gitDirectory.hasSuffix(".git"))
        #expect(repo.workingDirectory != nil)

        let head = try repo.head()
        #expect(head.refName == "refs/heads/main")
        #expect(try head.target?.hex == fixture.revParse("HEAD"))

        #expect(
            try repo.defaultBranch() == "main",
            "the fixture has no remote HEAD, so the local mainline answers")

        let listing = try repo.refs()
        #expect(listing.broken.isEmpty)
        let names = Set(listing.refs.map(\.shortName))
        #expect(names == ["main", "feature", "origin/main", "v0.9", "v1.0"])
        let tag = try #require(listing.refs.first { $0.shortName == "v1.0" })
        #expect(tag.kind == .tag)
        #expect(try tag.target.hex == fixture.revParse("v1.0^{commit}"))

        let root = try repo.resolveTree("main")
        #expect(try root.hex == fixture.revParse("main^{tree}"))
        #expect(try repo.treeOf(tag.target) == root)
        let entries = try repo.treeEntries(root)
        #expect(
            entries.map(\.name) == [
                "docs", "src", "alpha.txt", "bin.dat", "link", "README.md", "Zeta.txt",
            ])
        try #require(entries.count == 7)
        #expect(entries[0].kind == .tree)
        #expect(entries[4].kind == .symlink)

        let mainRs = try repo.resolve("main:src/main.rs")
        #expect(try mainRs.hex == fixture.revParse("main:src/main.rs"))
        guard case let .text(data, lineStarts, validUTF8) = try repo.readBlob(mainRs) else {
            Issue.record("expected text")
            return
        }
        #expect(String(decoding: data, as: UTF8.self) == "fn main() {\n    println!(\"hi\");\n}\n")
        #expect(lineStarts == [0, 12, 32])
        #expect(validUTF8)

        let bin = try repo.resolve("main:bin.dat")
        #expect(try repo.readBlob(bin) == .binary(size: 12))
        let readme = try repo.resolve("main:README.md")
        #expect(try repo.readBlob(readme, limit: 3) == .tooLarge(size: 17, limit: 3))

        let tip = try repo.resolve("main")
        let commits = try repo.logCursor(tip: tip).next(limit: 100)
        #expect(commits.map(\.title) == ["Add docs and library", "Initial import"])
        try #require(commits.count == 2)
        #expect(commits[0].parents == [commits[1].oid])
        #expect(commits[1].authorName == "Test Author")
        #expect(commits[1].authorTime == 1_704_067_200)
        #expect(try repo.commit(commits[1].oid) == commits[1])
        let cursor = try repo.logCursor(tip: tip)
        #expect(try cursor.next(limit: 1).map(\.title) == ["Add docs and library"])
        #expect(try cursor.next(limit: 5).map(\.title) == ["Initial import"], "it resumes")
        #expect(try cursor.next(limit: 5).isEmpty)

        #expect(throws: RepositoryError.self) { try repo.resolve("nope") }
        #expect(throws: RepositoryError.self) { try repo.treeEntries(mainRs) }
    }

    /// Names git stores as bytes that are not UTF-8 arrive flagged, so nothing sends their
    /// U+FFFD spelling back as a path or ref name.
    @Test func inexactNameTreeEntriesAndRefsArriveFlagged() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let blob = try #require(Oid(hex: try fixture.revParse("main:README.md")))
        let raw = Array("100644 f".utf8) + [0xFF] + Array("o".utf8) + [0] + blob.bytes
        try fixture.writeBytes("raw-tree", raw)
        let treeHex = try fixture.gitOutput(["hash-object", "-w", "-t", "tree", "raw-tree"])
        let packed = URL(fileURLWithPath: fixture.path).appendingPathComponent(".git/packed-refs")
        var refs = (try? Data(contentsOf: packed)) ?? Data()
        let main = try fixture.revParse("main")
        refs.append(contentsOf: Array("\(main) refs/tags/v".utf8) + [0xE9, 0x0A])
        try refs.write(to: packed)

        let repo = try Repository(path: fixture.path)
        let tree = try #require(Oid(hex: treeHex))
        let entries = try repo.treeEntries(tree)
        #expect(entries.map(\.name) == ["f\u{FFFD}o"])
        #expect(entries.map(\.hasExactName) == [false])
        let mainEntries = try repo.treeEntries(repo.resolveTree("main"))
        #expect(mainEntries.allSatisfy { $0.hasExactName })

        let listing = try repo.refs()
        let tag = try #require(listing.refs.first { $0.shortName == "v\u{FFFD}" })
        #expect(!tag.hasExactName)
        let others = listing.refs.filter { $0.shortName != "v\u{FFFD}" }
        #expect(others.allSatisfy { $0.hasExactName })
    }

    @Test func walksPathHistoryWithAScanCursor() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        let tip = try repo.resolve("main")
        let library = CommitFilter(path: "src/lib.rs")

        let page = try repo.historyWalk(tip: tip, filter: library).next(limit: 10, maxScan: 100)
        #expect(page.commits.map(\.title) == ["Add docs and library"])
        #expect(page.scanned == 2)
        #expect(page.complete)

        let capped = try repo.historyWalk(tip: tip, filter: library).next(limit: 10, maxScan: 1)
        #expect(capped.commits.map(\.title) == ["Add docs and library"])
        #expect(capped.scanned == 1)
        #expect(!capped.complete)

        let readme = try repo.historyWalk(tip: tip, filter: CommitFilter(path: "README.md"))
        let first = try readme.next(limit: 10, maxScan: 1)
        #expect(first.commits.isEmpty)
        #expect(first.scanned == 1)
        #expect(!first.complete)
        let rest = try readme.next(limit: 10, maxScan: 1)
        #expect(rest.commits.map(\.title) == ["Initial import"], "the walk resumes")
        #expect(rest.scanned == 1)
        #expect(rest.complete)

        let missing = try repo.historyWalk(tip: tip, filter: CommitFilter(path: "nope.txt"))
            .next(limit: 10, maxScan: 100)
        #expect(missing.commits.isEmpty)
        #expect(missing.complete)
    }

    @Test func searchesCommitsByMessageAuthorAndTime() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        let tip = try repo.resolve("main")
        let search = { (filter: CommitFilter) in
            try repo.historyWalk(tip: tip, filter: filter).next(limit: 10, maxScan: 100)
        }

        let byMessage = try search(CommitFilter(message: "DOCS"))
        #expect(byMessage.commits.map(\.title) == ["Add docs and library"])
        #expect(byMessage.complete)

        let byAuthor = try search(CommitFilter(author: "example.com"))
        #expect(byAuthor.commits.count == 2)

        let until = try search(CommitFilter(before: 1_704_067_200))
        #expect(until.commits.map(\.title) == ["Initial import"])

        let since = try search(CommitFilter(after: 1_704_067_201))
        #expect(since.commits.map(\.title) == ["Add docs and library"])

        let nothing = try search(CommitFilter(message: "zzz"))
        #expect(nothing.commits.isEmpty)
        #expect(nothing.complete)

        let inLibrary = try search(CommitFilter(message: "docs", path: "src/lib.rs"))
        #expect(inLibrary.commits.map(\.title) == ["Add docs and library"])
        let inReadme = try search(CommitFilter(message: "docs", path: "README.md"))
        #expect(inReadme.commits.isEmpty, "the docs commit never touched README.md")

        let initial = try repo.historyWalk(tip: tip, filter: CommitFilter(message: "Initial"))
        let first = try initial.next(limit: 10, maxScan: 1)
        #expect(first.commits.isEmpty)
        #expect(first.scanned == 1)
        #expect(!first.complete)
        let rest = try initial.next(limit: 10, maxScan: 1)
        #expect(rest.commits.map(\.title) == ["Initial import"], "the walk resumes")
        #expect(rest.complete)
    }

    @Test func readsCommitDetails() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.git(
            [
                "commit", "-q", "--allow-empty", "-m", "Detail commit", "-m", "The body.",
                "--date", "2024-01-04T00:00:00+0000",
            ],
            extraEnvironment: ["GIT_COMMITTER_DATE": "2024-01-04T00:00:00+0000"])
        let repo = try Repository(path: fixture.path)
        let tip = try repo.resolve("main")

        let detail = try repo.commitDetails(tip)
        #expect(detail.summary == (try repo.commit(tip)))
        #expect(detail.summary.title == "Detail commit")
        #expect(detail.body == "The body.")
        #expect(detail.committerName == "Test Committer")
        #expect(detail.committerEmail == "committer@example.com")
        #expect(detail.committerTime == 1_704_326_400)
        #expect(try detail.tree.hex == fixture.revParse("main^{tree}"))

        let titledOnly = try repo.resolve("main~1")
        #expect(try repo.commitDetails(titledOnly).body.isEmpty)

        let tree = try repo.resolveTree("main")
        #expect(throws: RepositoryError.self) { try repo.commitDetails(tree) }
    }

    @Test func diffsTreesAndBlobsAndBlames() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        let old = try repo.resolve("v0.9")
        let new = try repo.resolve("main")

        let changes = try repo.diffTrees(from: old, to: new)
        #expect(
            changes.map(\.path) == ["Zeta.txt", "alpha.txt", "docs/guide.md", "link", "src/lib.rs"])
        #expect(changes.allSatisfy { $0.kind == .added && $0.old == nil && $0.new != nil })
        let link = try #require(changes.first { $0.path == "link" })
        #expect(link.entry == .symlink)

        let fromNothing = try repo.diffTrees(from: nil, to: old)
        #expect(fromNothing.count == 3, "a root commit diffs against the empty tree")

        let guide = try #require(changes.first { $0.path == "docs/guide.md" })
        guard case let .text(hunks) = try repo.diffBlobs(old: nil, new: guide.new) else {
            Issue.record("expected a text diff")
            return
        }
        try #require(hunks.count == 1)
        #expect(hunks[0].lines == [DiffLine(kind: .added, text: "guide")])
        #expect((hunks[0].newStart, hunks[0].newLines) == (1, 1))

        let bin = try repo.resolve("main:bin.dat")
        #expect(
            try repo.diffBlobs(old: bin, new: guide.new) == .binary(oldSize: 12, newSize: 6))
        #expect(
            try repo.diffBlobs(old: guide.new, new: guide.new, limit: 2)
                == .tooLarge(size: 6, limit: 2))

        guard case let .ranges(spans) = try repo.blame(tip: new, path: "README.md") else {
            Issue.record("expected ranges")
            return
        }
        try #require(spans.count == 1)
        #expect(spans[0].startLine == 1)
        #expect(spans[0].lineCount == 3)
        #expect(try spans[0].commit.hex == fixture.revParse("v0.9^{commit}"))
        #expect(!spans[0].boundary)

        guard case let .ranges(stopped) = try repo.blame(tip: new, path: "README.md", since: .max)
        else {
            Issue.record("expected ranges")
            return
        }
        let marked = try #require(stopped.first)
        #expect(try marked.commit.hex == fixture.revParse("main"))
        #expect(marked.boundary, "a floor past the tip leaves every line at the tip, marked")

        let declined = try repo.blame(tip: new, path: "README.md", limit: 2)
        #expect(declined == .tooLarge(size: 17, limit: 2))
        #expect(throws: RepositoryError.self) { try repo.blame(tip: new, path: "nope.txt") }
    }

    @Test func diffsModifiedDeletedAndRenamedPathsAndEditedLines() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write("README.md", "# fixture\n\nhi\n")
        try fixture.git(["mv", "alpha.txt", "beta.txt"])
        try fixture.git(["rm", "-q", "Zeta.txt"])
        try fixture.commit("Edit", date: "2024-01-05T00:00:00+0000")
        let repo = try Repository(path: fixture.path)

        let changes = try repo.diffTrees(from: try repo.resolve("main~1"), to: repo.resolve("HEAD"))
        #expect(changes.map(\.path) == ["README.md", "Zeta.txt", "beta.txt"])
        #expect(changes.map(\.kind) == [.modified, .deleted, .renamed])
        let zeta = try #require(changes.first { $0.path == "Zeta.txt" })
        #expect(zeta.new == nil)
        #expect(zeta.old != nil)
        let beta = try #require(changes.first { $0.path == "beta.txt" })
        #expect(beta.sourcePath == "alpha.txt")

        let readme = try #require(changes.first { $0.path == "README.md" })
        guard case let .text(hunks) = try repo.diffBlobs(old: readme.old, new: readme.new) else {
            Issue.record("expected a text diff")
            return
        }
        #expect(
            hunks.flatMap(\.lines) == [
                DiffLine(kind: .context, text: "# fixture"),
                DiffLine(kind: .context, text: ""),
                DiffLine(kind: .removed, text: "hello"),
                DiffLine(kind: .added, text: "hi"),
            ])
    }

    @Test func defaultBranchIsNilWhenNothingNamesAMainline() throws {
        let fixture = try GitFixture.emptyDirectory()
        defer { fixture.remove() }
        try fixture.git(["init", "-q", "-b", "trunk"])
        try fixture.write("a.txt", "a\n")
        try fixture.commit("one", date: "2024-01-01T00:00:00+0000")
        #expect(try Repository(path: fixture.path).defaultBranch() == nil)
    }

    @Test func diffLinesCarryTheirLineEnding() throws {
        let fixture = try GitFixture.emptyDirectory()
        defer { fixture.remove() }
        try fixture.git(["init", "-q", "-b", "main"])
        try fixture.write("f.txt", "x\na")
        try fixture.commit("one", date: "2024-01-01T00:00:00+0000")
        try fixture.write("f.txt", "x\r\na\n")
        try fixture.commit("two", date: "2024-01-02T00:00:00+0000")
        let repo = try Repository(path: fixture.path)
        let old = try #require(Oid(hex: try fixture.revParse("main~1:f.txt")))
        let new = try #require(Oid(hex: try fixture.revParse("main:f.txt")))

        guard case let .text(hunks) = try repo.diffBlobs(old: old, new: new) else {
            Issue.record("expected a text diff")
            return
        }
        #expect(
            hunks.flatMap(\.lines) == [
                DiffLine(kind: .removed, text: "x", eol: .lf),
                DiffLine(kind: .removed, text: "a", eol: .none),
                DiffLine(kind: .added, text: "x", eol: .crlf),
                DiffLine(kind: .added, text: "a", eol: .lf),
            ])
    }

    @Test func onlyAPlainNewlineGoesUnmarked() {
        #expect(LineEnd.lf.marker == nil)
        #expect(LineEnd.crlf.marker?.contains("CRLF") == true)
        #expect(LineEnd.none.marker?.contains("no newline at end of file") == true)
    }

    @Test func listsAndReadsTheWorkingTree() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write("untracked.txt", "new\n")
        try fixture.write(".gitignore", "ignored.txt\n")
        try fixture.write("ignored.txt", "skip\n")
        try FileManager.default.removeItem(
            at: URL(fileURLWithPath: fixture.path).appendingPathComponent("Zeta.txt"))
        let repo = try Repository(path: fixture.path)
        let tree = try repo.resolveTree("HEAD")

        let listing = try repo.worktreeEntries(tree: tree, dir: "")
        #expect(listing.missing.map(\.name) == ["Zeta.txt"])
        #expect(try listing.missing.first?.oid.hex == fixture.revParse("HEAD:Zeta.txt"))
        let untracked = try #require(listing.entries.first { $0.name == "untracked.txt" })
        #expect(untracked.status == .untracked)
        #expect(untracked.oid == nil)
        let ignored = try #require(listing.entries.first { $0.name == "ignored.txt" })
        #expect(ignored.status == .ignored)
        let readme = try #require(listing.entries.first { $0.name == "README.md" })
        #expect(readme.status == .unchanged)
        #expect(try readme.oid?.hex == fixture.revParse("HEAD:README.md"))

        guard case let .text(data, _, _) = try repo.worktreeFile(path: "untracked.txt") else {
            Issue.record("expected text")
            return
        }
        #expect(String(decoding: data, as: UTF8.self) == "new\n")
        #expect(throws: RepositoryError.self) { try repo.worktreeFile(path: "nope.txt") }
    }

    @Test func listsEveryFileOnDiskIncludingIgnoredOnes() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write(".gitignore", ".env\nbuild/\n")
        try fixture.write(".env", "SECRET=1\n")
        try fixture.write("build/out.o", "x\n")
        let repo = try Repository(path: fixture.path)
        let tree = try repo.resolveTree("HEAD")

        let list = try repo.worktreeFiles(tree: tree)
        #expect(!list.truncated)
        let paths = list.files.map(\.path)
        #expect(paths.first == ".env")
        #expect(paths.contains("src/main.rs"))
        #expect(!paths.contains("build/out.o"))
        #expect(list.files.first { $0.path == "link" }?.kind == .symlink)

        let capped = try repo.worktreeFiles(tree: tree, maxScan: 1)
        #expect(capped.truncated)
    }

    @Test func scansAnUnbornBranchWithNoTree() throws {
        let fixture = try GitFixture.emptyDirectory()
        defer { fixture.remove() }
        try fixture.git(["init", "-q", "-b", "main"])
        try fixture.write("a.txt", "hi\n")
        let repo = try Repository(path: fixture.path)
        #expect(try repo.head().target == nil)

        let report = try repo.worktreeStatus(tree: nil)
        #expect(report.entries.map(\.path) == ["a.txt"])
        #expect(report.entries.map(\.status) == [.untracked])
    }

    @Test func statusReportsTheIgnoredFoldersItPruned() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write(".gitignore", "target/\n")
        try fixture.write("target/debug/x.o", "object\n")
        try fixture.write("tools/target/y.o", "object\n")
        let repo = try Repository(path: fixture.path)

        let report = try repo.worktreeStatus(tree: try repo.resolveTree("HEAD"))
        #expect(report.ignoredDirs == ["target", "tools/target"])
        #expect(!report.entries.contains { $0.path.hasPrefix("target/") })
    }

    @Test func scansStatusDiffsWorktreeFilesAndListsStashes() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        try fixture.write("alpha.txt", "stash me\n")
        try fixture.git(["stash", "push", "-q", "-m", "kept"])
        try fixture.write("alpha.txt", "alpha\nbeta\n")
        try fixture.write("untracked.txt", "new\n")
        try FileManager.default.removeItem(
            at: URL(fileURLWithPath: fixture.path).appendingPathComponent("Zeta.txt"))
        let repo = try Repository(path: fixture.path)
        let tree = try repo.resolveTree("HEAD")

        let report = try repo.worktreeStatus(tree: tree)
        #expect(!report.truncated)
        #expect(report.scanned > 0)
        #expect(try repo.worktreeStatus(tree: tree, maxScan: 1).truncated)
        let rows = report.entries.map { ($0.path, $0.status) }
        #expect(rows.map(\.0) == ["Zeta.txt", "alpha.txt", "untracked.txt"])
        #expect(rows.map(\.1) == [.deleted, .modified, .untracked])
        try #require(report.entries.count == 3)
        let modified = report.entries[1]
        #expect(try modified.oid?.hex == fixture.revParse("HEAD:alpha.txt"))

        guard
            case let .text(hunks) = try repo.diffWorktreeFile(
                path: "alpha.txt", old: modified.oid)
        else {
            Issue.record("expected a text diff")
            return
        }
        #expect(hunks.count == 1)
        #expect(throws: RepositoryError.self) {
            try repo.diffWorktreeFile(path: "nope.txt", old: nil)
        }

        let stashes = try repo.stashList()
        try #require(stashes.count == 1)
        #expect(stashes[0].index == 0)
        #expect(stashes[0].message.hasSuffix("kept"))
        #expect(try stashes[0].oid.hex == fixture.revParse("stash@{0}"))
    }

    @Test func repositoryIsUsableFromManyThreadsAtOnce() async throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        let root = try repo.resolveTree("main")
        let expected = try repo.treeEntries(root)
        try await withThrowingTaskGroup(of: [TreeEntry].self) { group in
            for _ in 0..<16 {
                group.addTask { try repo.treeEntries(root) }
            }
            for try await entries in group {
                #expect(entries == expected)
            }
        }
    }
}
