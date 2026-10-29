import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct MenuShortcutsTests {
    /// Every row of the table, read by reflection so a row added later cannot be left out.
    private static let rows: [MenuShortcut] = Mirror(reflecting: MenuShortcuts()).children
        .compactMap { $0.value as? MenuShortcut }

    /// AppKit's standard items and the chords macOS users expect on them.
    private static let standard: [(title: String, chord: MenuChord)] = [
        ("Hide", MenuChord(key: "h", modifiers: .command)),
        ("Hide Others", MenuChord(key: "h", modifiers: [.command, .option])),
        ("Quit", MenuChord(key: "q", modifiers: .command)),
        ("Minimize", MenuChord(key: "m", modifiers: .command)),
        ("Enter Full Screen", MenuChord(key: "f", modifiers: [.command, .control])),
    ]

    @Test func reflectionSeesEveryRow() {
        #expect(Self.rows.count == 44)
    }

    @Test func noTwoItemsShareAChord() {
        var owners: [MenuChord: String] = [:]
        for row in Self.rows {
            if let other = owners[row.chord] {
                Issue.record("\(row.title) and \(other) share \(row.chord)")
            }
            owners[row.chord] = row.title
        }
    }

    @Test func standardChordsBelongToTheStandardItems() {
        for (title, chord) in Self.standard {
            let owners = Self.rows.filter { $0.chord == chord }.map(\.title)
            #expect(owners == [title], "\(chord)")
        }
    }

    @Test func fileHistoryLeavesHideOthersAlone() {
        let keys = MenuShortcuts()
        #expect(keys.fileHistory.chord == MenuChord(key: "y", modifiers: [.command, .option]))
        #expect(keys.fileHistory.chord != keys.hideOthers.chord)
    }

    @Test func searchTitleFollowsTheMode() {
        let keys = MenuShortcuts()
        #expect(keys.searchTitle(for: .files) == "Find in Repository…")
        #expect(keys.searchTitle(for: .history) == "Search Commits")
        #expect(keys.searchTitle(for: .changes) == "Search Commits")
    }

    @Test func uppercaseLetterIsTheLowercaseLetterWithShift() {
        #expect(
            MenuChord(key: "G", modifiers: .command)
                == MenuChord(key: "g", modifiers: [.command, .shift]))
        #expect(
            MenuChord(key: "F", modifiers: [.command, .shift])
                == MenuChord(key: "f", modifiers: [.command, .shift]))
        #expect(
            MenuChord(key: "g", modifiers: .command) != MenuChord(key: "G", modifiers: .command))
        #expect(MenuChord(key: "[", modifiers: .command).modifiers == .command)
    }
}
