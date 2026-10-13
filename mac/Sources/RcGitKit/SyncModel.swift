import Foundation

/// The seam between the sync workflow and the FFI, so tests never open a socket. The
/// network steps take the operation's `CancelToken`; it is the only way to reach a
/// thread already blocked inside a transfer.
///
/// `host` on a network step is the host `credentials` were issued for; the guard offers them
/// to no other.
public protocol SyncPerforming: Sendable {
    func preflight(repoPath: String) throws -> RemoteStatus
    /// The repository's mainline branch, or `nil` when nothing names one. Local and cheap.
    func defaultBranch(repoPath: String) throws -> String?
    func fetch(
        repoPath: String, host: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> FetchOutcome
    func pullFastForward(repoPath: String) throws(SyncError) -> PullOutcome
    func checkout(repoPath: String, branch: String) throws(SyncError) -> CheckoutOutcome
    func push(
        repoPath: String, host: String, credentials: Credentials?, acceptedHostKey: String?,
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
        repoPath: String, host: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> FetchOutcome {
        try RepositoryGuard.fetch(
            repoPath: repoPath, credentials: credentials, host: host,
            acceptedHostKey: acceptedHostKey, cancel: cancel)
    }

    public func pullFastForward(repoPath: String) throws(SyncError) -> PullOutcome {
        try RepositoryGuard.pullFastForward(repoPath: repoPath)
    }

    public func checkout(repoPath: String, branch: String) throws(SyncError) -> CheckoutOutcome {
        try RepositoryGuard.checkout(repoPath: repoPath, branch: branch)
    }

    public func push(
        repoPath: String, host: String, credentials: Credentials?, acceptedHostKey: String?,
        cancel: CancelToken
    ) throws(SyncError) -> PushOutcome {
        try RepositoryGuard.push(
            repoPath: repoPath, credentials: credentials, host: host,
            acceptedHostKey: acceptedHostKey, cancel: cancel)
    }
}

/// Titles and enabled state for the Pull and Push controls: the bare verb, or the verb with
/// the commit count from the last preflight. The button row and the menu both ask here, so
/// they cannot disagree.
public enum SyncTitles {
    public static func pull(_ status: RemoteStatus?) -> String {
        title("Pull", count: status?.behind ?? 0)
    }

    public static func push(_ status: RemoteStatus?) -> String {
        title("Push", count: status?.ahead ?? 0)
    }

    /// Pull needs a known upstream: false before the first status, after a failed read,
    /// without an upstream, and while a workflow runs.
    public static func canPull(status: RemoteStatus?, busy: Bool) -> Bool {
        !busy && status?.hasUpstream == true
    }

    /// Push needs exactly what Pull needs.
    public static func canPush(status: RemoteStatus?, busy: Bool) -> Bool {
        canPull(status: status, busy: busy)
    }

    /// "Sync — 2 to push, 3 to pull" while idle, from the last preflight; when the last read
    /// failed, says so with the cause rather than repeating an older answer.
    public static func idleSyncTooltip(status: RemoteStatus?, error: String?) -> String {
        if let error {
            return "Sync — couldn't read upstream status: \(error)"
        }
        guard let status, status.hasUpstream else { return "Sync" }
        if status.ahead == 0 && status.behind == 0 {
            return "Sync — up to date"
        }
        var parts: [String] = []
        if status.ahead > 0 { parts.append("\(status.ahead) to push") }
        if status.behind > 0 { parts.append("\(status.behind) to pull") }
        return "Sync — " + parts.joined(separator: ", ")
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
    /// The step after which a cancel stopped the workflow, `nil` when it ran to the end.
    /// Set when the cancel landed during a step that cannot stop (checkout, pull): that step
    /// finished, and what it did is still reported.
    public let cancelledAfter: SyncStep?

    public init(
        fetchedRefs: Int, pulledFiles: Int, pushedCommits: Int, cancelledAfter: SyncStep? = nil
    ) {
        self.fetchedRefs = fetchedRefs
        self.pulledFiles = pulledFiles
        self.pushedCommits = pushedCommits
        self.cancelledAfter = cancelledAfter
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
    /// Authentication attempts one step makes before giving up: the stored (or anonymous)
    /// attempt, then up to two prompted ones.
    private static let attemptLimit = 3

    public private(set) var state: SyncState = .idle
    /// The last preflight's answer; feeds menu titles and tooltips. `nil` before the first
    /// read and after a failed one, so a stale count is never shown as current.
    public private(set) var remoteStatus: RemoteStatus?
    /// Why the last preflight failed, `nil` after a successful one.
    public private(set) var statusError: String?
    /// The repository's mainline branch, read once with the first status refresh. Names the
    /// Back-to-default control and is what it switches to.
    public private(set) var defaultBranch: String?
    /// Whether `defaultBranch` holds an answer, so a repository that names no mainline is not
    /// re-read on every refresh.
    private var defaultBranchRead = false
    /// A status read is running, and another is wanted once it lands. Watcher-driven, so it
    /// coalesces rather than queueing a read per event.
    private var statusRefreshing = false
    private var statusPending = false
    /// Bumped before every status read; a read that finds it moved drops its answer.
    private var statusGeneration = 0
    /// The branch the last workflow checked out, set the moment the switch lands and kept
    /// whatever ends the workflow — done, diverged, failed or cancelled — because HEAD and
    /// the working tree have moved either way. Cleared when a workflow starts.
    private var switchedBranch: String?

    public var isBusy: Bool {
        switch state {
        case .running, .cancelling: true
        case .idle, .done, .diverged, .failed: false
        }
    }

    /// Whether the running workflow can still be asked to stop. Not during a checkout or a
    /// pull: those writes take no cancel and always run to completion.
    public var isCancellable: Bool {
        switch state {
        case .running(.fetch), .running(.push): true
        case .running(.checkout), .running(.pull), .idle, .cancelling, .done, .diverged,
            .failed:
            false
        }
    }

    public var onStateChanged: (() -> Void)?
    /// Fires on the main actor when a network step needs credentials for `host`. The
    /// kind says what to ask for: an HTTPS token, or an ssh key passphrase, with the
    /// locked key file's path (`nil` for HTTPS). Resume with `nil` to cancel the flow;
    /// the model retries otherwise.
    public var onCredentialsNeeded:
        ((String, CredentialKind, String?, @escaping (Credentials?) -> Void) -> Void)?
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
    ///
    /// Coalesces: a call made while a read runs marks one more read wanted and returns, and
    /// the running loop reads once more when it lands.
    public func refreshStatus() async {
        statusPending = true
        guard !statusRefreshing else { return }
        statusRefreshing = true
        while statusPending {
            statusPending = false
            await readStatusOnce()
        }
        statusRefreshing = false
    }

    private func readStatusOnce() async {
        guard !isBusy else { return }
        statusGeneration += 1
        let generation = statusGeneration
        let readsBranch = !defaultBranchRead
        async let statusRead = readStatus()
        async let branchRead = readDefaultBranch(if: readsBranch)
        let (status, branch) = await (statusRead, branchRead)
        guard generation == statusGeneration, !isBusy else { return }
        if case let .success(name) = branch {
            defaultBranch = name
            defaultBranchRead = true
        }
        apply(status)
    }

    /// Records one preflight's answer: a failure clears the last status, so nothing reads an
    /// older count as current.
    private func apply(_ status: Result<RemoteStatus, StatusReadError>) {
        switch status {
        case let .success(value):
            remoteStatus = value
            statusError = nil
        case let .failure(error):
            remoteStatus = nil
            statusError = error.message
        }
        onStateChanged?()
    }

    /// The branch a workflow switched to, once: the caller follows it, and a second call
    /// answers `nil`. `nil` while a workflow runs and when the last one switched nothing.
    public func takeSwitchedBranch() -> String? {
        guard !isBusy else { return nil }
        defer { switchedBranch = nil }
        return switchedBranch
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
    /// connection to time out. A cancel that lands during a checkout or a pull lets that
    /// write finish and stops the workflow at the next step, which still reports what the
    /// write did. A no-op when nothing is running or a cancel is already under way.
    public func cancelRunning() {
        guard case let .running(step) = state else { return }
        cancel.signal()
        state = .cancelling(step)
        onStateChanged?()
    }

    public func fetch() async {
        await workflow(start: .fetch) { model -> SyncSummary? in
            let fetched = try await model.runFetch()
            return SyncSummary(
                fetchedRefs: fetched.updatedRefs, pulledFiles: 0, pushedCommits: 0)
        }
    }

    /// Fetch, then fast-forward: what the Pull menu item means.
    public func pull() async {
        await workflow(start: .fetch) { model -> SyncSummary? in
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
        await workflow(start: .checkout, checkUpstream: false) { model -> SyncSummary? in
            let branch = try await model.resolveDefaultBranch()
            let switched = try await model.runCheckout(branch)
            if !switched.alreadyOnBranch {
                model.switchedBranch = switched.branch
            }
            if model.cancel.isSignalled {
                return SyncSummary(
                    fetchedRefs: 0, pulledFiles: switched.filesChanged, pushedCommits: 0,
                    cancelledAfter: .checkout)
            }
            try await model.adoptFreshUpstream(.fetch)
            let fetched = try await model.runFetch()
            let pulled = try await model.runPull()
            return SyncSummary(
                fetchedRefs: fetched.updatedRefs,
                pulledFiles: pulled.filesChanged + switched.filesChanged,
                pushedCommits: 0
            )
        }
    }

    public func push() async {
        await workflow(start: .push) { model -> SyncSummary? in
            let pushed = try await model.runPush()
            return SyncSummary(
                fetchedRefs: 0, pulledFiles: 0, pushedCommits: pushed.pushedCommits)
        }
    }

    /// Fetch, fast-forward, then push. Stops at the first failure or divergence.
    public func sync() async {
        await workflow(start: .fetch) { model -> SyncSummary? in
            let fetched = try await model.runFetch()
            let pulled = try await model.runPull()
            if model.cancel.isSignalled {
                return SyncSummary(
                    fetchedRefs: fetched.updatedRefs, pulledFiles: pulled.filesChanged,
                    pushedCommits: 0, cancelledAfter: .pull)
            }
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

    /// Runs one workflow from `start`. With `checkUpstream`, the upstream is re-read first and
    /// the workflow refuses on that fresh answer, never on the cached one: a `git remote add`
    /// or `git checkout --detach` in a terminal since the last refresh decides, and the network
    /// steps key credentials and host keys on the URL the guard will dial.
    private func workflow(
        start: SyncStep, checkUpstream: Bool = true,
        _ body: (SyncModel) async throws -> SyncSummary?
    ) async {
        guard !isBusy else { return }
        generation += 1
        let generation = generation
        cancel = CancelToken()
        switchedBranch = nil
        enter(start)
        do {
            if checkUpstream {
                try await adoptFreshUpstream(start)
            }
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

    /// Why a status read failed, carried as a message ready to show.
    private struct StatusReadError: Error {
        let message: String
    }

    /// One preflight on a detached task.
    private func readStatus() async -> Result<RemoteStatus, StatusReadError> {
        let repoPath = repoPath
        let performer = performer
        return await Task.detached(priority: .utility) {
            do {
                return .success(try performer.preflight(repoPath: repoPath))
            } catch {
                return .failure(StatusReadError(message: Self.message(of: error)))
            }
        }.value
    }

    /// The mainline branch on a detached task, or `nil` when `needed` is false. A success
    /// carrying `nil` means nothing names a mainline; a failure means it could not be read.
    private func readDefaultBranch(if needed: Bool) async
        -> Result<String?, StatusReadError>?
    {
        guard needed else { return nil }
        let repoPath = repoPath
        let performer = performer
        return await Task.detached(priority: .utility) {
            do {
                return .success(try performer.defaultBranch(repoPath: repoPath))
            } catch {
                return .failure(StatusReadError(message: Self.message(of: error)))
            }
        }.value
    }

    private nonisolated static func message(of error: any Error) -> String {
        switch error {
        case let error as SyncError: error.message
        case let error as RepositoryError: error.message
        default: "\(error)"
        }
    }

    /// The branch the Back-to-default workflow targets. Fails the workflow when nothing names
    /// one, because there is then no sensible branch to switch to, and with the cause when the
    /// repository could not be read.
    private func resolveDefaultBranch() async throws -> String {
        try bailIfCancelled(.checkout)
        enter(.checkout)
        if let branch = defaultBranch {
            return branch
        }
        switch await readDefaultBranch(if: true) {
        case let .success(branch?):
            defaultBranch = branch
            defaultBranchRead = true
            return branch
        case .success(nil), nil:
            defaultBranchRead = true
            throw StepFailure(
                step: .checkout,
                message:
                    "cannot tell which branch is the mainline: no remote HEAD, and no local "
                    + "'master' or 'main'",
                diverged: false
            )
        case let .failure(error):
            throw StepFailure(
                step: .checkout,
                message: "cannot read the mainline branch: \(error.message)",
                diverged: false)
        }
    }

    /// Re-reads the upstream and makes it the status everything else reads, so the steps that
    /// follow authenticate against the remote the guard will dial. A failed read or a branch
    /// with no upstream stops the workflow here rather than opening a connection it cannot
    /// name. An in-flight refresh started before this read drops its older answer.
    private func adoptFreshUpstream(_ step: SyncStep) async throws {
        statusGeneration += 1
        let status = await readStatus()
        apply(status)
        switch status {
        case let .failure(error):
            throw StepFailure(
                step: step, message: "cannot read the upstream status: \(error.message)",
                diverged: false)
        case let .success(status) where !status.hasUpstream:
            throw StepFailure(
                step: step,
                message:
                    "branch '\(status.branch)' has no upstream; configure one with git first",
                diverged: false
            )
        case .success:
            return
        }
    }

    private func runCheckout(_ branch: String) async throws -> CheckoutOutcome {
        try bailIfCancelled(.checkout)
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
        let repoPath = repoPath
        return try await networkStep(.fetch) {
            (performer, host, credentials, hostKey, cancel) throws(SyncError) -> FetchOutcome in
            try performer.fetch(
                repoPath: repoPath, host: host, credentials: credentials,
                acceptedHostKey: hostKey, cancel: cancel)
        }
    }

    private func runPull() async throws -> PullOutcome {
        try bailIfCancelled(.pull)
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
        let repoPath = repoPath
        return try await networkStep(.push) {
            (performer, host, credentials, hostKey, cancel) throws(SyncError) -> PushOutcome in
            try performer.push(
                repoPath: repoPath, host: host, credentials: credentials,
                acceptedHostKey: hostKey, cancel: cancel)
        }
    }

    /// Runs one network step with the credential loop: stored credentials (or none)
    /// first, then prompt-and-retry on what a secret can fix (`authRequired` over HTTPS,
    /// `passphraseRequired` over ssh), storing only what worked. An
    /// unknown ssh host key gets one trust prompt, separate from the credential
    /// attempts; the accepted fingerprint persists in `hostKeys`. An upstream that names no
    /// host goes to `hostlessStep` instead, before anything keyed on a host is read.
    private func networkStep<Outcome: Sendable>(
        _ step: SyncStep,
        _ operation: @escaping NetworkOperation<Outcome>
    ) async throws -> Outcome {
        enter(step)
        guard let status = remoteStatus, !status.host.isEmpty else {
            return try await hostlessStep(step, operation)
        }
        let host = status.host
        let kind: CredentialKind = status.usesSsh ? .sshPassphrase : .httpsToken
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
            let attemptCredentials = credentials
            let attemptHostKey = acceptedHostKey
            let cancel = cancel
            let result = await detached { () throws(SyncError) -> Outcome in
                try operation(performer, host, attemptCredentials, attemptHostKey, cancel)
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
                guard case let .prompt(key) = Self.credentialRetry(after: error, kind: kind)
                else {
                    throw StepFailure(
                        step: step, message: error.message, diverged: isDiverged(error))
                }
                try bailIfCancelled(step)
                authAttempts += 1
                guard authAttempts < Self.attemptLimit else {
                    throw StepFailure(
                        step: step,
                        message: "'\(host)' kept rejecting the credentials: \(error.message)",
                        diverged: false
                    )
                }
                guard
                    let prompted = await promptForCredentials(host: host, kind: kind, key: key)
                else {
                    throw StepFailure(
                        step: step, message: "authentication canceled", diverged: false)
                }
                credentials = prompted
                fromPrompt = true
            }
        }
    }

    /// One network attempt: the performer, the host the credentials belong to, the
    /// credentials, the approved host key, and the workflow's cancel flag.
    private typealias NetworkOperation<Outcome> =
        @Sendable (SyncPerforming, String, Credentials?, String?, CancelToken)
        throws(SyncError) -> Outcome

    /// A step whose upstream names no host: a local upstream, whose fetch the guard skips,
    /// or a URL the guard refuses by its scheme. One attempt with no credentials and no
    /// approved host key, and no prompt: a secret or approval kept under an empty host would
    /// be handed to every other upstream that names none, in any repository.
    private func hostlessStep<Outcome: Sendable>(
        _ step: SyncStep, _ operation: @escaping NetworkOperation<Outcome>
    ) async throws -> Outcome {
        try bailIfCancelled(step)
        let performer = performer
        let cancel = cancel
        let result = await detached { () throws(SyncError) -> Outcome in
            try operation(performer, "", nil, nil, cancel)
        }
        switch result {
        case let .success(outcome):
            return outcome
        case .failure(.authRequired), .failure(.passphraseRequired), .failure(.hostKeyUnknown):
            throw StepFailure(
                step: step, message: "cannot tell which host the upstream is on",
                diverged: false)
        case let .failure(error):
            throw StepFailure(step: step, message: error.message, diverged: isDiverged(error))
        }
    }

    private static func changedHostKeyMessage(
        host: String, approved: String, offered: String
    ) -> String {
        "the ssh host key for '\(host)' changed: approved \(approved), offered \(offered); "
            + "this may be a man-in-the-middle — investigate with ssh directly"
    }

    /// What a failed attempt allows: a prompt for the secret that can fix it, naming the
    /// locked key over ssh, or nothing. Over ssh only a locked key is worth a prompt; a
    /// plain rejection means no key was accepted, which no passphrase changes.
    enum CredentialRetry: Equatable {
        case prompt(key: String?)
        case none
    }

    static func credentialRetry(after error: SyncError, kind: CredentialKind)
        -> CredentialRetry
    {
        switch (error, kind) {
        case (.authRequired, .httpsToken): .prompt(key: nil)
        case let (.passphraseRequired(_, key), .sshPassphrase): .prompt(key: key)
        case (.authRequired, .sshPassphrase), (.passphraseRequired, .httpsToken),
            (.diverged, _), (.hostKeyUnknown, _), (.cancelled, _), (.failure, _):
            .none
        }
    }

    private func promptForCredentials(host: String, kind: CredentialKind, key: String?) async
        -> Credentials?
    {
        guard let onCredentialsNeeded else { return nil }
        return await withCheckedContinuation { continuation in
            onCredentialsNeeded(host, kind, key) { credentials in
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

    /// Moves the toolbar to `step`. Once a cancel is signalled the workflow only unwinds, so
    /// the step shows as cancelling and never offers Stop again.
    private func enter(_ step: SyncStep) {
        state = cancel.isSignalled ? .cancelling(step) : .running(step)
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
