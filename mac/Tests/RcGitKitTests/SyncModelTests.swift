import Foundation
import Testing

@testable import RcGitKit

/// A scripted performer: each step returns its queued results in order, or throws.
private final class StubSyncPerformer: SyncPerforming, @unchecked Sendable {
    struct Script {
        /// One answer per preflight, the last one repeating; `nil` makes that preflight throw.
        var preflights: [RemoteStatus?]
        /// Seconds each preflight sleeps before answering, in call order.
        var preflightDelays: [TimeInterval] = []
        var defaultBranch: String? = "main"
        var defaultBranchFails = false
        var fetches: [Result<FetchOutcome, SyncError>] = []
        var pulls: [Result<PullOutcome, SyncError>] = []
        var checkouts: [Result<CheckoutOutcome, SyncError>] = []
        var pushes: [Result<PushOutcome, SyncError>] = []
    }

    private let lock = NSLock()
    private var script: Script
    private(set) var fetchCredentials: [Credentials?] = []
    private(set) var fetchHostKeys: [String?] = []
    private(set) var pushCredentials: [Credentials?] = []
    private(set) var checkoutBranches: [String] = []
    private(set) var pullCalls = 0
    private(set) var pushCalls = 0
    private(set) var preflightCalls = 0
    private(set) var defaultBranchCalls = 0
    /// Called on the step's own thread, with that step's token, before the step
    /// answers. A test uses it to cancel exactly while the transfer is in flight.
    var duringStep: (@Sendable (CancelToken) -> Void)?
    /// Called on the step's own thread before a checkout or a pull answers. Those steps take
    /// no token, so a test cancels through the model while this holds the step open.
    var duringLocalStep: (@Sendable () -> Void)?

    init(script: Script) {
        self.script = script
    }

    static func upstream(host: String = "example.com", ahead: Int = 0, behind: Int = 0)
        -> RemoteStatus
    {
        RemoteStatus(
            remote: "origin",
            url: "https://\(host)/repo.git",
            host: host,
            branch: "main",
            ahead: ahead,
            behind: behind,
            hasUpstream: true,
            usesSsh: false
        )
    }

    func preflight(repoPath: String) throws -> RemoteStatus {
        lock.lock()
        preflightCalls += 1
        let next: RemoteStatus?? =
            script.preflights.count > 1
            ? script.preflights.removeFirst() : script.preflights.first
        let delay = script.preflightDelays.isEmpty ? 0 : script.preflightDelays.removeFirst()
        lock.unlock()
        if delay > 0 {
            Thread.sleep(forTimeInterval: delay)
        }
        guard let next else {
            throw SyncError.failure(message: "no preflight scripted")
        }
        guard let status = next else {
            throw SyncError.failure(message: "preflight failed")
        }
        return status
    }

    func defaultBranch(repoPath: String) throws -> String? {
        lock.lock()
        defer { lock.unlock() }
        defaultBranchCalls += 1
        if script.defaultBranchFails {
            throw RepositoryError("cannot open /tmp/repo")
        }
        return script.defaultBranch
    }

    func checkout(repoPath: String, branch: String) throws(SyncError) -> CheckoutOutcome {
        duringLocalStep?()
        lock.lock()
        defer { lock.unlock() }
        checkoutBranches.append(branch)
        guard !script.checkouts.isEmpty else {
            throw SyncError.failure(message: "unexpected checkout")
        }
        return try script.checkouts.removeFirst().get()
    }

    func fetch(
        repoPath: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> FetchOutcome {
        duringStep?(cancel)
        lock.lock()
        defer { lock.unlock() }
        fetchCredentials.append(credentials)
        fetchHostKeys.append(acceptedHostKey)
        guard !script.fetches.isEmpty else {
            throw SyncError.failure(message: "unexpected fetch")
        }
        return try script.fetches.removeFirst().get()
    }

    func pullFastForward(repoPath: String) throws(SyncError) -> PullOutcome {
        duringLocalStep?()
        lock.lock()
        defer { lock.unlock() }
        pullCalls += 1
        guard !script.pulls.isEmpty else {
            throw SyncError.failure(message: "unexpected pull")
        }
        return try script.pulls.removeFirst().get()
    }

    func push(
        repoPath: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> PushOutcome {
        duringStep?(cancel)
        lock.lock()
        defer { lock.unlock() }
        pushCredentials.append(credentials)
        pushCalls += 1
        guard !script.pushes.isEmpty else {
            throw SyncError.failure(message: "unexpected push")
        }
        return try script.pushes.removeFirst().get()
    }
}

private final class InMemoryCredentialStore: CredentialStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String: Credentials] = [:]

