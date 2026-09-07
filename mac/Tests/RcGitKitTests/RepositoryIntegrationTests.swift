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

    @Test func readsRefsTreesBlobsAndHistory() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let repo = try Repository(path: fixture.path)
        #expect(repo.gitDirectory.hasSuffix(".git"))
        #expect(repo.workingDirectory != nil)

        let head = try repo.head()
        #expect(head.refName == "refs/heads/main")
        #expect(head.target?.hex == fixture.revParse("HEAD"))

        let listing = try repo.refs()
        #expect(listing.broken.isEmpty)
        let names = Set(listing.refs.map(\.shortName))
        #expect(names == ["main", "feature", "origin/main", "v0.9", "v1.0"])
        let tag = try #require(listing.refs.first { $0.shortName == "v1.0" })
        #expect(tag.kind == .tag)
        #expect(tag.target.hex == fixture.revParse("v1.0^{commit}"))

        let root = try repo.resolveTree("main")
        #expect(root.hex == fixture.revParse("main^{tree}"))
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
        guard case let .text(data, lineStarts) = try repo.readBlob(mainRs.oid) else {
            Issue.record("expected text")
            return
        }
        #expect(String(decoding: data, as: UTF8.self) == "fn main() {\n    println!(\"hi\");\n}\n")
        #expect(lineStarts == [0, 12, 32])

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

        let spans = try repo.blame(tip: new, path: "README.md")
        #expect(spans.count == 1)
        #expect(spans[0].startLine == 1)
        #expect(spans[0].lineCount == 3)
        #expect(spans[0].commit.hex == fixture.revParse("v0.9^{commit}"))

        #expect(throws: RepositoryError.self) { try repo.blame(tip: new, path: "nope.txt") }
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
        #expect(readme.oid?.hex == fixture.revParse("HEAD:README.md"))

        guard case let .text(data, _) = try repo.worktreeFile(path: "untracked.txt") else {
            Issue.record("expected text")
            return
        }
        #expect(String(decoding: data, as: UTF8.self) == "new\n")
        #expect(throws: RepositoryError.self) { try repo.worktreeFile(path: "nope.txt") }
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
