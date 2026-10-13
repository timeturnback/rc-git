import Foundation
import Testing

@testable import RcGitKit

@Suite struct ChangeTreeTests {
    private func entry(_ path: String) -> StatusEntry {
        StatusEntry(path: path, kind: .blob, status: .modified, oid: nil, size: 1)
    }

    @Test func flatKeepsOneRowPerEntryLabeledWithTheWholePath() {
        let entries = [entry("a.txt"), entry("mac/Sources/App.swift")]
        let rows = ChangeTree.nodes(entries, grouping: .flat)
        #expect(rows.map(\.name) == ["a.txt", "mac/Sources/App.swift"])
        #expect(rows.allSatisfy { !$0.isFolder && $0.children.isEmpty })
        #expect(rows.map(\.path) == entries.map(\.path))
    }

    @Test func foldersNestFilesAndListFoldersFirst() {
        let entries = [
            entry("README.md"), entry("mac/App.swift"), entry("mac/Kit.swift"),
            entry("mac/tests/KitTests.swift"),
        ]
        let rows = ChangeTree.nodes(entries, grouping: .folders)
        #expect(rows.map(\.name) == ["mac", "README.md"])
        let mac = rows.first
        #expect(mac?.isFolder == true)
        #expect(mac?.path == "mac")
        #expect(mac?.children.map(\.name) == ["tests", "App.swift", "Kit.swift"])
        #expect(mac?.children.first?.children.map(\.path) == ["mac/tests/KitTests.swift"])
    }

    @Test func aSoleSubfolderMergesIntoOneRow() {
        let rows = ChangeTree.nodes(
            [entry("docs/solutions/ui-bugs/one.md"), entry("docs/solutions/ui-bugs/two.md")],
            grouping: .folders)
        #expect(rows.map(\.name) == ["docs/solutions/ui-bugs"])
        #expect(rows.first?.path == "docs/solutions/ui-bugs")
        #expect(rows.first?.children.map(\.name) == ["one.md", "two.md"])
    }

    @Test func aFolderWithItsOwnFilesDoesNotMerge() {
        let rows = ChangeTree.nodes(
            [entry("docs/index.md"), entry("docs/solutions/one.md")], grouping: .folders)
        #expect(rows.map(\.name) == ["docs"])
        #expect(rows.first?.children.map(\.name) == ["solutions", "index.md"])
    }

    @Test func filePathsCoverEveryFileBeneathARow() {
        let entries = [entry("mac/App.swift"), entry("mac/tests/KitTests.swift"), entry("a.txt")]
        let rows = ChangeTree.nodes(entries, grouping: .folders)
        #expect(rows.first?.filePaths == ["mac/tests/KitTests.swift", "mac/App.swift"])
        #expect(rows.last?.filePaths == ["a.txt"])
    }

    @Test func aFileAndAFolderWithOneNameKeepDistinctKeys() throws {
        let rows = ChangeTree.nodes([entry("a"), entry("a/b")], grouping: .folders)
        try #require(rows.count == 2)
        let folder = try #require(rows.first { $0.isFolder })
        let file = try #require(rows.first { !$0.isFolder })
        #expect(folder.path == "a")
        #expect(file.path == "a")
        #expect(folder.key != file.key)
        #expect(folder.filePaths == ["a/b"])
        #expect(file.filePaths == ["a"])
    }

    @Test func emptyReportHasNoRows() {
        #expect(ChangeTree.nodes([StatusEntry](), grouping: .flat).isEmpty)
        #expect(ChangeTree.nodes([StatusEntry](), grouping: .folders).isEmpty)
    }

    @Test func treeChangesGroupByTheirPath() {
        let change = TreeChange(
            kind: .renamed, path: "mac/New.swift", sourcePath: "mac/Old.swift", old: nil,
            new: nil, entry: .blob)
        let rows = ChangeTree.nodes([change], grouping: .folders)
        #expect(rows.map(\.name) == ["mac"])
        #expect(rows.first?.children.first?.entry == change)
    }

    @Test func groupingPersistsThroughDefaults() throws {
        try withScratchDefaults { defaults in
            let store = ChangeGroupingStore.worktree(defaults: defaults)
            #expect(store.grouping == .flat)
            store.grouping = .folders
            #expect(ChangeGroupingStore.worktree(defaults: defaults).grouping == .folders)
        }
    }

    @Test func eachListKeepsItsOwnGroupingAndDefault() throws {
        try withScratchDefaults { defaults in
            let commit = ChangeGroupingStore.commit(defaults: defaults)
            #expect(commit.grouping == .folders)
            commit.grouping = .flat
            #expect(ChangeGroupingStore.worktree(defaults: defaults).grouping == .flat)
            ChangeGroupingStore.worktree(defaults: defaults).grouping = .folders
            #expect(ChangeGroupingStore.commit(defaults: defaults).grouping == .flat)
        }
    }

    @Test func anUnknownStoredGroupingFallsBackToTheListDefault() throws {
        try withScratchDefaults { defaults in
            defaults.set("sideways", forKey: ChangeGroupingStore.commitKey)
            #expect(ChangeGroupingStore.commit(defaults: defaults).grouping == .folders)
        }
    }
}
