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
        let mac = try? #require(rows.first)
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

    @Test func emptyReportHasNoRows() {
        #expect(ChangeTree.nodes([], grouping: .flat).isEmpty)
        #expect(ChangeTree.nodes([], grouping: .folders).isEmpty)
    }

    @Test func groupingPersistsThroughDefaults() {
        let suite = "rc-git.tests.\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suite) else {
            Issue.record("no test defaults suite")
            return
        }
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = ChangeGroupingStore(defaults: defaults)
        #expect(store.grouping == .flat)
        store.grouping = .folders
        #expect(ChangeGroupingStore(defaults: defaults).grouping == .folders)
    }
}