    init(_ items: [String: Credentials] = [:]) {
        self.items = items
    }

    private func key(_ host: String, _ kind: CredentialKind) -> String {
        kind == .httpsToken ? host : "\(host)#ssh"
    }

    func credentials(host: String, kind: CredentialKind) throws -> Credentials? {
        lock.lock()
        defer { lock.unlock() }
        return items[key(host, kind)]
    }

    func store(_ credentials: Credentials, host: String, kind: CredentialKind) throws {
        lock.lock()
        defer { lock.unlock() }
        items[key(host, kind)] = credentials
    }

    func delete(host: String, kind: CredentialKind) throws {
        lock.lock()
        defer { lock.unlock() }
        items[key(host, kind)] = nil
    }

    var stored: [String: Credentials] {
        lock.lock()
        defer { lock.unlock() }
        return items
    }
}

/// In-memory host-key approvals for tests.
private final class InMemoryHostKeyStore: HostKeyStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String: String] = [:]

    init(_ items: [String: String] = [:]) {
        self.items = items
    }

    func fingerprint(host: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return items[host]
    }

    func store(fingerprint: String, host: String) {
        lock.lock()
        defer { lock.unlock() }
        items[host] = fingerprint
    }

    func forget(host: String) {
        lock.lock()
        defer { lock.unlock() }
        items[host] = nil
    }

    var stored: [String: String] {
        lock.lock()
        defer { lock.unlock() }
        return items
    }
}

private let cleanFetch = FetchOutcome(updatedRefs: 1)
private let cleanPull = PullOutcome(
    newHead: nil, commitsAdvanced: false, filesChanged: 2, alreadyUpToDate: false)
private let cleanPush = PushOutcome(pushedCommits: 1, alreadyUpToDate: false)
private let switchedToMain = CheckoutOutcome(
    branch: "main", newHead: oid(0), filesChanged: 4,
    alreadyOnBranch: false)
private let alreadyOnMain = CheckoutOutcome(
    branch: "main", newHead: oid(0), filesChanged: 0,
    alreadyOnBranch: true)

