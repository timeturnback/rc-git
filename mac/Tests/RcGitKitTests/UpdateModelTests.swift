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
    /// Reads the model's last log line. Set, each step records what the log said as it
    /// started, so a test can tell a note logged before its step from one logged after.
    var lastLine: (@MainActor @Sendable () -> String?)?
    private(set) var lastLines: [String?] = []
    /// Reads the model's state. Set, `quit()` records what the window showed as it quit.
    var currentState: (@MainActor @Sendable () -> UpdateState)?
    private(set) var stateAtQuit: UpdateState?

    init(_ script: Script) {
        self.script = script
    }

    private func probe() async {
        guard let lastLine else { return }
        lastLines.append(await lastLine())
    }

    func toolchain() async -> ToolchainCheck {
        await probe()
        calls.append("toolchain")
        return script.toolchain
    }

    /// Answers each call with the next scripted state, repeating the last one.
    func sourceState() async -> SourceStateResult {
        await probe()
        calls.append("sourceState")
        guard !script.sourceStates.isEmpty else { return .failure("no source state scripted") }
        return script.sourceStates.count > 1
            ? script.sourceStates.removeFirst() : script.sourceStates[0]
    }

    func sync() async -> String? {
        await probe()
        calls.append("sync")
        return script.syncFailure
    }

    func install(onOutput: @escaping @Sendable (String) -> Void) async -> String? {
        await probe()
        calls.append("install")
        for line in script.installOutput { onOutput(line) }
        return script.installFailure
    }

    func handOff() async {
        await probe()
        calls.append("handOff")
    }

    func reclaim() async {
        await probe()
        calls.append("reclaim")
    }

    func relaunch() async -> String? {
        await probe()
        calls.append("relaunch")
        return script.relaunchFailure
    }

    func quit() async {
        await probe()
        calls.append("quit")
        if let currentState { stateAtQuit = await currentState() }
    }
}

