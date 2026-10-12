import Foundation

/// The seam between the sync workflow and the FFI, so tests never open a socket. The
/// network steps take the operation's `CancelToken`; it is the only way to reach a
/// thread already blocked inside a transfer.
public protocol SyncPerforming: Sendable {
    func preflight(repoPath: String) throws -> RemoteStatus
    /// The repository's mainline branch, or `nil` when nothing names one. Local and cheap.
    func defaultBranch(repoPath: String) throws -> String?
    func fetch(
        repoPath: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> FetchOutcome
    func pullFastForward(repoPath: String) throws(SyncError) -> PullOutcome
    func checkout(repoPath: String, branch: String) throws(SyncError) -> CheckoutOutcome
    func push(
        repoPath: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> PushOutcome
}

/// Production performer: forwards to `RepositoryGuard`.
public struct GuardSyncPerformer: SyncPerforming {
    public init() {}

    public func preflight(repoPath: String) throws -> RemoteStatus {
        try RepositoryGuard.remotePreflight(repoPath: repoPath)
    }

    public func defaultBranch(repoPath: String) throws -> String? {
        try Repository(path: repoPath).defaultBranch()
    }

    public func fetch(
        repoPath: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> FetchOutcome {
        try RepositoryGuard.fetch(
            repoPath: repoPath, credentials: credentials, acceptedHostKey: acceptedHostKey,
            cancel: cancel)
    }

    public func pullFastForward(repoPath: String) throws(SyncError) -> PullOutcome {
        try RepositoryGuard.pullFastForward(repoPath: repoPath)
    }

    public func checkout(repoPath: String, branch: String) throws(SyncError) -> CheckoutOutcome {
        try RepositoryGuard.checkout(repoPath: repoPath, branch: branch)
    }

    public func push(
        repoPath: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> PushOutcome {
        try RepositoryGuard.push(
            repoPath: repoPath, credentials: credentials, acceptedHostKey: acceptedHostKey,
            cancel: cancel)
    }
}

/// Titles for the Pull and Push controls: the bare verb, or the verb with the commit
/// count from the last preflight.
public enum SyncTitles {
    public static func pull(_ status: RemoteStatus?) -> String {
        title("Pull", count: status?.behind ?? 0)
    }

    public static func push(_ status: RemoteStatus?) -> String {
        title("Push", count: status?.ahead ?? 0)
    }

    /// Title for the control that switches to the mainline branch and brings it up to date.
    public static func backToDefault(_ branch: String?) -> String {
        branch.map { "Back to \($0)" } ?? "Back to the Default Branch"
    }

    private static func title(_ verb: String, count: Int) -> String {
        count > 0 ? "\(verb) \(count) Commit\(count == 1 ? "" : "s")" : verb
    }
}

/// The shell command that reconciles a diverged branch. rc-git only fast-forwards, so the
/// rebase runs in git; the `cd` lets the command paste into any shell and leaves that shell
/// in the checkout when the rebase stops on a conflict.
public enum ReconcileCommand {
    public static func rebase(workDir: String) -> String {
        "cd \(ShellWord.quote(workDir)) && git pull --rebase"
    }
}

/// One step of the sync workflow.
public enum SyncStep: Sendable, Hashable {
    case checkout
    case fetch
    case pull
    case push
}

/// What one completed workflow did.
public struct SyncSummary: Sendable, Hashable {
    public let fetchedRefs: Int
    public let pulledFiles: Int
    public let pushedCommits: Int
    /// The branch a workflow switched to, `nil` when it switched nothing.
    public let switchedTo: String?

    public init(
        fetchedRefs: Int, pulledFiles: Int, pushedCommits: Int, switchedTo: String? = nil
    ) {
        self.fetchedRefs = fetchedRefs
        self.pulledFiles = pulledFiles
        self.pushedCommits = pushedCommits
        self.switchedTo = switchedTo
    }
}

/// Where the sync workflow stands.
public enum SyncState: Sendable, Hashable {
    case idle
    case running(SyncStep)
    /// Stopping: the user asked, the step has been told, and the workflow is unwinding.
    case cancelling(SyncStep)
    case done(SyncSummary)
    /// Local and upstream both moved; informative and terminal.
    case diverged(message: String)
    case failed(step: SyncStep, message: String)
}

/// State machine for fetch/pull/push/sync: busy-gated, credential-negotiating, with every
/// blocking call on a detached task. Guarded actions run here (through the injected
/// performer); the window controller renders the state.
@MainActor
public final class SyncModel {
    /// How many times one step may ask for credentials before giving up.
    private static let promptLimit = 3

    public private(set) var state: SyncState = .idle
    /// The last successful preflight; feeds menu titles and tooltips.
    public private(set) var remoteStatus: RemoteStatus?
    /// The repository's mainline branch, read once with the first status refresh. Names the
    /// Back-to-default control and is what it switches to.
    public private(set) var defaultBranch: String?

    public var isBusy: Bool {
        switch state {
        case .running, .cancelling: true
        case .idle, .done, .diverged, .failed: false
        }
    }

    /// Whether the running workflow can still be asked to stop.
    public var isCancellable: Bool {
        if case .running = state { return true }
        return false
    }

    public var onStateChanged: (() -> Void)?
    /// Fires on the main actor when a network step needs credentials for `host`. The
    /// kind says what to ask for: an HTTPS token, or an ssh key passphrase. Resume with
    /// `nil` to cancel the flow; the model retries otherwise.
    public var onCredentialsNeeded:
        ((String, CredentialKind, @escaping (Credentials?) -> Void) -> Void)?
    /// Fires on the main actor when an ssh host is unknown: host and
    /// `"<key type> <fingerprint>"`. Resume with `true` to trust and retry.
    public var onHostKeyNeeded: ((String, String, @escaping (Bool) -> Void) -> Void)?

    private let repoPath: String
    private let performer: SyncPerforming
    private let store: CredentialStoring
    private let hostKeys: HostKeyStoring
    private var generation = 0
    /// The running workflow's flag. One per workflow: a token that has been signalled
    /// stays signalled, so the next workflow gets a fresh one.
    private var cancel = CancelToken()

    public init(
        repoPath: String,
        performer: SyncPerforming,
        store: CredentialStoring,
        hostKeys: HostKeyStoring = UserDefaultsHostKeyStore()
    ) {
        self.repoPath = repoPath
        self.performer = performer
        self.store = store
        self.hostKeys = hostKeys
    }

    /// Re-reads the upstream status. Cheap and local; a no-op while a workflow runs. The
    /// mainline branch comes along the first time, and does not move afterwards.
    public func refreshStatus() async {
        guard !isBusy else { return }
        let status = await readStatus()
        let branch = defaultBranch == nil ? await readDefaultBranch() : nil
        guard !isBusy else { return }
        if let branch {
            defaultBranch = branch
        }
        if let status {
            remoteStatus = status
            onStateChanged?()
        } else if branch != nil {
            onStateChanged?()
        }
    }

    /// Returns terminal states to `.idle` so the next workflow starts clean.
    public func reset() {
        guard !isBusy else { return }
        state = .idle
        onStateChanged?()
    }

    /// Asks the running workflow to stop. The flag reaches the step itself — the ssh
    /// transport wakes out of its read, an HTTPS step stops being waited for and its
    /// curl thread is left to time out on its own — so this does not wait for a dead
    /// connection to time out. A no-op when nothing is
    /// running or a cancel is already under way.
    public func cancelRunning() {
        guard case let .running(step) = state else { return }
        cancel.signal()
        state = .cancelling(step)
        onStateChanged?()
    }

    public func fetch() async {
        await workflow { model -> SyncSummary? in
            let fetched = try await model.runFetch()
            return SyncSummary(
                fetchedRefs: fetched.updatedRefs, pulledFiles: 0, pushedCommits: 0)
        }
    }

    /// Fetch, then fast-forward: what the Pull menu item means.
    public func pull() async {
        await workflow { model -> SyncSummary? in
            let fetched = try await model.runFetch()
            let pulled = try await model.runPull()
            return SyncSummary(
                fetchedRefs: fetched.updatedRefs,
                pulledFiles: pulled.filesChanged,
                pushedCommits: 0
            )
        }
    }

    /// Switch to the repository's mainline branch, then fetch and fast-forward it: one
    /// control for "give me the latest master to work on". The upstream is checked after the
    /// switch, because it is the branch being switched to whose upstream matters.
    public func backToDefaultBranch() async {
        await workflow(checkUpstream: false) { model -> SyncSummary? in
            let branch = try await model.resolveDefaultBranch()
            let switched = try await model.runCheckout(branch)
            try await model.adoptUpstreamOfCurrentBranch()
            let fetched = try await model.runFetch()
            let pulled = try await model.runPull()
            return SyncSummary(
                fetchedRefs: fetched.updatedRefs,
                pulledFiles: pulled.filesChanged + switched.filesChanged,
                pushedCommits: 0,
                switchedTo: switched.alreadyOnBranch ? nil : switched.branch
            )
        }
    }

    public func push() async {
        await workflow { model -> SyncSummary? in
            let pushed = try await model.runPush()
            return SyncSummary(
                fetchedRefs: 0, pulledFiles: 0, pushedCommits: pushed.pushedCommits)
        }
    }

    /// Fetch, fast-forward, then push. Stops at the first failure or divergence.
    public func sync() async {
        await workflow { model -> SyncSummary? in
            let fetched = try await model.runFetch()
            let pulled = try await model.runPull()
            let pushed = try await model.runPush()
            return SyncSummary(
                fetchedRefs: fetched.updatedRefs,
                pulledFiles: pulled.filesChanged,
                pushedCommits: pushed.pushedCommits
            )
        }
    }

    // MARK: - Workflow plumbing

    /// A step that failed in a way the workflow reports and stops on.
    private struct StepFailure: Error {
        let step: SyncStep
        let message: String
        let diverged: Bool
    }

    private func workflow(
        checkUpstream: Bool = true, _ body: (SyncModel) async throws -> SyncSummary?
    ) async {
        guard !isBusy, !checkUpstream || preflightAllows() else { return }
        generation += 1
        let generation = generation
        cancel = CancelToken()
        do {
            let summary = try await body(self)
            guard generation == self.generation else { return }
            state = .done(
                summary ?? SyncSummary(fetchedRefs: 0, pulledFiles: 0, pushedCommits: 0))
        } catch let failure as StepFailure {
            guard generation == self.generation else { return }
            // A cancel unwinds through whatever the step reported — usually the guard's
            // own cancelled error, or a transport error the step hit as the flag was
            // set. What the user asked for decides the state, not which of those
            // arrived.
            state =
                switch (cancel.isSignalled, failure.diverged) {
                case (true, _): .idle
                case (false, true): .diverged(message: failure.message)
                case (false, false): .failed(step: failure.step, message: failure.message)
                }
        } catch {
            guard generation == self.generation else { return }
            state = cancel.isSignalled ? .idle : .failed(step: .fetch, message: "\(error)")
        }
        onStateChanged?()
        await refreshStatus()
    }

    /// Fails fast — before any network step — when the branch has no upstream.
    private func preflightAllows() -> Bool {
        let repoPath = repoPath
        if let status = remoteStatus, !status.hasUpstream {
            state = .failed(
                step: .fetch,
                message:
                    "branch '\(status.branch)' has no upstream; configure one with git first"
            )
            onStateChanged?()
            return false
        }
        _ = repoPath
        return true
    }

    /// One preflight on a detached task. `nil` when it failed; the caller decides whether
    /// that is fatal.
    private func readStatus() async -> RemoteStatus? {
        let repoPath = repoPath
        let performer = performer
        return await Task.detached(priority: .utility) {
            try? performer.preflight(repoPath: repoPath)
        }.value
    }

    private func readDefaultBranch() async -> String? {
        let repoPath = repoPath
        let performer = performer
        return await Task.detached(priority: .utility) {
            try? performer.defaultBranch(repoPath: repoPath)
        }.value
    }

    /// The branch the Back-to-default workflow targets. Fails the workflow when nothing names
    /// one, because there is then no sensible branch to switch to.
    private func resolveDefaultBranch() async throws -> String {
        enter(.checkout)
        if let branch = defaultBranch {
            return branch
        }
        guard let branch = await readDefaultBranch() else {
            throw StepFailure(
                step: .checkout,
                message:
                    "cannot tell which branch is the mainline: no remote HEAD, and no local "
                    + "'master' or 'main'",
                diverged: false
            )
        }
        defaultBranch = branch
        return branch
    }

    /// Re-reads the upstream after a switch, so the fetch that follows authenticates against
    /// the new branch's remote. A branch with no upstream stops the workflow here rather than
    /// opening a connection it cannot name.
    private func adoptUpstreamOfCurrentBranch() async throws {
        guard let status = await readStatus() else {
            throw StepFailure(
                step: .fetch, message: "cannot read the upstream status", diverged: false)
        }
        remoteStatus = status
        onStateChanged?()
        guard status.hasUpstream else {
            throw StepFailure(
                step: .fetch,
                message:
                    "branch '\(status.branch)' has no upstream; configure one with git first",
                diverged: false
            )
        }
    }

    private func runCheckout(_ branch: String) async throws -> CheckoutOutcome {
        enter(.checkout)
        let performer = performer
        let repoPath = repoPath
        let result = await detached { () throws(SyncError) -> CheckoutOutcome in
            try performer.checkout(repoPath: repoPath, branch: branch)
        }
        switch result {
        case let .success(outcome):
            return outcome
        case let .failure(error):
            throw StepFailure(step: .checkout, message: error.message, diverged: false)
        }
    }

    /// Runs one transfer step off the main actor and returns its outcome. The closure type is
    /// spelled at each call site because a closure literal infers `throws(any Error)`.
    private func detached<T: Sendable>(
        _ body: @escaping @Sendable () throws(SyncError) -> T
    ) async -> Result<T, SyncError> {
        await Task.detached(priority: .userInitiated) {
            Result { () throws(SyncError) -> T in try body() }
        }.value
    }

    private func runFetch() async throws -> FetchOutcome {
        try await networkStep(.fetch) {
            (performer, repoPath, credentials, hostKey, cancel) throws(SyncError) -> FetchOutcome in
            try performer.fetch(
                repoPath: repoPath, credentials: credentials, acceptedHostKey: hostKey,
                cancel: cancel)
        }
    }

    private func runPull() async throws -> PullOutcome {
        enter(.pull)
        let performer = performer
        let repoPath = repoPath
        let result = await detached { () throws(SyncError) -> PullOutcome in
            try performer.pullFastForward(repoPath: repoPath)
        }
        switch result {
        case let .success(outcome):
            return outcome
        case let .failure(error):
            throw StepFailure(
                step: .pull,
                message: error.message,
                diverged: isDiverged(error)
            )
        }
    }

    private func runPush() async throws -> PushOutcome {
        try await networkStep(.push) {
            (performer, repoPath, credentials, hostKey, cancel) throws(SyncError) -> PushOutcome in
            try performer.push(
                repoPath: repoPath, credentials: credentials, acceptedHostKey: hostKey,
                cancel: cancel)
        }
    }

    /// Runs one network step with the credential loop: stored credentials (or none)
    /// first, then prompt-and-retry on `authRequired`, storing only what worked. An
    /// unknown ssh host key gets one trust prompt, separate from the credential
    /// attempts; the accepted fingerprint persists in `hostKeys`.
    private func networkStep<Outcome: Sendable>(
        _ step: SyncStep,
        _ operation:
            @escaping @Sendable (SyncPerforming, String, Credentials?, String?, CancelToken)
            throws(SyncError) -> Outcome
    ) async throws -> Outcome {
        enter(step)
        let host = remoteStatus?.host ?? ""
        let kind: CredentialKind =
            (remoteStatus?.usesSsh ?? false) ? .sshPassphrase : .httpsToken
        var credentials = (try? store.credentials(host: host, kind: kind)) ?? nil
        var acceptedHostKey = hostKeys.fingerprint(host: host)
        var fromPrompt = false
        var hostKeyPrompted = false
        var authAttempts = 0
        while true {
            // Covers the gap a sheet opens: a cancel raised while the user is looking
            // at a prompt must not start another attempt when they answer it.
            try bailIfCancelled(step)
            let performer = performer
            let repoPath = repoPath
            let attemptCredentials = credentials
            let attemptHostKey = acceptedHostKey
            let cancel = cancel
            let result = await detached { () throws(SyncError) -> Outcome in
                try operation(performer, repoPath, attemptCredentials, attemptHostKey, cancel)
            }
            switch result {
            case let .success(outcome):
                if fromPrompt, let credentials {
                    try? store.store(credentials, host: host, kind: kind)
                }
                return outcome
            case let .failure(.hostKeyUnknown(message, hostKey)):
                try bailIfCancelled(step)
                let fingerprint = String(
                    hostKey.split(separator: " ").last ?? Substring(hostKey))
                // A key that differs from an earlier approval is a changed key, never a
                // first contact: refuse it without offering the trust sheet.
                if let approved = acceptedHostKey, approved != fingerprint {
                    throw StepFailure(
                        step: step,
                        message: Self.changedHostKeyMessage(
                            host: host, approved: approved, offered: fingerprint),
                        diverged: false)
                }
                guard !hostKeyPrompted else {
                    throw StepFailure(step: step, message: message, diverged: false)
                }
                hostKeyPrompted = true
                guard await promptForHostKey(host: host, hostKey: hostKey) else {
                    throw StepFailure(
                        step: step, message: "host not trusted", diverged: false)
                }
                hostKeys.store(fingerprint: fingerprint, host: host)
                acceptedHostKey = fingerprint
            case let .failure(error):
                guard case .authRequired = error else {
                    throw StepFailure(
                        step: step, message: error.message, diverged: isDiverged(error))
                }
                try bailIfCancelled(step)
                authAttempts += 1
                guard authAttempts < Self.promptLimit else {
                    throw StepFailure(
                        step: step,
                        message: "'\(host)' kept rejecting the credentials",
                        diverged: false
                    )
                }
                guard let prompted = await promptForCredentials(host: host, kind: kind) else {
                    throw StepFailure(
                        step: step, message: "authentication canceled", diverged: false)
                }
                credentials = prompted
                fromPrompt = true
            }
        }
    }

    private static func changedHostKeyMessage(
        host: String, approved: String, offered: String
    ) -> String {
        "the ssh host key for '\(host)' changed: approved \(approved), offered \(offered); "
            + "this may be a man-in-the-middle — investigate with ssh directly"
    }

    private func promptForCredentials(host: String, kind: CredentialKind) async -> Credentials? {
        guard let onCredentialsNeeded else { return nil }
        return await withCheckedContinuation { continuation in
            onCredentialsNeeded(host, kind) { credentials in
                continuation.resume(returning: credentials)
            }
        }
    }

    private func promptForHostKey(host: String, hostKey: String) async -> Bool {
        guard let onHostKeyNeeded else { return false }
        return await withCheckedContinuation { continuation in
            onHostKeyNeeded(host, hostKey) { trusted in
                continuation.resume(returning: trusted)
            }
        }
    }

    private func enter(_ step: SyncStep) {
        state = .running(step)
        onStateChanged?()
    }

    /// Stops the retry loop when the user cancelled: a cancelled transfer can surface
    /// as an auth or host-key failure, and neither should put a sheet on screen after
    /// the user has already said stop.
    private func bailIfCancelled(_ step: SyncStep) throws(StepFailure) {
        guard cancel.isSignalled else { return }
        throw StepFailure(step: step, message: "cancelled", diverged: false)
    }

    private func isDiverged(_ error: SyncError) -> Bool {
        if case .diverged = error { return true }
        return false
    }
}