@MainActor
@Suite(.timeLimit(.minutes(1))) struct SyncModelTests {
    private func model(
        _ script: StubSyncPerformer.Script,
        store: InMemoryCredentialStore = InMemoryCredentialStore(),
        hostKeys: InMemoryHostKeyStore = InMemoryHostKeyStore()
    ) -> (SyncModel, StubSyncPerformer, InMemoryCredentialStore) {
        let performer = StubSyncPerformer(script: script)
        let model = SyncModel(
            repoPath: "/tmp/repo", performer: performer, store: store, hostKeys: hostKeys)
        return (model, performer, store)
    }

    @Test func aFullSyncRunsAllThreeStepsAndSummarizes() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        var states: [SyncState] = []
        model.onStateChanged = { states.append(model.state) }
        await model.refreshStatus()
        await model.sync()

        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 1)))
        #expect(states.contains(.running(.fetch)))
        #expect(states.contains(.running(.pull)))
        #expect(states.contains(.running(.push)))
        #expect(performer.pushCalls == 1)
    }

    @Test func authRetryPromptsStoresOnlyOnSuccess() async {
        let store = InMemoryCredentialStore()
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(host: "git.example.com")],
                fetches: [
                    .failure(.authRequired(message: "401")),
                    .success(cleanFetch),
                ]
            ), store: store)
        var promptedHost: String?
        model.onCredentialsNeeded = { host, _, _, resume in
            promptedHost = host
            resume(Credentials(username: "user", token: "sekret"))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedHost == "git.example.com")
        if case .done = model.state {
        } else {
            Issue.record("expected done, got \(model.state)")
        }
        #expect(performer.fetchCredentials[0] == nil, "the first attempt is anonymous")
        #expect(performer.fetchCredentials.last??.username == "user")
        #expect(store.stored["git.example.com"]?.token == "sekret")
    }

    @Test func rejectedCredentialsAreNeverStoredAndPromptsAreCapped() async {
        let store = InMemoryCredentialStore()
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [
                    .failure(.authRequired(message: "401")),
                    .failure(.authRequired(message: "401")),
                    .failure(.authRequired(message: "401")),
                ]
            ), store: store)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(Credentials(username: "user", token: "wrong"))
        }
        await model.refreshStatus()
        await model.fetch()

        if case let .failed(step, _) = model.state {
            #expect(step == .fetch)
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(prompts == 2, "attempt limit caps the prompt loop")
        #expect(store.stored.isEmpty, "rejected credentials must never be stored")
    }

    @Test func cancellingThePromptFailsTheStep() async {
        let (model, _, store) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401"))]
            ))
        model.onCredentialsNeeded = { _, _, _, resume in resume(nil) }
        await model.refreshStatus()
        await model.fetch()

        if case let .failed(_, message) = model.state {
            #expect(message.contains("canceled"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(store.stored.isEmpty)
    }

    @Test func divergenceStopsTheSyncBeforePush() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(ahead: 1, behind: 1)],
                fetches: [.success(cleanFetch)],
                pulls: [.failure(.diverged(message: "cannot fast-forward 'main'"))]
            ))
        await model.refreshStatus()
        await model.sync()

        if case let .diverged(message) = model.state {
            #expect(message.contains("fast-forward"))
        } else {
            Issue.record("expected diverged, got \(model.state)")
        }
        #expect(performer.pushCalls == 0, "a diverged pull must stop the push")
    }

    @Test func noUpstreamFailsFastWithoutNetwork() async {
        let status = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, usesSsh: false)
        let (model, performer, _) = model(.init(preflights: [status]))
        await model.refreshStatus()
        await model.sync()

        if case let .failed(_, message) = model.state {
            #expect(message.contains("no upstream"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.isEmpty, "no network step may run")
    }

    @Test func aSecondWorkflowWhileBusyIsANoOp() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        await model.refreshStatus()
        async let first: Void = model.sync()
        async let second: Void = model.sync()
        _ = await (first, second)

        #expect(performer.pushCalls <= 1, "the busy gate must drop the overlapping sync")
    }

    @Test func upToDateSyncReportsZeros() async {
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(FetchOutcome(updatedRefs: 0))],
                pulls: [
                    .success(
                        PullOutcome(
                            newHead: nil, commitsAdvanced: false, filesChanged: 0,
                            alreadyUpToDate: true))
                ],
                pushes: [.success(PushOutcome(pushedCommits: 0, alreadyUpToDate: true))]
            ))
        await model.refreshStatus()
        await model.sync()

        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 0, pulledFiles: 0, pushedCommits: 0)))
    }

    @Test func resetReturnsATerminalStateToIdle() async {
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.failure(message: "boom"))]
            ))
        await model.refreshStatus()
        await model.fetch()
        if case .failed = model.state {
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        model.reset()
        #expect(model.state == .idle)
    }
    private static func sshUpstream(host: String = "git.example.com") -> RemoteStatus {
        RemoteStatus(
            remote: "origin",
            url: "\(host):repo.git",
            host: host,
            branch: "main",
            ahead: 0,
            behind: 0,
            hasUpstream: true,
            usesSsh: true
        )
    }

    @Test func unknownHostKeyPromptsOncePersistsAndRetries() async {
        let hostKeys = InMemoryHostKeyStore()
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(
                        .hostKeyUnknown(
                            message: "not a known host",
                            hostKey: "ssh-ed25519 SHA256:abc123")),
                    .success(cleanFetch),
                ]
            ), hostKeys: hostKeys)
        var promptedKey: String?
        model.onHostKeyNeeded = { _, hostKey, resume in
            promptedKey = hostKey
            resume(true)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedKey == "ssh-ed25519 SHA256:abc123")
        if case .done = model.state {
        } else {
            Issue.record("expected done, got \(model.state)")
        }
        #expect(performer.fetchHostKeys == [nil, "SHA256:abc123"])
        #expect(hostKeys.stored["git.example.com"] == "SHA256:abc123")
    }

    @Test func decliningTheHostKeyFailsTheStep() async {
        let hostKeys = InMemoryHostKeyStore()
        let (model, _, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(
                        .hostKeyUnknown(
                            message: "not a known host",
                            hostKey: "ssh-ed25519 SHA256:abc123"))
                ]
            ), hostKeys: hostKeys)
        model.onHostKeyNeeded = { _, _, resume in resume(false) }
        await model.refreshStatus()
        await model.fetch()

        if case let .failed(_, message) = model.state {
            #expect(message.contains("not trusted"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(hostKeys.stored.isEmpty, "a declined key must not be stored")
    }

    @Test func aStoredFingerprintSkipsThePrompt() async {
        let hostKeys = InMemoryHostKeyStore(["git.example.com": "SHA256:abc123"])
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [.success(cleanFetch)]
            ), hostKeys: hostKeys)
        var prompted = false
        model.onHostKeyNeeded = { _, _, resume in
            prompted = true
            resume(true)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(!prompted, "a stored fingerprint must be passed without asking")
        #expect(performer.fetchHostKeys == ["SHA256:abc123"])
    }

    @Test func aChangedHostKeyFailsWithoutPrompting() async {
        let hostKeys = InMemoryHostKeyStore(["git.example.com": "SHA256:abc123"])
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(
                        .hostKeyUnknown(
                            message: "not a known host",
                            hostKey: "ssh-ed25519 SHA256:other"))
                ]
            ), hostKeys: hostKeys)
        var prompted = false
        model.onHostKeyNeeded = { _, _, resume in
            prompted = true
            resume(true)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(!prompted, "a changed key must not reach the first-use trust sheet")
        if case let .failed(_, message) = model.state {
            #expect(message.contains("SHA256:abc123"), "\(message)")
            #expect(message.contains("SHA256:other"), "\(message)")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchHostKeys == ["SHA256:abc123"])
        #expect(hostKeys.stored["git.example.com"] == "SHA256:abc123")
    }

    @Test func sshPassphrasePromptRetriesAndStoresUnderTheSshKind() async {
        let store = InMemoryCredentialStore()
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(
                        .passphraseRequired(message: "key is encrypted", key: "/k/id_ed25519")),
                    .success(cleanFetch),
                ]
            ), store: store)
        var promptedKind: CredentialKind?
        var promptedKey: String?
        model.onCredentialsNeeded = { _, kind, key, resume in
            promptedKind = kind
            promptedKey = key
            resume(Credentials(username: "", token: "opensesame"))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedKind == .sshPassphrase)
        #expect(promptedKey == "/k/id_ed25519", "the prompt names the locked key")
        if case .done = model.state {
        } else {
            Issue.record("expected done, got \(model.state)")
        }
        #expect(performer.fetchCredentials.last??.token == "opensesame")
        #expect(store.stored["git.example.com#ssh"]?.token == "opensesame")
        #expect(store.stored["git.example.com"] == nil)
    }

    /// Over ssh, `authRequired` means every key was offered and rejected: no passphrase
    /// can fix that, so the step fails with the server's answer instead of asking.
    @Test func anSshAuthRejectionFailsAtOnceWithoutAPrompt() async {
        let (model, performer, store) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(.authRequired(message: "no key accepted")),
                    .success(cleanFetch),
                ]
            ))
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(Credentials(username: "", token: "opensesame"))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(prompts == 0, "a rejected key is not a locked key")
        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message == "no key accepted")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.count == 1, "no retry")
        #expect(store.stored.isEmpty)
    }

    @Test func onlyTheErrorTheSecretCanFixEarnsAPrompt() {
        let locked = SyncError.passphraseRequired(message: "m", key: "/k/id")
        let rejected = SyncError.authRequired(message: "m")
        #expect(
            SyncModel.credentialRetry(after: locked, kind: .sshPassphrase)
                == .prompt(key: "/k/id"))
        #expect(SyncModel.credentialRetry(after: rejected, kind: .sshPassphrase) == .none)
        #expect(
            SyncModel.credentialRetry(after: rejected, kind: .httpsToken) == .prompt(key: nil))
        #expect(SyncModel.credentialRetry(after: locked, kind: .httpsToken) == .none)
        #expect(
            SyncModel.credentialRetry(after: .failure(message: "m"), kind: .sshPassphrase)
                == .none)
    }

    @Test func anScpLikeRemoteAsksForThePassphraseAndLeavesTheHttpsTokenAlone() async {
        let httpsToken = Credentials(username: "me", token: "pat-for-https")
        let store = InMemoryCredentialStore(["example.com": httpsToken])
        let scpLike = RemoteStatus(
            remote: "origin", url: "example.com:x/y.git", host: "example.com",
            branch: "main", ahead: 0, behind: 0, hasUpstream: true, usesSsh: true)
        let (model, performer, _) = model(
            .init(
                preflights: [scpLike],
                fetches: [
                    .failure(
                        .passphraseRequired(message: "key is encrypted", key: "/k/id_ed25519")),
                    .success(cleanFetch),
                ]
            ), store: store)
        var promptedKind: CredentialKind?
        var promptedKey: String?
        model.onCredentialsNeeded = { _, kind, key, resume in
            promptedKind = kind
            promptedKey = key
            resume(Credentials(username: "", token: "opensesame"))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedKind == .sshPassphrase)
        #expect(promptedKey == "/k/id_ed25519")
        #expect(
            performer.fetchCredentials.allSatisfy { $0?.token != "pat-for-https" },
            "the HTTPS token must never be offered as the key passphrase")
        #expect(store.stored["example.com#ssh"]?.token == "opensesame")
        #expect(store.stored["example.com"]?.token == "pat-for-https")
    }

    @Test func cancellingMidStepEndsIdleWithoutAnErrorOrTheRemainingSteps() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(ahead: 1)],
                fetches: [.failure(.failure(message: "fetch: cancelled"))],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        // The real transfer sees the flag and fails; the stub stands in for that, and
        // cancels from the step's own thread the way the toolbar button does.
        performer.duringStep = { cancel in cancel.signal() }
        await model.refreshStatus()
        await model.sync()

        #expect(model.state == .idle, "a cancel is not a failure to report")
        #expect(performer.pullCalls == 0, "a cancelled fetch must stop the workflow")
        #expect(performer.pushCalls == 0)
        #expect(!model.isBusy)
    }

    @Test func cancellingIsRefusedWhenNothingIsRunningAndTheNextSyncStillWorks() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        await model.refreshStatus()
        model.cancelRunning()
        #expect(model.state == .idle)
        #expect(!model.isCancellable)

        // The flag a cancel raises belongs to one workflow: the next one starts clean.
        await model.sync()
        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 1)))
        #expect(performer.pushCalls == 1)
    }

    @Test func aCancelledAuthFailureDoesNotReopenTheCredentialPrompt() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401"))]
            ))
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(Credentials(username: "user", token: "token"))
        }
        performer.duringStep = { cancel in cancel.signal() }
        await model.refreshStatus()
        await model.fetch()

        #expect(prompts == 0, "the user already said stop")
        #expect(model.state == .idle)
        #expect(performer.fetchCredentials.count == 1, "no retry after a cancel")
    }

    /// Cancels through the model from inside the performer's checkout or pull, the way the
    /// toolbar would, after checking the toolbar does not offer it. The step's thread hops to
    /// the main actor, which is free while the model awaits the step.
    private func cancelDuringLocalStep(
        _ step: SyncStep, model: SyncModel, performer: StubSyncPerformer
    ) {
        performer.duringLocalStep = {
            DispatchQueue.main.sync {
                MainActor.assumeIsolated {
                    #expect(model.state == .running(step))
                    #expect(
                        !model.isCancellable, "a \(step) cannot stop; the toolbar must not offer it"
                    )
                    model.cancelRunning()
                }
            }
        }
    }

    @Test func aCancelDuringTheSwitchStillReportsItAndSkipsTheNetwork() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                checkouts: [.success(switchedToMain)]
            ))
        await model.refreshStatus()
        cancelDuringLocalStep(.checkout, model: model, performer: performer)
        await model.backToDefaultBranch()

        #expect(
            model.state
                == .done(
                    SyncSummary(
                        fetchedRefs: 0, pulledFiles: 4, pushedCommits: 0, switchedTo: "main",
                        cancelledAfter: .checkout)),
            "the switch landed, so the browser must still follow it"
        )
        #expect(performer.fetchCredentials.isEmpty, "the cancel stops the workflow after it")
        #expect(performer.pullCalls == 0)
    }

    @Test func aCancelDuringThePullKeepsItsFilesAndSkipsThePush() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(ahead: 1, behind: 1)],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        await model.refreshStatus()
        cancelDuringLocalStep(.pull, model: model, performer: performer)
        await model.sync()

        #expect(
            model.state
                == .done(
                    SyncSummary(
                        fetchedRefs: 1, pulledFiles: 2, pushedCommits: 0, cancelledAfter: .pull)
                ))
        #expect(performer.pushCalls == 0, "the cancel stops the workflow before the push")
        #expect(!model.isBusy)
    }

    @Test func aCancelBeforeTheSwitchStartsSwitchesNothing() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                checkouts: [.success(switchedToMain)]
            ))
        var cancelled = false
        model.onStateChanged = {
            if !cancelled, model.state == .running(.checkout) {
                cancelled = true
                model.cancelRunning()
            }
        }
        await model.refreshStatus()
        await model.backToDefaultBranch()

        #expect(cancelled)
        #expect(model.state == .idle)
        #expect(performer.checkoutBranches.isEmpty, "a cancel before the write runs it never")
    }

    @Test func backToDefaultSwitchesThenFetchesAndFastForwards() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                checkouts: [.success(switchedToMain)]
            ))
        var states: [SyncState] = []
        model.onStateChanged = { states.append(model.state) }
        await model.refreshStatus()
        await model.backToDefaultBranch()

        #expect(performer.checkoutBranches == ["main"])
        let steps = states.compactMap { state -> SyncStep? in
            if case let .running(step) = state { return step }
            return nil
        }
        #expect(
            steps.filter { $0 != .checkout } == [.fetch, .pull],
            "the switch runs first, then the network steps: \(steps)")
        #expect(steps.first == .checkout)
        #expect(performer.pullCalls == 1)
        #expect(
            model.state
                == .done(
                    SyncSummary(
                        fetchedRefs: 1, pulledFiles: 6, pushedCommits: 0, switchedTo: "main")),
            "the files the switch wrote and the ones the pull wrote both count"
        )
    }

    @Test func backToDefaultOnTheBranchAlreadyStillFetchesButReportsNoSwitch() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                checkouts: [.success(alreadyOnMain)]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        #expect(performer.pullCalls == 1)
        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 0)),
            "nothing was switched, so nothing to follow in the UI"
        )
    }

    @Test func aRefusedSwitchStopsBeforeAnyNetworkStep() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                checkouts: [
                    .failure(
                        .failure(
                            message:
                                "checkout: 'alpha.txt' has uncommitted changes; "
                                + "commit, stash, or discard it first"))
                ]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        if case let .failed(step, message) = model.state {
            #expect(step == .checkout)
            #expect(message.contains("alpha.txt"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.isEmpty, "a refused switch may not reach the network")
        #expect(performer.pullCalls == 0)
    }

    @Test func noMainlineBranchFailsBeforeTheSwitch() async {
        let (model, performer, _) = model(
            .init(preflights: [StubSyncPerformer.upstream()], defaultBranch: nil))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        if case let .failed(step, message) = model.state {
            #expect(step == .checkout)
            #expect(message.contains("mainline"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.checkoutBranches.isEmpty)
    }

    @Test func theBranchSwitchedToWithoutAnUpstreamStopsBeforeTheFetch() async {
        let noUpstream = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, usesSsh: false)
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(), noUpstream],
                fetches: [.success(cleanFetch)],
                checkouts: [.success(switchedToMain)]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        #expect(performer.checkoutBranches == ["main"], "the switch is local and still runs")
        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message.contains("no upstream"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.isEmpty, "no network step may run")
    }

    @Test func aFailedRefreshClearsTheStaleStatusAndSaysWhy() async {
        let (model, _, _) = model(
            .init(preflights: [StubSyncPerformer.upstream(ahead: 2), nil]))
        await model.refreshStatus()
        #expect(SyncTitles.push(model.remoteStatus) == "Push 2 Commits")
        var changes = 0
        model.onStateChanged = { changes += 1 }
        await model.refreshStatus()

        #expect(model.remoteStatus == nil, "a failed read must not keep the older count")
        #expect(model.statusError == "preflight failed")
        #expect(changes == 1, "the views hear about the failure")
        #expect(SyncTitles.push(model.remoteStatus) == "Push")
        #expect(!SyncTitles.canPull(status: model.remoteStatus, busy: false))
        #expect(!SyncTitles.canPush(status: model.remoteStatus, busy: false))
        #expect(
            SyncTitles.idleSyncTooltip(status: model.remoteStatus, error: model.statusError)
                == "Sync — couldn't read upstream status: preflight failed")
    }

    @Test func aSuccessfulRefreshAfterAFailureClearsTheError() async {
        let (model, _, _) = model(.init(preflights: [nil, StubSyncPerformer.upstream()]))
        await model.refreshStatus()
        #expect(model.statusError != nil)
        await model.refreshStatus()
        #expect(model.statusError == nil)
        #expect(model.remoteStatus == StubSyncPerformer.upstream())
    }

    @Test func aSlowRefreshDoesNotOverwriteTheFreshReadAWorkflowMade() async {
        let (model, _, _) = model(
            .init(
                preflights: [
                    StubSyncPerformer.upstream(ahead: 1), StubSyncPerformer.upstream(ahead: 2),
                ],
                preflightDelays: [1],
                fetches: [.success(cleanFetch)]
            ))
        var seen: [Int?] = []
        model.onStateChanged = { seen.append(model.remoteStatus?.ahead) }
        let slow = Task { await model.refreshStatus() }
        try? await Task.sleep(for: .milliseconds(50))
        await model.fetch()
        await slow.value

        #expect(model.remoteStatus?.ahead == 2)
        #expect(!seen.contains(1), "the older answer never lands: \(seen)")
    }

    @Test func aBurstOfRefreshesCostsTwoPreflightsAndOneMainlineRead() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()], preflightDelays: [1],
                defaultBranch: nil))
        let slow = Task { await model.refreshStatus() }
        try? await Task.sleep(for: .milliseconds(50))
        let burst = (0..<20).map { _ in Task { await model.refreshStatus() } }
        await slow.value
        for refresh in burst { await refresh.value }
        #expect(performer.preflightCalls == 2, "the burst joins one more read")

        await model.refreshStatus()
        await model.refreshStatus()
        #expect(
            performer.defaultBranchCalls == 1,
            "a repository that names no mainline is not asked again")
        #expect(model.defaultBranch == nil)
    }

    @Test func aFailedMainlineReadIsRetriedOnTheNextRefresh() async {
        let (model, performer, _) = model(
            .init(preflights: [StubSyncPerformer.upstream()], defaultBranchFails: true))
        await model.refreshStatus()
        await model.refreshStatus()
        #expect(performer.defaultBranchCalls == 2, "a failure is not an answer")
    }

    @Test func anUnreadableMainlineNamesTheCause() async {
        let (model, performer, _) = model(
            .init(preflights: [StubSyncPerformer.upstream()], defaultBranchFails: true))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        if case let .failed(step, message) = model.state {
            #expect(step == .checkout)
            #expect(message == "cannot read the mainline branch: cannot open /tmp/repo")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.checkoutBranches.isEmpty)
    }

    @Test func anUnreadableUpstreamAfterTheSwitchStopsBeforeTheFetch() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(), nil],
                fetches: [.success(cleanFetch)],
                checkouts: [.success(switchedToMain)]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message == "cannot read the upstream status: preflight failed")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.checkoutBranches == ["main"])
        #expect(performer.fetchCredentials.isEmpty, "no network step may run")
    }

    @Test func anUnreadableUpstreamRefusesTheFetchWithTheCause() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(), nil],
                fetches: [.success(cleanFetch)]))
        await model.refreshStatus()
        await model.fetch()

        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message == "cannot read the upstream status: preflight failed")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.isEmpty, "no network step may run")
        #expect(model.remoteStatus == nil)
    }

    @Test func aCachedMissingUpstreamDoesNotRefuseOnceOneIsConfigured() async {
        let noUpstream = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, usesSsh: false)
        let (model, performer, _) = model(
            .init(
                preflights: [noUpstream, StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)]))
        await model.refreshStatus()
        #expect(model.remoteStatus?.hasUpstream == false)
        await model.fetch()

        #expect(
            model.state == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 0, pushedCommits: 0)))
        #expect(performer.fetchCredentials.count == 1)
    }

    @Test func aRemoteMovedSinceTheRefreshIsAskedForItsOwnCredentials() async {
        let store = InMemoryCredentialStore([
            "old.example.com": Credentials(username: "old", token: "old-token"),
            "new.example.com": Credentials(username: "new", token: "new-token"),
        ])
        let (model, performer, _) = model(
            .init(
                preflights: [
                    StubSyncPerformer.upstream(host: "old.example.com"),
                    StubSyncPerformer.upstream(host: "new.example.com"),
                ],
                fetches: [.success(cleanFetch)]
            ), store: store)
        await model.refreshStatus()
        await model.fetch()

        #expect(performer.fetchCredentials.map { $0?.token } == ["new-token"])
    }
}

