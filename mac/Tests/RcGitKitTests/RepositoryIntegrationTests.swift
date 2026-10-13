import Foundation
import Testing

@testable import RcGitKit

/// Drives the real Rust core through the C ABI against a repository built with the git CLI.
@Suite struct RepositoryIntegrationTests {
    @Test func libraryVersionIsExposed() {
        #expect(Repository.libraryVersion.split(separator: ".").count == 3)
    }

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
        #expect(entries[0].kind == .tree)
        #expect(entries[4].kind == .symlink)

        let mainRs = try #require(try repo.entry(at: "src/main.rs", under: root))
        #expect(mainRs.kind == .blob)
        #expect(try repo.entry(at: "nope", under: root) == nil)
        #expect(try repo.blobSize(mainRs.oid) == 34)
        guard case let .text(data, lineStarts, validUTF8) = try repo.readBlob(mainRs.oid) else {
            Issue.record("expected text")
            return
        }
        #expect(String(decoding: data, as: UTF8.self) == "fn main() {\n    println!(\"hi\");\n}\n")
        #expect(lineStarts == [0, 12, 32])
        #expect(validUTF8)

        let bin = try #require(try repo.entry(at: "bin.dat", under: root))
        #expect(try repo.readBlob(bin.oid) == .binary(size: 12))
        let readme = try #require(try repo.entry(at: "README.md", under: root))
        #expect(try repo.readBlob(readme.oid, limit: 3) == .tooLarge(size: 17, limit: 3))

        let tip = try repo.resolve("main")
        let commits = try repo.log(tip: tip)
        #expect(commits.map(\.title) == ["Add docs and library", "Initial import"])
        #expect(commits[0].parents == [commits[1].oid])
        #expect(commits[1].authorName == "Test Author")
        #expect(commits[1].authorTime == 1_704_067_200)
        #expect(try repo.commit(commits[1].oid) == commits[1])
        #expect(try repo.log(tip: tip, skip: 1, limit: 5).count == 1)

        #expect(throws: RepositoryError.self) { try repo.resolve("nope") }
        #expect(throws: RepositoryError.self) { try repo.treeEntries(mainRs.oid) }
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

        let page = try repo.pathHistory(
            tip: tip, path: "src/lib.rs", scanSkip: 0, limit: 10, maxScan: 100)
        #expect(page.commits.map(\.title) == ["Add docs and library"])
        #expect(page.scanned == 2)
        #expect(page.complete)

        let capped = try repo.pathHistory(
            tip: tip, path: "src/lib.rs", scanSkip: 0, limit: 10, maxScan: 1)
        #expect(capped.scanned == 1)
        #expect(!capped.complete)

        let missing = try repo.pathHistory(
            tip: tip, path: "nope.txt", scanSkip: 0, limit: 10, maxScan: 100)
        #expect(missing.commits.isEmpty)
        #expect(missing.complete)
    }

    @Test func searchesCommitsByMessageAuthorAndTime() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        let tip = try repo.resolve("main")

        let byMessage = try repo.searchCommits(
            tip: tip, filter: CommitFilter(message: "DOCS"), scanSkip: 0, limit: 10, maxScan: 100)
        #expect(byMessage.commits.map(\.title) == ["Add docs and library"])
        #expect(byMessage.complete)

        let byAuthor = try repo.searchCommits(
            tip: tip, filter: CommitFilter(author: "example.com"), scanSkip: 0, limit: 10,
            maxScan: 100)
        #expect(byAuthor.commits.count == 2)

        let until = try repo.searchCommits(
            tip: tip, filter: CommitFilter(before: 1_704_067_200), scanSkip: 0, limit: 10,
            maxScan: 100)
        #expect(until.commits.map(\.title) == ["Initial import"])

        let nothing = try repo.searchCommits(
            tip: tip, filter: CommitFilter(message: "zzz"), scanSkip: 0, limit: 10, maxScan: 100)
        #expect(nothing.commits.isEmpty)
        #expect(nothing.complete)
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
        #expect(hunks.count == 1)
        #expect(hunks[0].lines == [DiffLine(kind: .added, text: "guide")])
        #expect((hunks[0].newStart, hunks[0].newLines) == (1, 1))

        let root = try repo.resolveTree("main")
        let bin = try #require(try repo.entry(at: "bin.dat", under: root))
        guard case .binary = try repo.diffBlobs(old: bin.oid, new: guide.new) else {
            Issue.record("expected binary")
            return
        }
        guard case .tooLarge = try repo.diffBlobs(old: guide.new, new: guide.new, limit: 2)
        else {
            Issue.record("expected too large")
            return
        }

        guard case let .ranges(spans) = try repo.blame(tip: new, path: "README.md") else {
            Issue.record("expected ranges")
            return
        }
        #expect(spans.count == 1)
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
        let repo = try Repository(path: fixture.path)
        let tree = try repo.resolveTree("HEAD")

        let listing = try repo.worktreeEntries(tree: tree, dir: "")
        #expect(listing.missing.isEmpty)
        let untracked = try #require(listing.entries.first { $0.name == "untracked.txt" })
        #expect(untracked.status == .untracked)
        #expect(untracked.oid == nil)
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
        let rows = report.entries.map { ($0.path, $0.status) }
        #expect(rows.map(\.0) == ["Zeta.txt", "alpha.txt", "untracked.txt"])
        #expect(rows.map(\.1) == [.deleted, .modified, .untracked])
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
        #expect(stashes.count == 1)
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
