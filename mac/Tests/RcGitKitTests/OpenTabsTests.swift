import Foundation
import Testing

@testable import RcGitKit

@Suite struct OpenTabsTests {
    @Test func clampsSelectionIntoTheList() {
        #expect(OpenTabs.clampedSelection(1, count: 3) == 1)
        #expect(OpenTabs.clampedSelection(0, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(5, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(-1, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(0, count: 0) == 0)
    }

    @Test func persistsThroughDefaults() {
        let suite = "rc-git.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let tabs = OpenTabs(defaults: defaults)
        #expect(tabs.paths.isEmpty)
        #expect(tabs.selectedIndex == 0)
        tabs.save(paths: ["/one", "/two", "/three"], selectedIndex: 2)
        #expect(tabs.paths == ["/one", "/two", "/three"])
        #expect(tabs.selectedIndex == 2)
        tabs.clear()
        #expect(tabs.paths.isEmpty)
        #expect(tabs.selectedIndex == 0)
    }

    /// The handover to a relaunching build writes the same state as a save; what it adds is
    /// the flush, which cannot be observed from inside one process.
    @Test func handsOffTheSameStateItSaves() {
        let suite = "rc-git.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let tabs = OpenTabs(defaults: defaults)
        tabs.handOff(paths: ["/one", "/two"], selectedIndex: 1)
        #expect(tabs.paths == ["/one", "/two"])
        #expect(tabs.selectedIndex == 1)
    }

    @Test func savedSelectionOutsideTheListFallsBackToFirst() {
        let suite = "rc-git.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let tabs = OpenTabs(defaults: defaults)
        tabs.save(paths: ["/one", "/two"], selectedIndex: 9)
        #expect(tabs.selectedIndex == 0)
    }
}
