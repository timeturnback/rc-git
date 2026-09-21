import Foundation
import Testing

@testable import RcGitKit

/// A scripted environment: it records what the model asked for and answers from the
/// script, so the whole update runs without a toolchain, a network, or a build. The model
/// calls one step at a time, so the mutable state needs no lock.
private final class StubUpdateEnvironment: UpdateEnvironment, @unchecked Sendable {
    struct Script {
        var toolchain: ToolchainCheck = .ready
        /// One entry per `sourceState` call: preflight reads it, then the sync step does.
        var sourceStates: [SourceStateResult] = []
        var syncFailure: String?
        var installFailure: String?
        var installOutput: [String] = []
        var relaunchFailure: String?
    }

    private var script: Script
    private(set) var calls: [String] = []

    init(_ script: Script) {
        self.script = script
    }

    func toolchain() async -> ToolchainCheck {
        calls.append("toolchain")
        return script.toolchain
    }

    /// Answers each call with the next scripted state, repeating the last one.
    func sourceState() async -> SourceStateResult {
        calls.append("sourceState")
        guard !script.sourceStates.isEmpty else { return .failure("no source state scripted") }
        return script.sourceStates.count > 1
            ? script.sourceStates.removeFirst() : script.sourceStates[0]
    }

    func sync() async -> String? {
        calls.append("sync")
        return script.syncFailure
    }

    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? {
        calls.append("install")
        for line in script.installOutput { onOutput(line) }
        return script.installFailure
    }

    func relaunch() async -> String? {
        calls.append("relaunch")
        return script.relaunchFailure
    }
}

@MainActor
@Suite struct UpdateModelTests {
    private static let head = "a3f91c2d4e5f60718293a4b5c6d7e8f901234567"
    private static let upstream = "b7d21ff0112233445566778899aabbccddeeff00"

    private static func build(
        variant: AppVariant = .release, sourceRoot: String? = "/checkout",
        commit: String? = head
    ) -> AppBuild {
        AppBuild(
            variant: variant, name: "rc-git", version: "0.1.0", build: "42",
            sourceRoot: sourceRoot, commit: commit, dirty: false, buildDate: nil)
    }

    private static func model(
        _ script: StubUpdateEnvironment.Script, build: AppBuild = build()
    ) -> (UpdateModel, StubUpdateEnvironment) {
        let environment = StubUpdateEnvironment(script)
        return (UpdateModel(build: build, environment: environment), environment)
    }

