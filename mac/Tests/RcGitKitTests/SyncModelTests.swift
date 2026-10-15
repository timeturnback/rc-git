import Foundation
import Testing

@testable import RcGitKit

/// A scripted performer: each step returns its queued results in order, or throws.
private final class StubSyncPerformer: SyncPerforming, @unchecked Sendable {
    struct Script {
        /// One answer per preflight, the last one repeating; `nil` makes that preflight throw.
        var preflights: [RemoteStatus?]
        var defaultBranch: DefaultBranch? = DefaultBranch(name: "main")
        var defaultBranchFails = false
        var fetches: [Result<FetchOutcome, SyncError>] = []
        var pulls: [Result<PullOutcome, SyncError>] = []
        var checkouts: [Result<CheckoutOutcome, SyncError>] = []
        var pushes: [Result<PushOutcome, SyncError>] = []
    }

    private let lock = NSLock()
    private var script: Script
    private(set) var fetchCredentials: [Credentials?] = []
    private(set) var fetchHosts: [String] = []
    private(set) var fetchHostKeys: [String?] = []
    private(set) var pushCredentials: [Credentials?] = []
    private(set) var checkoutBranches: [String] = []
    private(set) var pullCalls = 0
    private(set) var pushCalls = 0
    private(set) var preflightCalls = 0
    private(set) var defaultBranchCalls = 0
    /// Holds the next preflight at the gate until the test opens it; consumed by that call.
    var preflightGate: CallGate?
    /// Called on the step's own thread, with that step's token, before the step
    /// answers. A test uses it to cancel exactly while the transfer is in flight.
    var duringStep: (@Sendable (CancelToken) -> Void)?
    /// Called on the step's own thread before a checkout or a pull answers. Those steps take
    /// no token, so a test cancels through the model while this holds the step open.
    var duringLocalStep: (@Sendable () -> Void)?

    init(script: Script) {
        self.script = script
    }

    static func upstream(
        host: String = "example.com", remote: String = "origin", ahead: Int = 0, behind: Int = 0,
        published: Bool = true
    ) -> RemoteStatus {
        RemoteStatus(
            remote: remote,
            url: "https://\(host)/repo.git",
            host: host,
            branch: "main",
            ahead: ahead,
            behind: behind,
            hasUpstream: true,
            published: published,
            usesSsh: false
        )
    }

    func preflight(repoPath: String) throws -> RemoteStatus {
        lock.lock()
        preflightCalls += 1
        let next: RemoteStatus?? =
            script.preflights.count > 1
            ? script.preflights.removeFirst() : script.preflights.first
        let gate = preflightGate
        preflightGate = nil
        lock.unlock()
        gate?.pass()
        guard let next else {
            throw SyncError.failure(message: "no preflight scripted")
        }
        guard let status = next else {
            throw SyncError.failure(message: "preflight failed")
        }
        return status
    }

    func defaultBranch(repoPath: String) throws -> DefaultBranch? {
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
        repoPath: String, host: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> FetchOutcome {
        duringStep?(cancel)
        lock.lock()
        defer { lock.unlock() }
        fetchCredentials.append(credentials)
        fetchHosts.append(host)
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
        repoPath: String, host: String, credentials: Credentials?, acceptedHostKey: String?,
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
    private var queries: [String] = []
    private var writes = 0
    private let failsReads: Bool
    private let failsWrites: Bool

    init(
        _ items: [String: Credentials] = [:], failsReads: Bool = false, failsWrites: Bool = false
    ) {
        self.items = items
        self.failsReads = failsReads
        self.failsWrites = failsWrites
    }

    /// Every host `credentials(host:kind:)` was asked for, in order.
    var queriedHosts: [String] {
        lock.lock()
        defer { lock.unlock() }
        return queries
    }

    private func key(_ host: String, _ kind: CredentialKind) -> String {
        kind == .httpsToken ? host : "\(host)#ssh"
    }

    func credentials(host: String, kind: CredentialKind) throws -> Credentials? {
        lock.lock()
        defer { lock.unlock() }
        queries.append(host)
        if failsReads {
            throw RepositoryError("keychain read failed for '\(host)' (status -25293)")
        }
        return items[key(host, kind)]
    }

    func store(_ credentials: Credentials, host: String, kind: CredentialKind) throws {
        lock.lock()
        defer { lock.unlock() }
        writes += 1
        if failsWrites {
            throw RepositoryError("keychain write failed for '\(host)' (status -25293)")
        }
        items[key(host, kind)] = credentials
    }

    /// How many times `store(_:host:kind:)` was called, failed writes included.
    var writeCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return writes
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

    func approvals() -> [HostKeyApproval] {
        lock.lock()
        defer { lock.unlock() }
        return items.map { HostKeyApproval(host: $0.key, fingerprint: $0.value) }
    }

    var stored: [String: String] {
        lock.lock()
        defer { lock.unlock() }
        return items
    }
}

/// What a step saw when the test stopped it through the model, read after the workflow ends.
private final class CancelObservation: @unchecked Sendable {
    private let lock = NSLock()
    private var stopOffered = false
    private var signalled = false

    func record(offeredStop: Bool) {
        lock.lock()
        defer { lock.unlock() }
        stopOffered = offeredStop
    }

    func record(tokenSignalled: Bool) {
        lock.lock()
        defer { lock.unlock() }
        signalled = tokenSignalled
    }

    var offeredStop: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopOffered
    }

    var tokenSignalled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return signalled
    }
}

