import Foundation
import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct OpenTabsTests {
    @Test func clampsSelectionIntoTheList() {
        #expect(OpenTabs.clampedSelection(1, count: 3) == 1)
        #expect(OpenTabs.clampedSelection(0, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(2, count: 3) == 2)
        #expect(OpenTabs.clampedSelection(3, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(5, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(-1, count: 3) == 0)
        #expect(OpenTabs.clampedSelection(0, count: 0) == 0)
    }

    @Test func persistsThroughDefaults() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            #expect(tabs.paths.isEmpty)
            #expect(tabs.selectedIndex == 0)
            tabs.save(paths: ["/one", "/two", "/three"], selectedIndex: 2)
            #expect(tabs.paths == ["/one", "/two", "/three"])
            #expect(tabs.selectedIndex == 2)
            tabs.save(paths: [], selectedIndex: 2)
            #expect(tabs.paths.isEmpty)
            #expect(tabs.selectedIndex == 0)
        }
    }

    /// The handover to a relaunching build writes the same state as a record; what it adds is
    /// the flush, which cannot be observed from inside one process.
    @Test func handsOffTheSameStateItRecords() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            tabs.save(paths: ["/one", "/two", "/three"], selectedIndex: 2)
            tabs.handOff(live: ["/two", "/one"], selectedIndex: 1)
            #expect(tabs.paths == ["/two", "/one", "/three"])
            #expect(tabs.selectedIndex == 1)
        }
    }

    @Test func mergeKeepsTabsThatAreNoLongerOpen() {
        #expect(
            OpenTabs.merged(live: ["/two"], saved: ["/one", "/two", "/three"])
                == ["/two", "/one", "/three"])
        #expect(OpenTabs.merged(live: [], saved: ["/one", "/two"]) == ["/one", "/two"])
        #expect(OpenTabs.merged(live: ["/one"], saved: []) == ["/one"])
        #expect(OpenTabs.merged(live: ["/one", "/one"], saved: ["/one"]) == ["/one"])
    }

    /// A repository that failed to open survives the restore only while the folder that
    /// holds it is unreachable — a disk or share not mounted yet at launch.
    @Test func restoreKeepsFailedPathsWhoseParentIsUnreachable() {
        let paths = OpenTabs.restoredPaths(
            opened: ["/Users/me/open"],
            failed: ["/Volumes/Ext/repo", "/Users/me/gone", "/Volumes/Share/other"],
            directoryExists: { !$0.hasPrefix("/Volumes/") })
        #expect(paths == ["/Users/me/open", "/Volumes/Ext/repo", "/Volumes/Share/other"])
    }

    /// Nothing opening is not a reason to forget the list: every unmounted repository waits
    /// for the next launch, and only the ones gone from a reachable folder are dropped.
    @Test func restoreWithNothingOpenedKeepsUnreachablePaths() {
        let paths = OpenTabs.restoredPaths(
            opened: [], failed: ["/Volumes/Ext/a", "/Users/me/gone"],
            directoryExists: { !$0.hasPrefix("/Volumes/") })
        #expect(paths == ["/Volumes/Ext/a"])
        #expect(
            OpenTabs.restoredPaths(opened: [], failed: ["/a"], directoryExists: { _ in true })
                .isEmpty)
        #expect(OpenTabs.restoredPaths(opened: [], failed: []).isEmpty)
    }

    /// A volume root's parent is `/Volumes`, which exists whether or not the disk is mounted,
    /// so the root itself decides: missing means unmounted, and the path is kept.
    @Test func restoreKeepsAnUnmountedVolumeRoot() {
        let mounted: Set<String> = ["/", "/Volumes/", "/Volumes/Here/", "/Users/me/"]
        let paths = OpenTabs.restoredPaths(
            opened: [],
            failed: ["/Volumes/Ext", "/Volumes/Here", "/Volumes/Gone/", "/Users/me/gone"],
            directoryExists: { mounted.contains($0.hasSuffix("/") ? $0 : $0 + "/") })
        #expect(paths == ["/Volumes/Ext", "/Volumes/Gone/"])
    }

    @Test func mountFolderIsTheVolumeRootOrTheContainingFolder() {
        #expect(OpenTabs.mountFolder(of: "/Volumes/Ext") == "/Volumes/Ext")
        #expect(OpenTabs.mountFolder(of: "/Volumes/Ext/") == "/Volumes/Ext/")
        #expect(OpenTabs.mountFolder(of: "/Volumes/Ext/repo") == "/Volumes/Ext/")
        #expect(OpenTabs.mountFolder(of: "/Users/me/repo") == "/Users/me/")
    }

    @Test func directoryExistsNeedsAFolder() throws {
        let folder = URL.temporaryDirectory.appending(path: "rc-git-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer {
            do {
                try FileManager.default.removeItem(at: folder)
            } catch {
                Issue.record("could not remove scratch folder \(folder.path): \(error)")
            }
        }
        let file = folder.appending(path: "file")
        try Data().write(to: file)

        #expect(OpenTabs.directoryExists(folder.path(percentEncoded: false)))
        let missing = folder.appending(path: "nope").path(percentEncoded: false)
        #expect(!OpenTabs.directoryExists(missing))
        #expect(!OpenTabs.directoryExists(file.path(percentEncoded: false)))
    }

    /// Without `dropping` — ⇧⌘W and ⌘Q — what is recorded is the live order plus everything
    /// that was saved before it.
    @Test func recordWithoutDroppingKeepsClosedTabs() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            tabs.record(live: ["/one", "/two"], selectedIndex: 1)
            #expect(tabs.paths == ["/one", "/two"])

            tabs.record(live: ["/one"], selectedIndex: 0)  // closed /two
            #expect(tabs.paths == ["/one", "/two"])
            #expect(tabs.selectedIndex == 0)

            tabs.record(live: [], selectedIndex: 0)  // closed the window, then quit
            #expect(tabs.paths == ["/one", "/two"])

            tabs.record(live: ["/three"], selectedIndex: 0)
            #expect(tabs.paths == ["/three", "/one", "/two"])
        }
    }

    /// ⌘W on its own drops that tab's repository; the rest of the saved list stays.
    @Test func recordDropsTheTabClosedOnItsOwn() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            tabs.save(paths: ["/one", "/two", "/three"], selectedIndex: 0)
            tabs.record(live: ["/one"], selectedIndex: 0, dropping: "/two")
            #expect(tabs.paths == ["/one", "/three"])

            tabs.record(live: [], selectedIndex: 0, dropping: "/one")
            #expect(tabs.paths == ["/three"])

            tabs.record(live: ["/three"], selectedIndex: 0, dropping: "/gone")
            #expect(tabs.paths == ["/three"])
        }
    }

    /// The restore at launch replaces the list, so a repository that
    /// no longer opens is not retried on every launch.
    @Test func saveReplacesTheListSoRestoreCanPrune() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            tabs.record(live: ["/one", "/gone", "/two"], selectedIndex: 2)
            let opened = ["/one", "/two"]
            let selected = OpenTabs.restoredSelection(
                saved: tabs.paths, selectedIndex: tabs.selectedIndex, opened: opened)
            tabs.save(paths: opened, selectedIndex: selected)
            #expect(tabs.paths == ["/one", "/two"])
            #expect(tabs.selectedIndex == 1)
        }
    }

    @Test func restoredSelectionFollowsTheSelectedPath() {
        let saved = ["/a", "/b", "/c"]
        #expect(
            OpenTabs.restoredSelection(saved: saved, selectedIndex: 2, opened: ["/b", "/c"]) == 1)
        #expect(
            OpenTabs.restoredSelection(saved: saved, selectedIndex: 1, opened: ["/a", "/b"]) == 1)
        #expect(
            OpenTabs.restoredSelection(
                saved: ["/a", "/gone", "/b", "/c"], selectedIndex: 2, opened: ["/a", "/b", "/c"])
                == 1)
        #expect(OpenTabs.restoredSelection(saved: saved, selectedIndex: 2, opened: saved) == 2)
    }

    @Test func restoredSelectionFallsBackToFirstWhenTheSelectedPathFailed() {
        let saved = ["/a", "/b", "/c"]
        #expect(
            OpenTabs.restoredSelection(saved: saved, selectedIndex: 0, opened: ["/b", "/c"]) == 0)
        #expect(
            OpenTabs.restoredSelection(saved: saved, selectedIndex: 2, opened: ["/a", "/b"]) == 0)
        #expect(OpenTabs.restoredSelection(saved: [], selectedIndex: 0, opened: []) == 0)
        #expect(OpenTabs.restoredSelection(saved: saved, selectedIndex: 9, opened: ["/a"]) == 0)
    }

    @Test func restoredSelectionKeepsTheLaunchDocument() {
        let saved = ["/a", "/b", "/c"]
        #expect(
            OpenTabs.restoredSelection(
                saved: saved, selectedIndex: 0, opened: saved, launchDocument: "/c") == 2)
        #expect(
            OpenTabs.restoredSelection(
                saved: saved, selectedIndex: 1, opened: ["/a", "/b"], launchDocument: "/gone")
                == 1)
        #expect(
            OpenTabs.restoredSelection(
                saved: [], selectedIndex: 0, opened: ["/launch"], launchDocument: "/launch") == 0)
    }

    /// Another build under the same bundle id may have written these keys with another type;
    /// that reads as no tabs, never as a crash.
    @Test func aWronglyTypedStoredListReadsAsEmpty() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            defaults.set("oops", forKey: OpenTabs.pathsKey)
            defaults.set("x", forKey: OpenTabs.selectedKey)
            #expect(tabs.paths.isEmpty)
            #expect(tabs.selectedIndex == 0)

            defaults.set(["/a", 3] as [Any], forKey: OpenTabs.pathsKey)
            #expect(tabs.paths.isEmpty)

            defaults.set(["/a", "/b"], forKey: OpenTabs.pathsKey)
            #expect(tabs.selectedIndex == 0)
        }
    }

    @Test func savedSelectionOutsideTheListFallsBackToFirst() throws {
        try withScratchDefaults { defaults in
            let tabs = OpenTabs(defaults: defaults)
            tabs.save(paths: ["/one", "/two"], selectedIndex: 9)
            #expect(tabs.selectedIndex == 0)
        }
    }
}