    @Test func aFullUpdateRunsEveryStepInOrder() async {
        let (model, environment) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                    .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
                ],
                installOutput: ["==> building", "built RCGit.app"]
            ))
        await model.start()
        #expect(model.state == .installed)
        #expect(
            environment.calls == [
                "toolchain", "sourceState", "sync", "sourceState", "install", "relaunch",
            ])
        #expect(model.log.contains("on master"))
        #expect(model.log.contains("built RCGit.app"))
    }

    /// The commit did not move, so the minutes-long rebuild is skipped.
    @Test func anUnchangedCheckoutStopsBeforeTheBuild() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]))
        await model.start()
        #expect(model.state == .upToDate)
        #expect(!environment.calls.contains("install"))
    }

    @Test func forceRebuildsAnUnchangedCheckout() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]))
        await model.start(force: true)
        #expect(model.state == .installed)
        #expect(environment.calls.contains("install"))
    }

    @Test func aMissingToolFailsPreflightWithoutTouchingTheCheckout() async {
        let (model, environment) = Self.model(.init(toolchain: .missing(["cargo", "cbindgen"])))
        await model.start()
        guard case let .failed(step, message) = model.state else {
            Issue.record("expected a preflight failure, got \(model.state)")
            return
        }
        #expect(step == .preflight)
        #expect(message.contains("cargo, cbindgen"))
        #expect(environment.calls == ["toolchain"])
    }

    /// A probe that cannot answer is a failure with the reason, not a window that spins:
    /// the checkout is never read, and the message is what the environment said.
    @Test func anUnanswerableToolchainCheckFailsPreflight() async {
        let (model, environment) = Self.model(
            .init(toolchain: .failure("the toolchain check did not answer in 20s and was killed")))
        await model.start()
        #expect(
            model.state
                == .failed(
                    step: .preflight,
                    message: "the toolchain check did not answer in 20s and was killed"))
        #expect(environment.calls == ["toolchain"])
    }

    /// Each half of preflight is logged before it runs, so a stall in either one says which.
    @Test func preflightLogsBothHalvesBeforeRunningThem() async {
        let (model, _) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]))
        await model.start(force: true)
        #expect(model.log.contains("checking the toolchain"))
        #expect(model.log.contains("reading the checkout"))
        let toolchain = model.log.firstIndex(of: "checking the toolchain")
        let checkout = model.log.firstIndex(of: "reading the checkout")
        #expect(toolchain ?? 0 < checkout ?? 0)
    }

    @Test func uncommittedWorkStopsTheUpdate() async {
        let (model, environment) = Self.model(
            .init(
                sourceStates: [
                    .state(
                        SourceState(
                            commit: Self.head, branch: "master",
                            dirtyPaths: ["a.rs", "b.rs", "c.rs", "d.rs"]))
                ]))
        await model.start()
        guard case let .failed(step, message) = model.state else {
            Issue.record("expected a preflight failure, got \(model.state)")
            return
        }
        #expect(step == .preflight)
        #expect(message.contains("a.rs, b.rs, c.rs and 1 more"))
        #expect(!environment.calls.contains("sync"))
    }

    @Test func aFailedFetchStopsBeforeTheBuild() async {
        let (model, environment) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
                ],
                syncFailure: "host not trusted"
            ))
        await model.start()
        #expect(model.state == .failed(step: .sync, message: "host not trusted"))
        #expect(!environment.calls.contains("install"))
    }

    @Test func aFailedBuildNeverRelaunches() async {
        let (model, environment) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                    .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
                ],
                installFailure: "scripts/install-app.sh failed (status 1): linker error",
                installOutput: ["==> building", "error: linker error"]
            ))
        await model.start()
        guard case let .failed(step, message) = model.state else {
            Issue.record("expected an install failure, got \(model.state)")
            return
        }
        #expect(step == .install)
        #expect(message.contains("linker error"))
        #expect(!environment.calls.contains("relaunch"))
        #expect(model.log.contains("error: linker error"))
    }

    /// The bundle is in place but the new instance did not come up: that is still a
    /// failure the window has to show, not a silent success.
    @Test func aFailedRelaunchReportsTheInstallStep() async {
        let (model, _) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                    .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
                ],
                relaunchFailure: "could not launch /Applications/RCGit.app"
            ))
        await model.start()
        guard case let .failed(step, message) = model.state else {
            Issue.record("expected an install failure, got \(model.state)")
            return
        }
        #expect(step == .install)
        #expect(message.contains("/Applications/RCGit.app"))
    }

    @Test func anUnreadableCheckoutFailsPreflight() async {
        let (model, _) = Self.model(.init(sourceStates: [.failure("cannot open /checkout")]))
        await model.start()
        #expect(model.state == .failed(step: .preflight, message: "cannot open /checkout"))
    }

    /// The dev loop rebuilds the working tree as it stands: no fetch, and uncommitted work
    /// goes into the build instead of stopping it. The checkout is at the commit this
    /// bundle was built from, which for a release update would mean "already up to date" —
    /// a rebuild has no such shortcut, because the tree moves without the commit.
    @Test func theDevBundleRebuildsTheWorkingTreeWithoutFetching() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: ["a.rs"]))
            ]),
            build: Self.build(variant: .dev))
        await model.start()
        #expect(model.state == .installed)
        #expect(environment.calls == ["toolchain", "sourceState", "install", "relaunch"])
        #expect(model.log.contains("uncommitted work goes in: a.rs"))
    }

    @Test func aDevBundleWithNoStampNamesTheDevScript() async {
        let (model, environment) = Self.model(
            .init(), build: Self.build(variant: .dev, sourceRoot: nil))
        await model.start()
        guard case .failed(.preflight, let message) = model.state else {
            Issue.record("expected a preflight failure, got \(model.state)")
            return
        }
        #expect(message.contains("scripts/build-app.sh --dev"))
        #expect(environment.calls.isEmpty)
    }

    @Test func anUnstampedBundleRefusesBeforeAnyWork() async {
        let (model, environment) = Self.model(
            .init(), build: Self.build(variant: .release, sourceRoot: nil))
        await model.start()
        guard case .failed(.preflight, let message) = model.state else {
            Issue.record("expected a preflight failure, got \(model.state)")
            return
        }
        #expect(message.contains("scripts/install-app.sh"))
        #expect(environment.calls.isEmpty)
    }

    @Test func resetReturnsAFailureToIdle() async {
        let (model, _) = Self.model(.init(toolchain: .missing(["cargo"])))
        await model.start()
        model.reset()
        #expect(model.state == .idle)
    }

    @Test func theLogStaysBounded() async {
        let (model, _) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                    .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
                ],
                installOutput: (0..<(UpdateModel.maxLogLines + 500)).map { "line \($0)" }
            ))
        await model.start()
        #expect(model.log.count == UpdateModel.maxLogLines)
        #expect(model.log.last == "line \(UpdateModel.maxLogLines + 499)")
    }

    @Test func listNamesUpToThreePaths() {
        #expect(UpdateModel.list(["a", "b"]) == "a, b")
        #expect(UpdateModel.list(["a", "b", "c"]) == "a, b, c")
        #expect(UpdateModel.list(["a", "b", "c", "d"]) == "a, b, c and 1 more")
    }
}