private let cleanFetch = FetchOutcome(updatedRefs: 1)
private let cleanPull = PullOutcome(filesChanged: 2)
private let cleanPush = PushOutcome(pushedCommits: 1)
private let switchedToMain = CheckoutOutcome(
    branch: "main", newHead: oid(0), filesChanged: 4,
    alreadyOnBranch: false, reflogWritten: true)
private let alreadyOnMain = CheckoutOutcome(
    branch: "main", newHead: oid(0), filesChanged: 0,
    alreadyOnBranch: true, reflogWritten: true)
private let switchedWithoutReflog = CheckoutOutcome(
    branch: "main", newHead: oid(0), filesChanged: 4,
    alreadyOnBranch: false, reflogWritten: false)

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

    @Test func authRetryPromptsStoresOnlyOnSuccess() async throws {
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
            resume(.provided(Credentials(username: "user", token: "sekret")))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedHost == "git.example.com")
        if case .done = model.state {
        } else {
            Issue.record("expected done, got \(model.state)")
        }
        let first = try #require(performer.fetchCredentials.first)
        #expect(first == nil, "the first attempt is anonymous")
        #expect(performer.fetchCredentials.last??.username == "user")
        #expect(store.stored["git.example.com"]?.token == "sekret")
    }

    @Test func aSyncReadsTheStoreOnce() async {
        let saved = Credentials(username: "user", token: "saved")
        let store = InMemoryCredentialStore(["example.com": saved])
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ), store: store)
        await model.refreshStatus()
        await model.sync()

        #expect(store.queriedHosts == ["example.com"])
        #expect(performer.fetchCredentials.map { $0?.token } == ["saved"])
        #expect(performer.pushCredentials.map { $0?.token } == ["saved"])
    }

    @Test func storedCredentialsAreTriedFirstWithoutAPrompt() async {
        let saved = Credentials(username: "u", token: "t")
        let store = InMemoryCredentialStore(["git.example.com": saved])
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(host: "git.example.com")],
                fetches: [.success(cleanFetch)]
            ), store: store)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.cancelled)
        }
        await model.refreshStatus()
        await model.fetch()

        guard case let .done(summary) = model.state else {
            Issue.record("expected done, got \(model.state)")
            return
        }
        #expect(summary.fetchedRefs == 1)
        #expect(performer.fetchCredentials.first??.token == "t")
        #expect(prompts == 0)
        #expect(store.writeCount == 0, "credentials nobody typed are never written back")
    }

    @Test func storedSshPassphraseIsTriedFirstAndNeverTheHttpsToken() async {
        let https = Credentials(username: "u", token: "https-token")
        let ssh = Credentials(username: "", token: "passphrase")
        let store = InMemoryCredentialStore([
            "git.example.com": https, "git.example.com#ssh": ssh,
        ])
        let (model, performer, _) = model(
            .init(preflights: [Self.sshUpstream()], fetches: [.success(cleanFetch)]),
            store: store)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.cancelled)
        }
        await model.refreshStatus()
        await model.fetch()

        guard case let .done(summary) = model.state else {
            Issue.record("expected done, got \(model.state)")
            return
        }
        #expect(summary.fetchedRefs == 1)
        #expect(performer.fetchCredentials.map { $0?.token } == ["passphrase"])
        #expect(prompts == 0)
        #expect(store.writeCount == 0)
    }

    @Test func aPromptedTokenCarriesFromTheFetchToThePushOfOneSync() async {
        let store = InMemoryCredentialStore()
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401")), .success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ), store: store)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.provided(Credentials(username: "user", token: "typed")))
        }
        await model.refreshStatus()
        await model.sync()

        #expect(prompts == 1)
        #expect(store.queriedHosts.count == 1)
        #expect(performer.pushCredentials.map { $0?.token } == ["typed"])
    }

    @Test func aFailedStoreReadStillPromptsAndTheFailureIsReported() async throws {
        let store = InMemoryCredentialStore(failsReads: true)
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401")), .success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ), store: store)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.provided(Credentials(username: "user", token: "typed")))
        }
        await model.refreshStatus()
        await model.sync()

        #expect(prompts == 1)
        let first = try #require(performer.fetchCredentials.first)
        #expect(first == nil, "falls back to anonymous")
        #expect(performer.pushCredentials.map { $0?.token } == ["typed"])
        guard case let .done(summary) = model.state else {
            Issue.record("expected done, got \(model.state)")
            return
        }
        #expect(summary.pushedCommits == 1)
        #expect(summary.notice?.contains("could not be read") == true)
        #expect(summary.notice?.contains("status -25293") == true)
        #expect(summary.notice?.contains("typed") == false, "never the token")
    }

    @Test func aFailedStoreReadIsNamedInTheFailure() async {
        let store = InMemoryCredentialStore(failsReads: true)
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401"))]
            ), store: store)
        model.onCredentialsNeeded = { _, _, _, resume in resume(.cancelled) }
        await model.refreshStatus()
        await model.fetch()

        guard case let .failed(_, message) = model.state else {
            Issue.record("expected failed, got \(model.state)")
            return
        }
        #expect(message.contains("authentication canceled"))
        #expect(message.contains("keychain read failed for 'example.com'"))
    }

    @Test func aFailedStoreWriteKeepsTheStepAndSaysTheTokenWasNotSaved() async {
        let store = InMemoryCredentialStore(failsWrites: true)
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401")), .success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ), store: store)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.provided(Credentials(username: "user", token: "typed")))
        }
        await model.refreshStatus()
        await model.sync()

        #expect(prompts == 1)
        #expect(performer.pushCredentials.map { $0?.token } == ["typed"])
        #expect(store.stored.isEmpty)
        guard case let .done(summary) = model.state else {
            Issue.record("expected done, got \(model.state)")
            return
        }
        #expect(summary.fetchedRefs == 1 && summary.pushedCommits == 1)
        #expect(
            summary.notice
                == "signed in, but the token was not saved: "
                + "keychain write failed for 'example.com' (status -25293)")
    }

    @Test func eachWorkflowReadsTheStoreAfresh() async {
        let store = InMemoryCredentialStore()
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch), .success(cleanFetch)]
            ), store: store)
        await model.refreshStatus()
        await model.fetch()
        model.reset()
        await model.fetch()

        #expect(store.queriedHosts.count == 2)
    }

    @Test func fetchWithNoPriorRefreshKeysCredentialsOnTheRealHost() async {
        let saved = Credentials(username: "user", token: "saved")
        let store = InMemoryCredentialStore(["git.example.com": saved])
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(host: "git.example.com")],
                fetches: [.success(cleanFetch)]
            ), store: store)
        await model.fetch()

        #expect(
            model.state == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 0, pushedCommits: 0)))
        #expect(store.queriedHosts == ["git.example.com"])
        #expect(performer.fetchHosts == ["git.example.com"])
        #expect(performer.fetchCredentials.first??.token == "saved")
    }

    @Test func anEmptyHostNeverReadsStoresOrPromptsForSecrets() async throws {
        let poisoned = Credentials(username: "user", token: "kept-under-empty-host")
        let store = InMemoryCredentialStore(["": poisoned])
        let hostKeys = InMemoryHostKeyStore(["": "SHA256:poisoned"])
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(host: "")],
                fetches: [
                    .failure(.authRequired(message: "401")),
                    .success(cleanFetch),
                ]
            ), store: store, hostKeys: hostKeys)
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.provided(Credentials(username: "user", token: "typed")))
        }
        model.onHostKeyNeeded = { _, _, resume in
            prompts += 1
            resume(.trusted)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(
            model.state
                == .failed(step: .fetch, message: "cannot tell which host the upstream is on"))
        #expect(prompts == 0)
        #expect(store.queriedHosts.isEmpty)
        #expect(store.stored.count == 1, "nothing new is stored")
        #expect(performer.fetchCredentials.count == 1, "one attempt, never retried")
        let sent = try #require(performer.fetchCredentials.first)
        #expect(sent == nil, "the poisoned item is never sent")
        #expect(performer.fetchHostKeys == [nil])
        #expect(hostKeys.stored == ["": "SHA256:poisoned"])
    }

    @Test func aLocalUpstreamStillPullsWithoutTouchingTheStore() async throws {
        let store = InMemoryCredentialStore()
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(host: "", remote: ".")],
                fetches: [.success(FetchOutcome(updatedRefs: 0))],
                pulls: [.success(cleanPull)]
            ), store: store)
        await model.pull()

        #expect(
            model.state == .done(SyncSummary(fetchedRefs: 0, pulledFiles: 2, pushedCommits: 0)))
        #expect(performer.fetchCredentials.count == 1)
        let sent = try #require(performer.fetchCredentials.first)
        #expect(sent == nil)
        #expect(performer.pullCalls == 1)
        #expect(store.queriedHosts.isEmpty)
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
            resume(.provided(Credentials(username: "user", token: "wrong")))
        }
        await model.refreshStatus()
        await model.fetch()

        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message == "401 (gave up after 3 attempts)", "the guard's reason is kept")
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
        model.onCredentialsNeeded = { _, _, _, resume in resume(.cancelled) }
        await model.refreshStatus()
        await model.fetch()

        if case let .failed(_, message) = model.state {
            #expect(message.contains("canceled"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(store.stored.isEmpty)
    }

    @Test func aLockedKeyThatNeverUnlocksNamesTheKeyNotTheHost() async {
        let locked = SyncError.passphraseRequired(
            message: "the key '/k/id' for 'git.example.com' is encrypted; enter its passphrase",
            key: "/k/id")
        let (model, _, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [.failure(locked), .failure(locked), .failure(locked)]
            ))
        model.onCredentialsNeeded = { _, _, _, resume in
            resume(.provided(Credentials(username: "", token: "wrong")))
        }
        await model.refreshStatus()
        await model.fetch()

        guard case let .failed(_, message) = model.state else {
            Issue.record("expected failed, got \(model.state)")
            return
        }
        #expect(message.contains("the key '/k/id'"))
        #expect(message.contains("gave up after 3 attempts"))
        #expect(!message.contains("rejecting"))
    }

    @Test func noPromptHandlerSaysNoneAreStoredRatherThanCanceled() async {
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401"))]
            ))
        await model.refreshStatus()
        await model.fetch()

        guard case let .failed(_, message) = model.state else {
            Issue.record("expected failed, got \(model.state)")
            return
        }
        #expect(message == "credentials required for 'example.com' and none are stored: 401")
    }

    @Test func anUnavailablePromptAfterStoredCredentialsKeepsTheReason() async {
        let saved = Credentials(username: "user", token: "stale")
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401"))]
            ), store: InMemoryCredentialStore(["example.com": saved]))
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.unavailable)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(prompts == 1)
        guard case let .failed(_, message) = model.state else {
            Issue.record("expected failed, got \(model.state)")
            return
        }
        #expect(message == "401; no prompt is available to ask again")
    }

    @Test func noHostKeyHandlerIsNotReportedAsARefusal() async {
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
        await model.refreshStatus()
        await model.fetch()

        guard case let .failed(_, message) = model.state else {
            Issue.record("expected failed, got \(model.state)")
            return
        }
        #expect(message == "not a known host; no prompt is available to approve it")
        #expect(hostKeys.stored.isEmpty)
    }

    @Test func theCredentialSheetRefusesEmptyFields() {
        func problem(_ entry: CredentialEntry) -> String? {
            if case let .invalid(missing) = entry { return missing }
            return nil
        }
        #expect(
            problem(CredentialEntry.check(kind: .httpsToken, username: " ", secret: "t"))
                == "Enter a username.")
        #expect(
            problem(CredentialEntry.check(kind: .httpsToken, username: "u", secret: ""))
                == "Enter a token.")
        #expect(
            problem(CredentialEntry.check(kind: .sshPassphrase, username: "", secret: ""))
                == "Enter the passphrase.")
        guard
            case let .valid(https) = CredentialEntry.check(
                kind: .httpsToken, username: " u ", secret: "t")
        else {
            Issue.record("a username and token are valid")
            return
        }
        #expect(https.username == "u")
        #expect(https.token == "t")
        guard
            case let .valid(ssh) = CredentialEntry.check(
                kind: .sshPassphrase, username: "", secret: "p")
        else {
            Issue.record("a passphrase alone is valid over ssh")
            return
        }
        #expect(ssh.token == "p")
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

    @Test func pushAloneRunsNoFetchOrPull() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(ahead: 1)],
                pushes: [.success(cleanPush)]
            ))
        await model.push()

        #expect(performer.fetchCredentials.isEmpty, "push must not fetch first")
        #expect(performer.pullCalls == 0)
        #expect(performer.pushCalls == 1)
        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 0, pulledFiles: 0, pushedCommits: 1)))
    }

    @Test func pullFetchesThenFastForwardsAndNeverPushes() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(behind: 1)],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)]
            ))
        await model.pull()

        #expect(performer.fetchCredentials.count == 1)
        #expect(performer.pullCalls == 1)
        #expect(performer.pushCalls == 0)
        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 0)))
    }

    @Test func aRejectedPushEndsDivergedNotFailed() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(ahead: 1)],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.failure(.diverged(message: "push rejected: not a fast-forward"))]
            ))
        await model.sync()

        #expect(performer.pushCalls == 1)
        #expect(model.state == .diverged(message: "push rejected: not a fast-forward"))
    }

    @Test func noUpstreamFailsFastWithoutNetwork() async {
        let status = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, published: false, usesSsh: false)
        let (model, performer, _) = model(.init(preflights: [status]))
        await model.refreshStatus()
        await model.sync()

        if case let .failed(_, message) = model.state {
            #expect(
                message
                    == "branch 'main' has no remote to use; add an 'origin' remote or set "
                    + "branch.main.remote")
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

        #expect(
            performer.fetchCredentials.count == 1, "the overlapping sync never reached the network")
        #expect(performer.pushCalls == 1)
        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 1)))
    }

    @Test func upToDateSyncReportsZeros() async {
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(FetchOutcome(updatedRefs: 0))],
                pulls: [.success(PullOutcome(filesChanged: 0))],
                pushes: [.success(PushOutcome(pushedCommits: 0))]
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
            published: true,
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
            resume(.trusted)
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

    @Test func aHostKeyStillUnknownAfterTrustFailsWithoutASecondPrompt() async {
        let unknown = SyncError.hostKeyUnknown(
            message: "still not a known host", hostKey: "ssh-ed25519 SHA256:abc123")
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [.failure(unknown), .failure(unknown)]))
        var prompts = 0
        model.onHostKeyNeeded = { _, _, resume in
            prompts += 1
            resume(.trusted)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(prompts == 1, "a key the guard still rejects after trust must not ask again")
        #expect(performer.fetchHostKeys == [nil, "SHA256:abc123"])
        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message == "still not a known host")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
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
        model.onHostKeyNeeded = { _, _, resume in resume(.refused) }
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
            resume(.trusted)
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
            resume(.trusted)
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

    @Test func aRemovedApprovalMakesTheRotatedKeyUnknownAgain() async {
        let hostKeys = InMemoryHostKeyStore(["git.example.com": "SHA256:abc123"])
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(
                        .hostKeyUnknown(
                            message: "not a known host",
                            hostKey: "ssh-ed25519 SHA256:rotated")),
                    .success(cleanFetch),
                ]
            ), hostKeys: hostKeys)
        let settings = ApprovedHostsModel(store: hostKeys)
        settings.remove(at: [0])
        #expect(settings.rows.isEmpty)
        var promptedKey: String?
        model.onHostKeyNeeded = { _, hostKey, resume in
            promptedKey = hostKey
            resume(.trusted)
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedKey == "ssh-ed25519 SHA256:rotated", "a forgotten host is first contact")
        if case .done = model.state {
        } else {
            Issue.record("expected done, got \(model.state)")
        }
        #expect(performer.fetchHostKeys == [nil, "SHA256:rotated"])
        #expect(hostKeys.stored["git.example.com"] == "SHA256:rotated")
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
            resume(.provided(Credentials(username: "", token: "opensesame")))
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
            resume(.provided(Credentials(username: "", token: "opensesame")))
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

    /// A redirect to another host that asks for credentials gets none from rc-git, so asking
    /// the user again cannot help: the step fails at once with the guard's message.
    @Test func aRefusedCredentialHostFailsAtOnceWithoutAPrompt() async {
        let refusal =
            "fetch: 'example.com' redirected to 'login.example.net', which asked for credentials"
        let (model, performer, store) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [
                    .failure(.credentialHostRefused(message: refusal)),
                    .success(cleanFetch),
                ]
            ))
        var prompts = 0
        model.onCredentialsNeeded = { _, _, _, resume in
            prompts += 1
            resume(.provided(Credentials(username: "me", token: "typed")))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(prompts == 0, "no credentials can satisfy another host")
        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message == refusal)
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.count == 1, "no retry")
        #expect(store.stored.isEmpty)
        let refused = SyncError.credentialHostRefused(message: "m")
        #expect(SyncModel.credentialRetry(after: refused, kind: .httpsToken) == .none)
        #expect(SyncModel.credentialRetry(after: refused, kind: .sshPassphrase) == .none)
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
            branch: "main", ahead: 0, behind: 0, hasUpstream: true, published: true,
            usesSsh: true)
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
            resume(.provided(Credentials(username: "", token: "opensesame")))
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
        // The real transfer sees the flag and fails; the stub stands in for that by raising
        // the token directly. The toolbar goes through `cancelRunning()` on the main actor,
        // which `cancelRunningMidFetchStopsTheWorkflow` covers.
        performer.duringStep = { cancel in cancel.signal() }
        await model.refreshStatus()
        await model.sync()

        #expect(model.state == .idle, "a cancel is not a failure to report")
        #expect(performer.pullCalls == 0, "a cancelled fetch must stop the workflow")
        #expect(performer.pushCalls == 0)
        #expect(!model.isBusy)
    }

    @Test func aCancelDuringAFetchThatSucceedsStillSkipsThePull() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(behind: 1)],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)]
            ))
        // The stop lands after the transfer's last poll, so the fetch still succeeds.
        performer.duringStep = { cancel in cancel.signal() }
        await model.refreshStatus()
        await model.pull()

        #expect(model.state == .idle)
        #expect(performer.pullCalls == 0, "the user said stop before the working tree moved")
    }

    @Test func aCancelledWorkflowNeverOffersStopAgain() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(behind: 1)],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)]
            ))
        await model.refreshStatus()
        var states: [SyncState] = []
        model.onStateChanged = {
            states.append(model.state)
            if states.count == 1, model.state == .running(.fetch) {
                model.cancelRunning()
            }
        }
        await model.pull()

        let afterCancel = states.drop { $0 != .cancelling(.fetch) }
        #expect(!afterCancel.isEmpty, "the cancel must land: \(states)")
        #expect(
            !afterCancel.contains { if case .running = $0 { true } else { false } },
            "a step entered after Stop must stay cancelling: \(states)")
        #expect(model.state == .idle)
        #expect(performer.fetchCredentials.isEmpty)
        #expect(performer.pullCalls == 0)
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

        // A cancel while idle is refused, so it leaves nothing behind for the next sync.
        await model.sync()
        #expect(
            model.state
                == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 1)))
        #expect(performer.pushCalls == 1)
    }

    @Test func cancelRunningMidFetchStopsTheWorkflow() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream(ahead: 1)],
                fetches: [.failure(.failure(message: "fetch: cancelled"))],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        var states: [SyncState] = []
        model.onStateChanged = { states.append(model.state) }
        let seen = CancelObservation()
        // Stop the way the toolbar does: through the model on the main actor, which is free
        // while the model awaits the step. The step's thread waits for the hop to finish.
        performer.duringStep = { cancel in
            let hopped = DispatchSemaphore(value: 0)
            Task { @MainActor in
                seen.record(offeredStop: model.isCancellable)
                model.cancelRunning()
                hopped.signal()
            }
            hopped.wait()
            seen.record(tokenSignalled: cancel.isSignalled)
        }
        await model.refreshStatus()
        await model.sync()

        #expect(seen.offeredStop, "a running fetch must offer Stop")
        #expect(seen.tokenSignalled, "cancelRunning() must raise the running step's token")
        #expect(states.contains(.cancelling(.fetch)), "\(states)")
        #expect(model.state == .idle)
        #expect(performer.pullCalls == 0, "a cancelled fetch must stop the workflow")
        #expect(performer.pushCalls == 0)
        #expect(!model.isBusy)
    }

    @Test func aCancelWhileTheCredentialSheetIsUpStartsNoSecondAttempt() async {
        let (model, performer, store) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.authRequired(message: "401")), .success(cleanFetch)]
            ))
        model.onCredentialsNeeded = { _, _, _, resume in
            model.cancelRunning()
            resume(.provided(Credentials(username: "user", token: "typed")))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(performer.fetchCredentials.count == 1, "the answer after Stop is not tried")
        #expect(model.state == .idle)
        #expect(store.stored.isEmpty, "credentials never tried are never stored")
    }

    @Test func aCancelledWorkflowLeavesTheNextOneClean() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [
                    .failure(.failure(message: "fetch: cancelled")), .success(cleanFetch),
                ],
                pulls: [.success(cleanPull)],
                pushes: [.success(cleanPush)]
            ))
        performer.duringStep = { cancel in cancel.signal() }
        await model.refreshStatus()
        await model.sync()
        #expect(model.state == .idle)

        // The flag a cancel raises belongs to one workflow: the next one starts clean.
        performer.duringStep = nil
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
            resume(.provided(Credentials(username: "user", token: "token")))
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
                        fetchedRefs: 0, pulledFiles: 4, pushedCommits: 0,
                        cancelledAfter: .checkout)))
        #expect(
            model.takeSwitchedBranch() == "main",
            "the switch landed, so the browser must still follow it")
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
                        fetchedRefs: 1, pulledFiles: 6, pushedCommits: 0)),
            "the files the switch wrote and the ones the pull wrote both count"
        )
        #expect(model.takeSwitchedBranch() == "main")
        #expect(model.takeSwitchedBranch() == nil, "a switch is followed once")
    }

    @Test func aSwitchThatMissedItsReflogLineFinishesWithItsWarning() async throws {
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                checkouts: [.success(switchedWithoutReflog)]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        let warning = try #require(switchedWithoutReflog.warning)
        #expect(warning.contains("git checkout -"))
        #expect(
            model.state
                == .done(
                    SyncSummary(
                        fetchedRefs: 1, pulledFiles: 6, pushedCommits: 0, notice: warning)),
            "the switch landed, so the workflow finishes and carries the warning")
        #expect(model.takeSwitchedBranch() == "main")
    }

    @Test func aSwitchWithItsReflogLineHasNoWarning() {
        #expect(switchedToMain.warning == nil)
        #expect(alreadyOnMain.warning == nil)
    }

    @Test func aPushThatLeftItsTrackingRefFinishesWithTheWarning() async {
        let stuck = PushOutcome(
            pushedCommits: 1,
            warning: "push landed, but the tracking ref was not moved: lock held")
        let (model, _, _) = model(
            .init(preflights: [StubSyncPerformer.upstream(ahead: 1)], pushes: [.success(stuck)]))
        await model.refreshStatus()
        await model.push()

        #expect(
            model.state
                == .done(
                    SyncSummary(
                        fetchedRefs: 0, pulledFiles: 0, pushedCommits: 1, notice: stuck.warning)),
            "the push landed, so it is done, with the stale tracking ref as a notice")
    }

    @Test func aSyncWhosePushLeftItsTrackingRefFinishesWithTheWarning() async {
        let stuck = PushOutcome(pushedCommits: 1, warning: "tracking ref was not moved")
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.success(cleanFetch)],
                pulls: [.success(cleanPull)],
                pushes: [.success(stuck)]
            ))
        await model.refreshStatus()
        await model.sync()

        #expect(
            model.state
                == .done(
                    SyncSummary(
                        fetchedRefs: 1, pulledFiles: 2, pushedCommits: 1, notice: stuck.warning)))
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
        #expect(model.takeSwitchedBranch() == nil)
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
            ahead: 0, behind: 0, hasUpstream: false, published: false, usesSsh: false)
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
            #expect(
                message
                    == "branch 'main' has no remote to use; add an 'origin' remote or set "
                    + "branch.main.remote")
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.fetchCredentials.isEmpty, "no network step may run")
        #expect(
            model.takeSwitchedBranch() == "main",
            "HEAD moved before the failure, so the browser must still follow it")
    }

    @Test func aFetchThatFailsAfterTheSwitchStillReportsTheSwitch() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.failure(message: "offline"))],
                checkouts: [.success(switchedToMain)]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()

        if case let .failed(step, message) = model.state {
            #expect(step == .fetch)
            #expect(message.contains("offline"))
        } else {
            Issue.record("expected failed, got \(model.state)")
        }
        #expect(performer.pullCalls == 0)
        #expect(model.takeSwitchedBranch() == "main")
    }

    @Test func aNewWorkflowForgetsASwitchNobodyFollowed() async {
        let (model, _, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()],
                fetches: [.failure(.failure(message: "offline")), .success(cleanFetch)],
                checkouts: [.success(switchedToMain)]
            ))
        await model.refreshStatus()
        await model.backToDefaultBranch()
        await model.fetch()

        #expect(
            model.state == .done(SyncSummary(fetchedRefs: 1, pulledFiles: 0, pushedCommits: 0)))
        #expect(model.takeSwitchedBranch() == nil, "the fetch switched nothing")
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
        let availability = SyncAvailability(
            status: model.remoteStatus, busy: false, cancellable: false, hasRepository: true,
            hasWorkingTree: true)
        #expect(!availability.canPull)
        #expect(!availability.canPush)
        #expect(!availability.canSync)
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
        let (model, performer, _) = model(
            .init(
                preflights: [
                    StubSyncPerformer.upstream(ahead: 1), StubSyncPerformer.upstream(ahead: 2),
                ],
                fetches: [.success(cleanFetch)]
            ))
        let gate = CallGate()
        performer.preflightGate = gate
        var seen: [Int?] = []
        model.onStateChanged = { seen.append(model.remoteStatus?.ahead) }
        let slow = Task { await model.refreshStatus() }
        await gate.waitUntilArrived()
        await model.fetch()
        gate.open()
        await slow.value

        #expect(model.remoteStatus?.ahead == 2)
        #expect(!seen.contains(1), "the older answer never lands: \(seen)")
    }

    @Test func aBurstOfRefreshesCostsTwoPreflightsAndOneMainlineRead() async {
        let (model, performer, _) = model(
            .init(
                preflights: [StubSyncPerformer.upstream()], defaultBranch: nil))
        let gate = CallGate()
        performer.preflightGate = gate
        let slow = Task { await model.refreshStatus() }
        await gate.waitUntilArrived()
        let burst = (0..<20).map { _ in Task { await model.refreshStatus() } }
        for refresh in burst { await refresh.value }
        gate.open()
        await slow.value
        #expect(performer.preflightCalls == 2, "the burst joins one more read")

        await model.refreshStatus()
        await model.refreshStatus()
        #expect(
            performer.defaultBranchCalls == 1,
            "a repository that names no mainline is not asked again")
        #expect(model.defaultBranch == nil)
    }

    @Test func anInexactMainlineIsRefusedBeforeTheSwitch() async {
        for refreshFirst in [true, false] {
            let (model, performer, _) = model(
                .init(
                    preflights: [StubSyncPerformer.upstream()],
                    defaultBranch: DefaultBranch(name: "tr\u{FFFD}", hasExactName: false)))
            if refreshFirst {
                await model.refreshStatus()
                #expect(model.defaultBranch == "tr\u{FFFD}", "the name still titles the control")
            }
            await model.backToDefaultBranch()

            if case let .failed(step, message) = model.state {
                #expect(step == .checkout)
                #expect(
                    message == "cannot switch to 'tr\u{FFFD}': the branch name is not valid UTF-8",
                    "refresh first: \(refreshFirst)")
            } else {
                Issue.record("expected failed, got \(model.state) (refresh first: \(refreshFirst))")
            }
            #expect(performer.checkoutBranches.isEmpty, "the repaired name is never checked out")
            #expect(performer.fetchCredentials.isEmpty)
        }
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
            ahead: 0, behind: 0, hasUpstream: false, published: false, usesSsh: false)
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

