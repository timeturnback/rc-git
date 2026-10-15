import Foundation
import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct FuzzyMatchTests {
    @Test func onlySubsequencesMatch() {
        #expect(FuzzyMatch.score("src/main.rs", query: "mainrs") != nil)
        #expect(FuzzyMatch.score("src/main.rs", query: "xyz") == nil)
        #expect(FuzzyMatch.score("src/main.rs", query: "srm") != nil)
        #expect(FuzzyMatch.score("abc", query: "abcd") == nil)
        #expect(FuzzyMatch.score("", query: "a") == nil)
        #expect(FuzzyMatch.score("anything", query: "") == 0)
    }

    @Test func matchingIsCaseInsensitive() {
        #expect(FuzzyMatch.score("README.md", query: "readme") != nil)
        #expect(FuzzyMatch.score("readme.md", query: "README") != nil)
    }

    @Test func fileNameMatchesBeatDirectoryMatches() throws {
        let inName = try #require(FuzzyMatch.score("docs/guide.md", query: "guide"))
        let inDirectory = try #require(FuzzyMatch.score("guide/notes.md", query: "guide"))
        #expect(inName > inDirectory)
    }

    @Test func fileNamedLikeItsFolderKeepsTheFileNameBonus() throws {
        let named = try #require(FuzzyMatch.score("Sources/Model/Model.swift", query: "model"))
        let sibling = try #require(FuzzyMatch.score("Sources/Model/View.swift", query: "model"))
        #expect(named > sibling)
    }

    @Test func consecutiveRunsBeatScatteredMatches() throws {
        let run = try #require(FuzzyMatch.score("browser.swift", query: "brow"))
        let scattered = try #require(FuzzyMatch.score("b_r_o_w_x.swift", query: "brow"))
        #expect(run > scattered)
    }

    @Test func nonAsciiQueriesMatchWholeCharacters() {
        #expect(FuzzyMatch.score("tiếng/bá.md", query: "ạ") == nil)
        #expect(FuzzyMatch.score("è©.txt", query: "é") == nil)
        #expect(FuzzyMatch.score("tiếng/bạn.md", query: "ạ") != nil)
        #expect(FuzzyMatch.score("tiếng/bạn.md", query: "bn") != nil)
    }
}