@Suite struct SyncTitlesTests {
    @Test func noPreflightYetShowsTheBareVerbs() {
        #expect(SyncTitles.pull(nil) == "Pull")
        #expect(SyncTitles.push(nil) == "Push")
    }

    @Test func upToDateShowsTheBareVerbs() {
        let status = StubSyncPerformer.upstream()
        #expect(SyncTitles.pull(status) == "Pull")
        #expect(SyncTitles.push(status) == "Push")
    }

    @Test func countsFromTheLastPreflightPluralize() {
        let status = StubSyncPerformer.upstream(ahead: 3, behind: 1)
        #expect(SyncTitles.push(status) == "Push 3 Commits")
        #expect(SyncTitles.pull(status) == "Pull 1 Commit")
    }

    @Test func pullAndPushNeedAKnownUpstreamAndAnIdleModel() {
        let noUpstream = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, usesSsh: false)
        let upstream = StubSyncPerformer.upstream()
        #expect(!SyncTitles.canPull(status: nil, busy: false), "no status read yet")
        #expect(!SyncTitles.canPush(status: nil, busy: false))
        #expect(!SyncTitles.canPull(status: noUpstream, busy: false))
        #expect(!SyncTitles.canPush(status: noUpstream, busy: false))
        #expect(!SyncTitles.canPull(status: upstream, busy: true))
        #expect(!SyncTitles.canPush(status: upstream, busy: true))
        #expect(SyncTitles.canPull(status: upstream, busy: false))
        #expect(SyncTitles.canPush(status: upstream, busy: false))
    }

    @Test func theIdleTooltipReadsTheLastPreflight() {
        #expect(SyncTitles.idleSyncTooltip(status: nil, error: nil) == "Sync")
        #expect(
            SyncTitles.idleSyncTooltip(status: StubSyncPerformer.upstream(), error: nil)
                == "Sync — up to date")
        #expect(
            SyncTitles.idleSyncTooltip(
                status: StubSyncPerformer.upstream(ahead: 2, behind: 3), error: nil)
                == "Sync — 2 to push, 3 to pull")
        #expect(
            SyncTitles.idleSyncTooltip(status: nil, error: "HEAD is detached")
                == "Sync — couldn't read upstream status: HEAD is detached")
    }

    @Test func backToDefaultNamesTheBranchOnceItIsKnown() {
        #expect(SyncTitles.backToDefault(nil) == "Back to the Default Branch")
        #expect(SyncTitles.backToDefault("master") == "Back to master")
    }
}

@Suite struct ReconcileCommandTests {
    @Test func aPlainPathStaysBare() {
        #expect(
            ReconcileCommand.rebase(workDir: "/Users/me/WorkSpace/rc-git")
                == "cd /Users/me/WorkSpace/rc-git && git pull --rebase")
    }

    @Test func aPathWithASpaceIsQuoted() {
        #expect(
            ReconcileCommand.rebase(workDir: "/Users/me/My Repos/app")
                == "cd '/Users/me/My Repos/app' && git pull --rebase")
    }
}