@Suite(.timeLimit(.minutes(1))) struct SyncTitlesTests {
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

    private func availability(
        _ status: RemoteStatus?, busy: Bool = false, cancellable: Bool = false,
        hasRepository: Bool = true, hasWorkingTree: Bool = true
    ) -> SyncAvailability {
        SyncAvailability(
            status: status, busy: busy, cancellable: cancellable, hasRepository: hasRepository,
            hasWorkingTree: hasWorkingTree)
    }

    /// Whether Fetch, Pull, Push and Sync are all enabled, or all disabled; `nil` when the
    /// four disagree, which no surface may ever see.
    private func remoteControls(_ availability: SyncAvailability) -> Bool? {
        let all = [
            availability.canFetch, availability.canPull, availability.canPush,
            availability.canSync,
        ]
        return Set(all).count == 1 ? all.first : nil
    }

    @Test func remoteControlsNeedAKnownUpstreamAndAnIdleModel() {
        let noUpstream = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, published: false, usesSsh: false)
        let upstream = StubSyncPerformer.upstream()
        #expect(remoteControls(availability(upstream)) == true)
        #expect(remoteControls(availability(nil)) == false, "no status read yet")
        #expect(
            remoteControls(availability(noUpstream)) == false,
            "fetch, pull, push and sync all refuse a branch with no upstream")
        #expect(remoteControls(availability(upstream, busy: true)) == false)
        #expect(
            remoteControls(availability(upstream, hasRepository: false)) == false,
            "an empty window has nothing to sync")
    }

    @Test func backToDefaultNeedsAnIdleCheckoutButNoUpstream() {
        let noUpstream = RemoteStatus(
            remote: "", url: "", host: "", branch: "topic",
            ahead: 0, behind: 0, hasUpstream: false, published: false, usesSsh: false)
        #expect(availability(noUpstream).canBackToDefault)
        #expect(availability(nil).canBackToDefault, "the switch needs no status read")
        #expect(!availability(nil, hasWorkingTree: false).canBackToDefault, "a bare repository")
        #expect(!availability(nil, busy: true).canBackToDefault)
        #expect(!availability(nil, hasRepository: false).canBackToDefault, "an empty window")
    }

    @Test func theSyncButtonIsCancelWhileACancellableStepRuns() {
        let upstream = StubSyncPerformer.upstream()
        let noUpstream = RemoteStatus(
            remote: "", url: "", host: "", branch: "main",
            ahead: 0, behind: 0, hasUpstream: false, published: false, usesSsh: false)
        #expect(availability(upstream).syncButtonEnabled)
        #expect(!availability(noUpstream).syncButtonEnabled, "agrees with the Sync menu item")
        #expect(!availability(nil).syncButtonEnabled)
        #expect(!availability(upstream, hasRepository: false).syncButtonEnabled)
        let fetching = availability(upstream, busy: true, cancellable: true)
        #expect(fetching.canCancel)
        #expect(fetching.syncButtonEnabled)
        #expect(!fetching.canSync)
        let pulling = availability(upstream, busy: true, cancellable: false)
        #expect(!pulling.canCancel)
        #expect(!pulling.syncButtonEnabled, "a pull or checkout runs to completion")
        #expect(!availability(upstream, cancellable: true).canCancel, "nothing is running")
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
            SyncTitles.idleSyncTooltip(
                status: StubSyncPerformer.upstream(published: false), error: nil)
                == "Sync — not published",
            "a never-pushed branch is not up to date")
        #expect(
            SyncTitles.idleSyncTooltip(status: nil, error: "HEAD is detached")
                == "Sync — couldn't read upstream status: HEAD is detached")
    }

    @Test func backToDefaultNamesTheBranchOnceItIsKnown() {
        #expect(SyncTitles.backToDefault(nil) == "Back to the Default Branch")
        #expect(SyncTitles.backToDefault("master") == "Back to master")
    }
}

