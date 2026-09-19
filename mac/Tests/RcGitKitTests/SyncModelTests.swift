import Foundation
import Testing

@testable import RcGitKit

/// A scripted performer: each step returns its queued results in order, or throws.
private final class StubSyncPerformer: SyncPerforming, @unchecked Sendable {
    struct Script {
        var preflights: [RemoteStatus]
        var fetches: [Result<FetchOutcome, SyncError>] = []
        var pulls: [Result<PullOutcome, SyncError>] = []
        var pushes: [Result<PushOutcome, SyncError>] = []
    }

    private let lock = NSLock()
    private var script: Script
    private(set) var fetchCredentials: [Credentials?] = []
    private(set) var fetchHostKeys: [String?] = []
    private(set) var pushCredentials: [Credentials?] = []
    private(set) var pullCalls = 0
    private(set) var pushCalls = 0

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
            hasUpstream: true
        )
    }

    func preflight(repoPath: String) throws -> RemoteStatus {
        lock.lock()
        defer { lock.unlock() }
        if script.preflights.count > 1 {
            return script.preflights.removeFirst()
        }
        guard let status = script.preflights.first else {
            throw SyncError.failure(message: "no preflight scripted")
        }
        return status
    }

    func fetch(repoPath: String, credentials: Credentials?, acceptedHostKey: String?) throws
        -> FetchOutcome
    {
        lock.lock()
        defer { lock.unlock() }
        fetchCredentials.append(credentials)
        fetchHostKeys.append(acceptedHostKey)
        guard !script.fetches.isEmpty else {
            throw SyncError.failure(message: "unexpected fetch")
        }
        return try script.fetches.removeFirst().get()
    }

    func pullFastForward(repoPath: String) throws -> PullOutcome {
        lock.lock()
        defer { lock.unlock() }
        pullCalls += 1
        guard !script.pulls.isEmpty else {
            throw SyncError.failure(message: "unexpected pull")
        }
        return try script.pulls.removeFirst().get()
    }

    func push(repoPath: String, credentials: Credentials?, acceptedHostKey: String?) throws
        -> PushOutcome
    {
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

    var stored: [String: String] {
        lock.lock()
        defer { lock.unlock() }
        return items
    }
}

private let cleanFetch = FetchOutcome(updatedRefs: 1, ahead: 0, behind: 0)
private let cleanPull = PullOutcome(
    newHead: nil, commitsAdvanced: false, filesChanged: 2, alreadyUpToDate: false)
private let cleanPush = PushOutcome(pushedCommits: 1, alreadyUpToDate: false)

@MainActor
@Suite struct SyncModelTests {
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
        model.onCredentialsNeeded = { host, _, resume in
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
        model.onCredentialsNeeded = { _, _, resume in
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
        model.onCredentialsNeeded = { _, _, resume in resume(nil) }
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
            ahead: 0, behind: 0, hasUpstream: false)
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
                fetches: [.success(FetchOutcome(updatedRefs: 0, ahead: 0, behind: 0))],
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
            url: "ssh://git@\(host)/repo.git",
            host: host,
            branch: "main",
            ahead: 0,
            behind: 0,
            hasUpstream: true
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

    @Test func sshPassphrasePromptRetriesAndStoresUnderTheSshKind() async {
        let store = InMemoryCredentialStore()
        let (model, performer, _) = model(
            .init(
                preflights: [Self.sshUpstream()],
                fetches: [
                    .failure(.authRequired(message: "key is encrypted")),
                    .success(cleanFetch),
                ]
            ), store: store)
        var promptedKind: CredentialKind?
        model.onCredentialsNeeded = { _, kind, resume in
            promptedKind = kind
            resume(Credentials(username: "", token: "opensesame"))
        }
        await model.refreshStatus()
        await model.fetch()

        #expect(promptedKind == .sshPassphrase)
        if case .done = model.state {
        } else {
            Issue.record("expected done, got \(model.state)")
        }
        #expect(performer.fetchCredentials.last??.token == "opensesame")
        #expect(store.stored["git.example.com#ssh"]?.token == "opensesame")
        #expect(store.stored["git.example.com"] == nil)
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
}
