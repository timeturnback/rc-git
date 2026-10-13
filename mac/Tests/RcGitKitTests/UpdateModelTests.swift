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

    func handOff() async {
        calls.append("handOff")
    }

    func reclaim() async {
        calls.append("reclaim")
    }

    func relaunch() async -> String? {
        calls.append("relaunch")
        return script.relaunchFailure
    }
}

/// Holds the toolchain probe until the test releases it, so a test can act while the model
/// sits in `.running(.preflight)`. Every other step fails fast; only the probe count matters.
private final class GatedToolchainEnvironment: UpdateEnvironment, @unchecked Sendable {
    let entered = Signal()
    let release = Signal()
    private let lock = NSLock()
    private var calls = 0

    var toolchainCalls: Int {
        lock.lock()
        defer { lock.unlock() }
        return calls
    }

    func toolchain() async -> ToolchainCheck {
        lock.withLock { calls += 1 }
        entered.signal()
        await release.wait()
        return .failure("released")
    }

    func sourceState() async -> SourceStateResult { .failure("unused") }
    func sync() async -> String? { "unused" }
    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? { "unused" }
    func handOff() async {}
    func reclaim() async {}
    func relaunch() async -> String? { "unused" }
}

/// A fetch or a build that never returns on its own: the first call to the `hanging` step
/// suspends until its Task is cancelled, like a build waiting on another build's lock. Every
/// later call succeeds at once, so a second attempt runs clean. The model calls one step at
/// a time, so `calls` needs no lock; `entered` fires once the hang has begun.
private final class HangingStepEnvironment: UpdateEnvironment, @unchecked Sendable {
    let hanging: UpdateStep
    let entered = Signal()
    private(set) var calls: [String] = []
    /// The first build's output callback, kept so a test can print a line late.
    private(set) var firstOutput: (@Sendable (String) -> Void)?

    init(hanging: UpdateStep) {
        self.hanging = hanging
    }

    func toolchain() async -> ToolchainCheck {
        calls.append("toolchain")
        return .ready
    }

    func sourceState() async -> SourceStateResult {
        calls.append("sourceState")
        let commit = calls.filter { $0 == "sourceState" }.count == 1 ? "a1" : "b2"
        return .state(SourceState(commit: commit, branch: "master", dirtyPaths: []))
    }

    func sync() async -> String? {
        calls.append("sync")
        return await hangOnce(.sync) ? "the pull was cancelled" : nil
    }

    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? {
        calls.append("install")
        if firstOutput == nil { firstOutput = onOutput }
        // Reports success even when cancelled, like a build that finished at the moment of
        // the cancel: the model alone must keep it from relaunching.
        _ = await hangOnce(.install)
        return nil
    }

    func handOff() async {}
    func reclaim() async {}

    func relaunch() async -> String? {
        calls.append("relaunch")
        return nil
    }

    /// Suspends until cancelled when `step` is the hanging one and this is its first call.
    private func hangOnce(_ step: UpdateStep) async -> Bool {
        guard step == hanging, calls.filter({ $0 == Self.name(step) }).count == 1 else {
            return false
        }
        let cancelled = Signal()
        await withTaskCancellationHandler {
            entered.signal()
            await cancelled.wait()
        } onCancel: {
            cancelled.signal()
        }
        return true
    }

    private static func name(_ step: UpdateStep) -> String {
        step == .sync ? "sync" : "install"
    }
}