/// Holds the first toolchain probe until the test releases it, so a test can act while the
/// model sits in `.running(.preflight)`. A later probe answers at once, so a second attempt
/// the model failed to refuse shows up as a failure instead of a hang. Every other step fails
/// fast; only the probe count matters.
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
        let call = lock.withLock {
            calls += 1
            return calls
        }
        guard call == 1 else { return .failure("a second probe ran") }
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
    func quit() async {}
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

    func quit() async {
        calls.append("quit")
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
        commit: String? = head, dirty: Bool = false
    ) -> AppBuild {
        AppBuild(
            variant: variant, name: "rc-git", version: "0.1.0", build: "42",
            sourceRoot: sourceRoot, commit: commit, dirty: dirty, buildDate: nil)
    }

    private static func model(
        _ script: StubUpdateEnvironment.Script, build: AppBuild = build()
    ) -> (UpdateModel, StubUpdateEnvironment) {
        let environment = StubUpdateEnvironment(script)
        return (UpdateModel(build: build, environment: environment), environment)
    }

    /// What the window saw at each `onChanged`: the state and the log it would draw.
    private typealias Frame = (state: UpdateState, log: [String])

    /// Whether every redraw carried one change, so no state was shown only once a later log
    /// line happened to redraw the window. Starts from the fresh model, idle with no log.
    private static func eachRedrawCarriesOneChange(_ frames: [Frame]) -> Bool {
        let previous: [Frame] = [(.idle, [])] + frames
        return zip(previous, frames).allSatisfy { before, after in
            before.state == after.state || before.log == after.log
        }
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
        var seen: [Frame] = []
        model.onChanged = { [weak model] in
            if let model { seen.append((model.state, model.log)) }
        }
        await model.start()
        #expect(model.state == .installed)
        #expect(seen.last?.state == model.state)
        #expect(Self.eachRedrawCarriesOneChange(seen))
        #expect(
            environment.calls == [
                "toolchain", "sourceState", "sync", "sourceState", "install", "handOff",
                "relaunch", "quit",
            ])
        #expect(model.log.contains("on master"))
        #expect(model.log.contains("built RCGit.app"))
    }

    /// Quitting never returns, so the window has to read `.installed` before the quit, not
    /// after it.
    @Test func theInstalledStateIsShownBeforeQuitting() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
            ]))
        environment.currentState = { [weak model] in model?.state ?? .idle }
        await model.start()
        #expect(environment.stateAtQuit == .installed)
    }

    /// The commit did not move, so the minutes-long rebuild is skipped.
    @Test func anUnchangedCheckoutStopsBeforeTheBuild() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]))
        var seen: [Frame] = []
        model.onChanged = { [weak model] in
            if let model { seen.append((model.state, model.log)) }
        }
        await model.start()
        #expect(model.state == .upToDate)
        #expect(seen.last?.state == model.state)
        #expect(Self.eachRedrawCarriesOneChange(seen))
        #expect(!environment.calls.contains("install"))
    }

    /// A bundle built from uncommitted work holds code in no commit, so the same commit is no
    /// reason to skip the rebuild.
    @Test func aDirtyBuildAtTheSameCommitRebuilds() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]),
            build: Self.build(dirty: true))
        await model.start()
        #expect(model.state == .installed)
        #expect(environment.calls.contains("install"))
    }

    /// A bundle stamped with no commit cannot prove it matches the checkout, so it rebuilds
    /// rather than report itself up to date.
    @Test func aBundleWithNoRecordedCommitRebuildsInsteadOfReportingUpToDate() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]),
            build: Self.build(commit: nil))
        await model.start()
        #expect(model.state == .installed)
        #expect(environment.calls.contains("install"))
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

    /// Every step outside the process is logged before it runs, so the log's last line names
    /// a step that stalls. The stub reads the log as each step starts: a note moved after its
    /// await would leave the previous step's line there.
    @Test func everyOutsideStepIsLoggedBeforeItRuns() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                .state(SourceState(commit: Self.upstream, branch: "master", dirtyPaths: [])),
            ]))
        environment.lastLine = { [weak model] in model?.log.last }
        await model.start()
        #expect(model.state == .installed)
        #expect(
            environment.lastLines == [
                "checking the toolchain", "reading the checkout", "fetching and fast-forwarding",
                "reading the checkout after the fetch", "building", "launching the new build",
                "launching the new build", "quitting this instance",
            ])
    }

    /// The dev bundle names its build for what it is: the working tree, not a fetched commit.
    @Test func aRebuildLogsTheWorkingTreeBuildBeforeRunningIt() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: []))
            ]),
            build: Self.build(variant: .dev))
        environment.lastLine = { [weak model] in model?.log.last }
        await model.start()
        #expect(model.state == .installed)
        #expect(
            environment.lastLines == [
                "checking the toolchain", "reading the checkout", "rebuilding the working tree",
                "launching the new build", "launching the new build", "quitting this instance",
            ])
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
        var seen: [Frame] = []
        model.onChanged = { [weak model] in
            if let model { seen.append((model.state, model.log)) }
        }
        await model.start()
        #expect(model.state == .failed(step: .sync, message: "host not trusted"))
        #expect(seen.last?.state == model.state)
        #expect(Self.eachRedrawCarriesOneChange(seen))
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
    @Test func aFailedRelaunchReportsTheRelaunchStep() async {
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
            Issue.record("expected a relaunch failure, got \(model.state)")
            return
        }
        #expect(step == .relaunch)
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
        #expect(!environment.calls.contains("quit"))
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

    /// The checkout is read again after the fetch; a read that fails there is the fetch
    /// step's failure, and nothing is built from a checkout nobody could read.
    @Test func anUnreadableCheckoutAfterTheFetchFailsTheSyncStep() async {
        let (model, environment) = Self.model(
            .init(sourceStates: [
                .state(SourceState(commit: Self.head, branch: "master", dirtyPaths: [])),
                .failure("x"),
            ]))
        await model.start()
        #expect(model.state == .failed(step: .sync, message: "x"))
        #expect(!environment.calls.contains("install"))
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
            environment.calls == [
                "toolchain", "sourceState", "install", "handOff", "relaunch", "quit",
            ])
        #expect(model.log.contains("uncommitted work goes in: a.rs"))
        #expect(!model.log.contains { $0.contains("stopped at its cap") })
    }

    /// A rebuild still builds the tree as it stands, but a scan that stopped at its cap
    /// says the dirty list it logged may be short.
    @Test func aRebuildAfterATruncatedScanSaysTheListIsPartial() async {
        let (model, _) = Self.model(
            .init(sourceStates: [
                .state(
                    SourceState(
                        commit: Self.head, branch: "master", dirtyPaths: ["a.rs"],
                        scanComplete: false))
            ]),
            build: Self.build(variant: .dev))
        await model.start()
        #expect(model.state == .installed)
        #expect(model.log.contains("uncommitted work goes in: a.rs"))
        #expect(
            model.log.contains(
                "the status scan stopped at its cap, so more uncommitted work may go in "
                    + "than is listed"))
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

    @Test func aSecondAttemptStartsWithAFreshLog() async {
        let (model, _) = Self.model(.init(toolchain: .missing(["cargo"])))
        await model.start()
        model.reset()
        await model.start()
        #expect(model.log.filter { $0 == "checking the toolchain" }.count == 1)
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
        #expect(
            model.log.suffix(3) == [
                "line \(UpdateModel.maxLogLines + 499)", "launching the new build",
                "quitting this instance",
            ])
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

    /// A second start or a reset while an attempt runs is ignored: two builds never run
    /// into the same staging folder, and the running attempt keeps its state and its log.
    @Test func aSecondStartWhileRunningIsIgnored() async {
        let environment = GatedToolchainEnvironment()
        let model = UpdateModel(build: Self.build(), environment: environment)
        let first = Task { await model.start() }
        await environment.entered.wait()
        #expect(model.isBusy)
        await model.start()
        model.reset()
        #expect(model.isBusy)
        #expect(model.state == .running(.preflight))
        #expect(model.log.contains("checking the toolchain"))
        environment.release.signal()
        await first.value
        #expect(environment.toolchainCalls == 1)
        #expect(model.state == .failed(step: .preflight, message: "released"))
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