@Suite(.timeLimit(.minutes(1))) struct ReconcileCommandTests {
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

@Suite(.timeLimit(.minutes(1))) struct SyncReportTests {
    @Test func aWorkflowThatRanToTheEndWithNothingToSayReportsNothing() {
        #expect(SyncSummary(fetchedRefs: 1, pulledFiles: 2, pushedCommits: 1).report == nil)
    }

    @Test func aCancelAfterAFinishedPullIsExplainedNotWarned() throws {
        let summary = SyncSummary(
            fetchedRefs: 1, pulledFiles: 2, pushedCommits: 0, cancelledAfter: .pull)
        let report = try #require(summary.report)
        #expect(!report.isProblem)
        #expect(report.title == "Sync cancelled")
        #expect(report.message.contains("pull had already finished"))
        #expect(report.message.contains("nothing was pushed"))
    }

    @Test func aCancelAfterAFinishedSwitchSaysTheSwitchStands() throws {
        let summary = SyncSummary(
            fetchedRefs: 0, pulledFiles: 4, pushedCommits: 0, cancelledAfter: .checkout)
        let report = try #require(summary.report)
        #expect(!report.isProblem)
        #expect(report.message.contains("branch switch had already finished"))
    }

    @Test func aNoticeIsAProblem() throws {
        let summary = SyncSummary(
            fetchedRefs: 1, pulledFiles: 0, pushedCommits: 0, notice: "token not saved")
        let report = try #require(summary.report)
        #expect(report.isProblem)
        #expect(report.message == "token not saved")
    }

    @Test func aNoticeAfterACancelCarriesBoth() throws {
        let summary = SyncSummary(
            fetchedRefs: 0, pulledFiles: 1, pushedCommits: 0, cancelledAfter: .checkout,
            notice: "no reflog line")
        let report = try #require(summary.report)
        #expect(report.isProblem)
        #expect(report.message.contains("branch switch had already finished"))
        #expect(report.message.hasSuffix("no reflog line"))
    }
}