@Suite(.timeLimit(.minutes(1))) struct PathIndexTests {
    let root = oid(1)
    let sub = oid(2)
    let deep = oid(3)

    func trees() -> [Oid: [TreeEntry]] {
        [
            root: [
                TreeEntry(name: "src", oid: sub, kind: .tree),
                TreeEntry(name: "README.md", oid: oid(0x10), kind: .blob),
                TreeEntry(name: "vendor", oid: oid(0x60), kind: .submodule),
            ],
            sub: [
                TreeEntry(name: "nested", oid: deep, kind: .tree),
                TreeEntry(name: "main.rs", oid: oid(0x11), kind: .blob),
                TreeEntry(name: "run.sh", oid: oid(0x12), kind: .executableBlob),
            ],
            deep: [
                TreeEntry(name: "link", oid: oid(0x13), kind: .symlink)
            ],
        ]
    }

    @Test func buildFlattensEveryFileAndSkipsSubmodules() async throws {
        let fixed = trees()
        let index = try await PathIndex.build(root: root) { try #require(fixed[$0]) }
        let paths = index.entries.map(\.path)
        #expect(
            paths == ["src/nested/link", "src/main.rs", "src/run.sh", "README.md"],
            "folders first, each in full and in order, then a folder's own files")
        #expect(!index.truncated)
        #expect(index.entries.first { $0.path == "src/main.rs" }?.name == "main.rs")
    }

    @Test func buildListsSiblingFoldersInOrderBeforeFiles() async throws {
        let first = oid(0x20)
        let second = oid(0x21)
        let fixed: [Oid: [TreeEntry]] = [
            root: [
                TreeEntry(name: "a", oid: first, kind: .tree),
                TreeEntry(name: "b", oid: second, kind: .tree),
                TreeEntry(name: "z.txt", oid: oid(0x22), kind: .blob),
            ],
            first: [TreeEntry(name: "x", oid: oid(0x23), kind: .blob)],
            second: [TreeEntry(name: "y", oid: oid(0x24), kind: .blob)],
        ]
        let index = try await PathIndex.build(root: root) { try #require(fixed[$0]) }
        #expect(try index.search("").map(\.path) == ["a/x", "b/y", "z.txt"])
        let capped = try await PathIndex.build(root: root, limit: 1) { try #require(fixed[$0]) }
        #expect(capped.entries.map(\.path) == ["a/x"], "the cut keeps the first folder")
    }

    @Test func inexactNameMarksItsPathAndEveryPathBelowIt() async throws {
        var fixed = trees()
        fixed[root]?[0] = TreeEntry(name: "s\u{FFFD}c", oid: sub, kind: .tree, hasExactName: false)
        fixed[root]?.append(
            TreeEntry(name: "f\u{FFFD}o", oid: oid(0x14), kind: .blob, hasExactName: false))
        let index = try await PathIndex.build(root: root) { try #require(fixed[$0]) }
        let inexact = index.entries.filter { !$0.hasExactPath }.map(\.path).sorted()
        #expect(
            inexact == [
                "f\u{FFFD}o", "s\u{FFFD}c/main.rs", "s\u{FFFD}c/nested/link", "s\u{FFFD}c/run.sh",
            ])
        #expect(index.entries.first { $0.path == "README.md" }?.hasExactPath == true)
    }

    @Test func searchRanksAndLimits() async throws {
        let fixed = trees()
        let index = try await PathIndex.build(root: root) { try #require(fixed[$0]) }
        let matches = try index.search("mainrs")
        #expect(matches.first?.path == "src/main.rs")
        #expect(try index.search("zzz").isEmpty)
        #expect(try index.search("").count == index.entries.count)
        #expect(try index.search("", limit: 2).count == 2)
    }

    @Test func tiesOrderByLengthThenPath() throws {
        let entries = ["src/main.rs", "b/main.rs", "a/main.rs"].map {
            PathIndexEntry(path: $0, oid: oid(0x11), kind: .blob, hidden: false)
        }
        let index = PathIndex(root: root, entries: entries, truncated: false)
        let ranked = ["a/main.rs", "b/main.rs", "src/main.rs"]
        #expect(try index.search("main").map(\.path) == ranked)
        #expect(try index.search(" main ").map(\.path) == ranked, "the query is trimmed")
        #expect(try index.search("   ").count == 3, "a blank query lists every entry")
    }

    @Test func tiesCompareLengthInUTF8Bytes() throws {
        let entries = ["\u{E9}/main.rs", "ab/main.rs"].map {
            PathIndexEntry(path: $0, oid: oid(0x11), kind: .blob, hidden: false)
        }
        let index = PathIndex(root: root, entries: entries, truncated: false)
        #expect(
            try index.search("main").map(\.path) == ["ab/main.rs", "\u{E9}/main.rs"],
            "both paths are 10 bytes, so the tie falls through to path order")
    }

    @Test func searchPutsTheFileNamedLikeItsFolderFirst() throws {
        let paths = [
            "components/button/index.ts", "components/button/Button.tsx",
            "components/button/styles.css",
        ]
        let entries = paths.map {
            PathIndexEntry(path: $0, oid: .unhashed, kind: .blob, hidden: false)
        }
        let index = PathIndex(root: root, entries: entries, truncated: false)
        #expect(try index.search("button").first?.path == "components/button/Button.tsx")
    }

    @Test func hiddenFilesAreMarkedAndLeftOutOnRequest() async throws {
        var fixed = trees()
        let meta = oid(0x70)
        fixed[root]! += [
            TreeEntry(name: ".DS_Store", oid: oid(0x71), kind: .blob),
            TreeEntry(name: ".git", oid: meta, kind: .tree),
            TreeEntry(name: ".env", oid: oid(0x72), kind: .blob),
        ]
        fixed[meta] = [TreeEntry(name: "HEAD", oid: oid(0x73), kind: .blob)]
        let index = try await PathIndex.build(root: root) { try #require(fixed[$0]) }
        let hidden = index.entries.filter(\.hidden).map(\.path).sorted()
        #expect(hidden == [".DS_Store", ".git/HEAD"], "a hidden folder hides what is inside it")

        let all = try index.search("", limit: 100).map(\.path)
        #expect(all.contains(".DS_Store"))
        let visible = try index.search("", limit: 100, includeHidden: false).map(\.path)
        #expect(
            visible.sorted() == [
                ".env", "README.md", "src/main.rs", "src/nested/link", "src/run.sh",
            ])
        #expect(try index.search("head", includeHidden: false).isEmpty)
        #expect(try index.search("head").map(\.path) == [".git/HEAD"])
    }

    @Test func buildStopsAtTheLimitAndSaysSo() async throws {
        let fixed = trees()
        let capped = try await PathIndex.build(root: root, limit: 2) { try #require(fixed[$0]) }
        #expect(capped.entries.count == 2)
        #expect(capped.truncated, "a walk cut at the limit is flagged")
        let whole = try await PathIndex.build(root: root, limit: 4) { try #require(fixed[$0]) }
        #expect(whole.entries.count == 4)
        #expect(!whole.truncated, "a walk that fits the limit exactly drops nothing")
        let roomy = try await PathIndex.build(root: root, limit: 5) { try #require(fixed[$0]) }
        #expect(roomy.entries.count == 4)
        #expect(!roomy.truncated)
    }

    @Test func buildStopsAtTheEntryCap() async throws {
        func flat(_ count: Int) async throws -> PathIndex {
            let files = (0..<count).map { TreeEntry(name: "f\($0)", oid: oid(0x77), kind: .blob) }
            return try await PathIndex.build(root: root) { _ in files }
        }
        let over = try await flat(PathIndex.maxEntries + 1)
        #expect(over.truncated)
        #expect(over.entries.count == PathIndex.maxEntries)
        let exact = try await flat(PathIndex.maxEntries)
        #expect(!exact.truncated, "nothing was dropped")
        #expect(exact.entries.count == PathIndex.maxEntries)
        #expect(try await !flat(PathIndex.maxEntries - 1).truncated)
    }

    @Test func buildStopsAtTheNextDirectoryOnceCancelled() async {
        let fixed = trees()
        let root = root
        let task = Task.detached { () async throws -> PathIndex in
            var reads = 0
            return try await PathIndex.build(root: root) { tree in
                reads += 1
                if reads == 1 { withUnsafeCurrentTask { $0?.cancel() } }
                return fixed[tree] ?? []
            }
        }
        await #expect(throws: CancellationError.self) { try await task.value }
    }

    @Test func searchStopsOnceCancelled() async {
        let entries = (0..<(PathIndex.cancellationStride * 2)).map {
            PathIndexEntry(path: "dir/file\($0).txt", oid: oid(0x11), kind: .blob, hidden: false)
        }
        let index = PathIndex(root: root, entries: entries, truncated: false)
        let task = Task.detached { () throws -> [PathIndexEntry] in
            withUnsafeCurrentTask { $0?.cancel() }
            return try index.search("file")
        }
        await #expect(throws: CancellationError.self) { try await task.value }
    }

    @Test func aWorktreeListingBecomesAnIndexWithoutBlobIds() throws {
        let list = WorktreeFileList(
            files: [
                WorktreeFile(path: ".env", kind: .blob),
                WorktreeFile(path: "sub/.DS_Store", kind: .blob),
                WorktreeFile(path: "run.sh", kind: .executableBlob),
            ], scanned: 5, truncated: true)
        let index = PathIndex(root: root, worktreeFiles: list)
        #expect(index.root == root)
        #expect(index.truncated)
        #expect(index.entries.map(\.path) == [".env", "sub/.DS_Store", "run.sh"])
        #expect(index.entries.map(\.hidden) == [false, true, false])
        #expect(index.entries.allSatisfy { $0.oid.isUnhashed })
        try #require(index.entries.count == 3)
        #expect(index.entries[2].kind == .executableBlob)
    }

    @Test func searchMatchesDecomposedNamesByWholeCharacters() throws {
        let list = WorktreeFileList(
            files: ["cafe\u{301}.txt", "\u{e8}\u{a9}.txt"].map {
                WorktreeFile(path: $0, kind: .blob)
            },
            scanned: 2, truncated: false)
        let index = PathIndex(root: root, worktreeFiles: list)
        let found = try index.search("caf\u{e9}")
        #expect(found.count == 1, "a precomposed query finds the decomposed name")
        #expect(found.first?.path == "cafe\u{301}.txt", "the stored path stays as it was on disk")
        #expect(
            try index.search("\u{e9}").map(\.path) == ["cafe\u{301}.txt"],
            "the letter matches only the name that has it, never bytes of other letters")
    }

    @Test func buildReadsEachDirectoryOnce() async throws {
        var wide: [Oid: [TreeEntry]] = [:]
        wide[root] = (0..<50).map {
            TreeEntry(name: "dir\($0)", oid: oid(UInt8($0 + 4)), kind: .tree)
        }
        for child in wide[root]! {
            wide[child.oid] = (0..<100).map {
                TreeEntry(name: "f\($0)", oid: oid(0x77), kind: .blob)
            }
        }
        var reads = 0
        let index = try await PathIndex.build(root: root) { tree in
            reads += 1
            return try #require(wide[tree])
        }
        #expect(reads == 51, "one read per directory")
        #expect(index.entries.count == 5000)
    }
}
