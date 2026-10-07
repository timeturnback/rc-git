import Foundation
import Testing

/// Runs `body` against an empty `UserDefaults` domain of its own, then deletes it.
///
/// Each call gets its own domain because suites run in parallel and the stores under test use
/// constant keys. The suite is named by an absolute path inside a temporary folder: CFPreferences
/// then writes `<path>.plist` there instead of into `~/Library/Preferences`, which UserDefaults
/// does not document but a probe measured (no new entry in `~/Library/Preferences`). That plist
/// outlives `removePersistentDomain`, so the folder is removed too.
func withScratchDefaults(_ body: (UserDefaults) throws -> Void) throws {
    let folder = URL.temporaryDirectory.appending(path: "rc-git-tests-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer {
        do {
            try FileManager.default.removeItem(at: folder)
        } catch {
            Issue.record("could not remove scratch defaults folder \(folder.path): \(error)")
        }
    }
    let path = folder.appending(path: "defaults").path(percentEncoded: false)
    let defaults = try #require(UserDefaults(suiteName: path))
    defer { defaults.removePersistentDomain(forName: path) }
    try body(defaults)
}