@MainActor
@Suite(.timeLimit(.minutes(1))) struct UpdateModelTests {
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
                "toolchain", "sourceState", "sync", "sourceState", "install", "handOff",
                "relaunch",
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

    /// The old process keeps running after a launch that failed, so the state it handed
    /// over is taken back and recorded again.
    @Test func aFailedRelaunchReclaimsTheHandedOffState() async {
        let (model, environment) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                    .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
                ],
                relaunchFailure: "could not launch /Applications/RCGit.app"
            ))
        await model.start()
        #expect(environment.calls.suffix(3) == ["handOff", "relaunch", "reclaim"])
    }

    /// A build that never relaunches never hands its state over either.
    @Test func aFailedBuildKeepsTheState() async {
        let (model, environment) = Self.model(
            .init(
                sourceStates: [
                    .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                    .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
                ],
                installFailure: "linker error"
            ))
        await model.start()
        #expect(!environment.calls.contains("handOff"))
        #expect(!environment.calls.contains("reclaim"))
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
        #expect(
            environment.calls == ["toolchain", "sourceState", "install", "handOff", "relaunch"])
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

    // MARK: proceed()

    @Test func proceedFromIdleStartsTheUpdate() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
            ]))
        #expect(model.canProceed)
        await model.proceed()
        #expect(model.state == .installed)
        #expect(environment.calls.first == "toolchain")
    }

    /// From "already up to date" the button reads "Rebuild Anyway": it forces the build.
    @Test func proceedFromUpToDateRebuildsAnyway() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]))
        await model.start()
        #expect(model.state == .upToDate)
        #expect(model.canProceed)
        await model.proceed()
        #expect(model.state == .installed)
        #expect(environment.calls.filter { $0 == "install" }.count == 1)
    }

    @Test func proceedFromAFailureStartsOver() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [.failure("cannot open /checkout")]))
        await model.start()
        #expect(model.state == .failed(step: .preflight, message: "cannot open /checkout"))
        #expect(model.canProceed)
        await model.proceed()
        #expect(environment.calls == ["toolchain", "sourceState", "toolchain", "sourceState"])
        #expect(model.state == .failed(step: .preflight, message: "cannot open /checkout"))
        #expect(model.log.filter { $0 == "checking the toolchain" }.count == 1)
    }

    @Test func proceedAfterInstallDoesNothing() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
            ]))
        await model.start()
        #expect(model.state == .installed)
        let callsBefore = environment.calls
        #expect(!model.canProceed)
        await model.proceed()
        #expect(environment.calls == callsBefore)
        #expect(model.state == .installed)
    }

    /// A press while a step runs, the shortcut included, neither starts a second update
    /// nor cancels the first.
    @Test func proceedWhileRunningDoesNothing() async {
        let environment = GatedToolchainEnvironment()
        let model = UpdateModel(build: Self.build(), environment: environment)
        let first = Task { await model.proceed() }
        await environment.entered.wait()
        #expect(model.state == .running(.preflight))
        #expect(!model.canProceed)
        await model.proceed()
        #expect(environment.toolchainCalls == 1)
        environment.release.signal()
        await first.value
        #expect(environment.toolchainCalls == 1)
    }

    // MARK: cancel()

    /// A build that never returns is stopped by Cancel: the update fails as cancelled,
    /// never relaunches, and the next attempt runs clean.
    @Test func cancelStopsAHungBuildAndTheNextAttemptRunsClean() async {
        let environment = HangingStepEnvironment(hanging: .install)
        let model = UpdateModel(build: Self.build(), environment: environment)
        let first = Task { await model.start() }
        await environment.entered.wait()
        #expect(model.state == .running(.install))
        await model.pressAction()
        await first.value
        #expect(model.state == .failed(step: .install, message: "cancelled"))
        #expect(!environment.calls.contains("relaunch"))

        await model.start()
        #expect(model.state == .installed)
        #expect(environment.calls.filter { $0 == "install" }.count == 2)
        #expect(environment.calls.filter { $0 == "relaunch" }.count == 1)
    }

    /// A line the cancelled build prints late stays out of the next attempt's log.
    @Test func aCancelledBuildsLateOutputStaysOutOfTheNextLog() async throws {
        let environment = HangingStepEnvironment(hanging: .install)
        let model = UpdateModel(build: Self.build(), environment: environment)
        let first = Task { await model.start() }
        await environment.entered.wait()
        model.cancel()
        await first.value
        await model.start()
        let stale = try #require(environment.firstOutput)
        stale("late line from the cancelled build")
        for _ in 0..<10 { await Task.yield() }
        #expect(!model.log.contains("late line from the cancelled build"))
    }

    /// The fetch is cancellable too, and a cancelled fetch never reaches the build.
    @Test func cancelStopsAHungFetchBeforeTheBuild() async {
        let environment = HangingStepEnvironment(hanging: .sync)
        let model = UpdateModel(build: Self.build(), environment: environment)
        let first = Task { await model.start() }
        await environment.entered.wait()
        #expect(model.state == .running(.sync))
        await model.pressAction()
        await first.value
        #expect(model.state == .failed(step: .sync, message: "cancelled"))
        #expect(!environment.calls.contains("install"))
    }

    /// Preflight is a bounded probe: the button does not cancel it, and pressing it starts
    /// nothing either.
    @Test func pressActionDuringPreflightDoesNothing() async {
        let environment = GatedToolchainEnvironment()
        let model = UpdateModel(build: Self.build(), environment: environment)
        let first = Task { await model.start() }
        await environment.entered.wait()
        await model.pressAction()
        #expect(model.state == .running(.preflight))
        environment.release.signal()
        await first.value
        #expect(model.state == .failed(step: .preflight, message: "released"))
        #expect(environment.toolchainCalls == 1)
    }

    @Test func cancelWithNothingRunningDoesNothing() {
        let (model, _) = Self.model(.init())
        model.cancel()
        #expect(model.state == .idle)
    }

    @Test func proceedOnAnUnstampedBundleDoesNothing() async {
        let (model, environment) = Self.model(
            .init(), build: Self.build(variant: .release, sourceRoot: nil))
        #expect(!model.canProceed)
        await model.proceed()
        #expect(model.state == .idle)
        #expect(environment.calls.isEmpty)
    }

    // MARK: Dirty rule

    private static func entry(_ path: String, _ status: WorktreeStatus) -> StatusEntry {
        StatusEntry(path: path, kind: .blob, status: status, oid: nil, size: 0)
    }

    @Test func modifiedAndDeletedFilesAreDirty() {
        let report = StatusReport(
            entries: [
                Self.entry("a.rs", .modified), Self.entry("b.rs", .deleted),
                Self.entry("RCGit.app", .untracked), Self.entry("build/x", .ignored),
            ],
            scanned: 10, truncated: false)
        let state = SourceState(commit: Self.head, branch: "master", report: report)
        #expect(state.dirtyPaths == ["a.rs", "b.rs"])
        #expect(!state.isClean)
    }

    @Test func untrackedAndIgnoredFilesAreNotDirty() {
        let report = StatusReport(
            entries: [Self.entry("RCGit.app", .untracked), Self.entry("build/x", .ignored)],
            scanned: 10, truncated: false)
        let state = SourceState(commit: Self.head, branch: "master", report: report)
        #expect(state.dirtyPaths.isEmpty)
        #expect(state.isClean)
    }

    @Test func anEmptyReportIsClean() {
        let report = StatusReport(entries: [], scanned: 10, truncated: false)
        #expect(SourceState(commit: Self.head, branch: nil, report: report).isClean)
    }

    @Test func aTruncatedReportIsNotClean() {
        let report = StatusReport(entries: [], scanned: 10, truncated: true)
        let state = SourceState(commit: Self.head, branch: nil, report: report)
        #expect(state.dirtyPaths.isEmpty)
        #expect(!state.scanComplete)
        #expect(!state.isClean)
    }

    /// A scan that stopped at its cap may have missed uncommitted work, so the release
    /// update refuses rather than fetching over it.
    @Test func aTruncatedScanStopsTheUpdate() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(
                    SourceState(
                        commit: Self.head, branch: "master", dirtyPaths: [], scanComplete: false))
            ]))
        await model.start()
        guard case let .failed(step, message) = model.state else {
            Issue.record("expected a preflight failure, got \(model.state)")
            return
        }
        #expect(step == .preflight)
        #expect(message.contains("uncommitted work cannot be ruled out"))
        #expect(!environment.calls.contains("sync"))
    }

    @Test func listNamesUpToThreePaths() {
        #expect(UpdateModel.list(["a", "b"]) == "a, b")
        #expect(UpdateModel.list(["a", "b", "c"]) == "a, b, c")
        #expect(UpdateModel.list(["a", "b", "c", "d"]) == "a, b, c and 1 more")
    }
}
