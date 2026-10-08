import Foundation
import Testing

@testable import RcGitKit

@Suite struct FuzzyMatchTests {
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

    @Test func fileNameMatchesBeatDirectoryMatches() {
        let inName = FuzzyMatch.score("docs/guide.md", query: "guide")
        let inDirectory = FuzzyMatch.score("guide/notes.md", query: "guide")
        #expect(inName != nil && inDirectory != nil)
        #expect(inName! > inDirectory!)
    }

    @Test func consecutiveRunsBeatScatteredMatches() {
        let run = FuzzyMatch.score("browser.swift", query: "brow")
        let scattered = FuzzyMatch.score("b_r_o_w_x.swift", query: "brow")
        #expect(run! > scattered!)
    }
}

@Suite struct PathIndexTests {
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

    @Test func buildFlattensEveryFileAndSkipsSubmodules() throws {
        let fixed = trees()
        let index = try PathIndex.build(root: root) { fixed[$0]! }
        let paths = index.entries.map(\.path).sorted()
        #expect(paths == ["README.md", "src/main.rs", "src/nested/link", "src/run.sh"])
        #expect(!index.truncated)
        #expect(index.entries.first { $0.path == "src/main.rs" }?.name == "main.rs")
    }

    @Test func inexactNameMarksItsPathAndEveryPathBelowIt() throws {
        var fixed = trees()
        fixed[root]?[0] = TreeEntry(name: "s\u{FFFD}c", oid: sub, kind: .tree, hasExactName: false)
        fixed[root]?.append(
            TreeEntry(name: "f\u{FFFD}o", oid: oid(0x14), kind: .blob, hasExactName: false))
        let index = try PathIndex.build(root: root) { fixed[$0]! }
        let inexact = index.entries.filter { !$0.hasExactPath }.map(\.path).sorted()
        #expect(
            inexact == [
                "f\u{FFFD}o", "s\u{FFFD}c/main.rs", "s\u{FFFD}c/nested/link", "s\u{FFFD}c/run.sh",
            ])
        #expect(index.entries.first { $0.path == "README.md" }?.hasExactPath == true)
    }

    @Test func searchRanksAndLimits() throws {
        let fixed = trees()
        let index = try PathIndex.build(root: root) { fixed[$0]! }
        let matches = index.search("mainrs")
        #expect(matches.first?.path == "src/main.rs")
        #expect(index.search("zzz").isEmpty)
        #expect(index.search("").count == index.entries.count)
        #expect(index.search("", limit: 2).count == 2)
    }

    @Test func hiddenFilesAreMarkedAndLeftOutOnRequest() throws {
        var fixed = trees()
        let meta = oid(0x70)
        fixed[root]! += [
            TreeEntry(name: ".DS_Store", oid: oid(0x71), kind: .blob),
            TreeEntry(name: ".git", oid: meta, kind: .tree),
            TreeEntry(name: ".env", oid: oid(0x72), kind: .blob),
        ]
        fixed[meta] = [TreeEntry(name: "HEAD", oid: oid(0x73), kind: .blob)]
        let index = try PathIndex.build(root: root) { fixed[$0]! }
        let hidden = index.entries.filter(\.hidden).map(\.path).sorted()
        #expect(hidden == [".DS_Store", ".git/HEAD"], "a hidden folder hides what is inside it")

        let all = index.search("", limit: 100).map(\.path)
        #expect(all.contains(".DS_Store"))
        let visible = index.search("", limit: 100, includeHidden: false).map(\.path)
        #expect(
            visible.sorted() == [
                ".env", "README.md", "src/main.rs", "src/nested/link", "src/run.sh",
            ])
        #expect(index.search("head", includeHidden: false).isEmpty)
        #expect(index.search("head").map(\.path) == [".git/HEAD"])
    }

    @Test func aWorktreeListingBecomesAnIndexWithoutBlobIds() {
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
        #expect(index.entries.allSatisfy { $0.oid.bytes.isEmpty })
        #expect(index.entries[2].kind == .executableBlob)
    }

    @Test func buildReadsEachDirectoryOnce() throws {
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
        let index = try PathIndex.build(root: root) { tree in
            reads += 1
            return wide[tree]!
        }
        #expect(reads == 51, "one read per directory")
        #expect(index.entries.count == 5000)
    }
}
