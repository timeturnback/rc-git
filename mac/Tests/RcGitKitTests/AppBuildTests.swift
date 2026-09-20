import Foundation
import Testing

@testable import RcGitKit

@Suite struct AppBuildTests {
    private static func stamped(_ overrides: [String: Any] = [:]) -> [String: Any] {
        var info: [String: Any] = [
            "CFBundleName": "rc-git",
            "CFBundleShortVersionString": "0.1.0",
            "CFBundleVersion": "42",
            "RCGitVariant": "release",
            "RCGitSourceRoot": "/Users/dev/rc-git",
            "RCGitSourceCommit": "a3f91c2d4e5f60718293a4b5c6d7e8f901234567",
            "RCGitSourceDirty": "false",
            "RCGitBuildDate": "2026-09-19T11:02:03Z",
        ]
        for (key, value) in overrides { info[key] = value }
        return info
    }

    @Test func readsAReleaseStamp() {
        let build = AppBuild(info: Self.stamped())
        #expect(build.variant == .release)
        #expect(build.name == "rc-git")
        #expect(build.sourceRoot == "/Users/dev/rc-git")
        #expect(build.shortCommit == "a3f91c2")
        #expect(build.dirty == false)
        #expect(build.canSelfUpdate)
        #expect(build.versionLine == "0.1.0 (42) · a3f91c2")
        let expected = DateComponents(
            calendar: Calendar(identifier: .gregorian), timeZone: TimeZone(identifier: "UTC"),
            year: 2026, month: 9, day: 19, hour: 11, minute: 2, second: 3
        ).date
        #expect(build.buildDate == expected)
    }

    @Test func aDirtyBuildMarksItsCommit() {
        let build = AppBuild(info: Self.stamped(["RCGitSourceDirty": "true"]))
        #expect(build.dirty)
        #expect(build.versionLine == "0.1.0 (42) · a3f91c2+")
    }

    @Test func theDevBundleNeverSelfUpdates() {
        let build = AppBuild(
            info: Self.stamped(["RCGitVariant": "dev", "CFBundleName": "rc-git (dev)"]))
        #expect(build.variant == .dev)
        #expect(build.name == "rc-git (dev)")
        #expect(!build.canSelfUpdate)
    }

    /// A bare `swift run` has no stamp at all: it must degrade, not crash, and must not
    /// offer to replace the installed app.
    @Test func anUnstampedBundleDegradesToDev() {
        let build = AppBuild(info: [:])
        #expect(build.variant == .dev)
        #expect(build.sourceRoot == nil)
        #expect(build.commit == nil)
        #expect(build.shortCommit == nil)
        #expect(build.buildDate == nil)
        #expect(!build.canSelfUpdate)
        #expect(build.versionLine == "0 (0)")
    }

    /// `scripts/build-app.sh` stamps `unknown` when the checkout is not a git repository.
    @Test func anUnknownCommitIsNoCommit() {
        let build = AppBuild(info: Self.stamped(["RCGitSourceCommit": "unknown"]))
        #expect(build.commit == nil)
        #expect(build.versionLine == "0.1.0 (42)")
        #expect(build.canSelfUpdate)
    }
}
