import Foundation

/// The seam between the sync workflow and the FFI, so tests never open a socket.
public protocol SyncPerforming: Sendable {
    func preflight(repoPath: String) throws -> RemoteStatus
    func fetch(repoPath: String, credentials: Credentials?) throws -> FetchOutcome
    func pullFastForward(repoPath: String) throws -> PullOutcome
    func push(repoPath: String, credentials: Credentials?) throws -> PushOutcome
}

/// Production performer: forwards to `RepositoryGuard`.
public struct GuardSyncPerformer: SyncPerforming {
    public init() {}

    public func preflight(repoPath: String) throws -> RemoteStatus {
        try RepositoryGuard.remotePreflight(repoPath: repoPath)
    }

    public func fetch(repoPath: String, credentials: Credentials?) throws -> FetchOutcome {
        try RepositoryGuard.fetch(repoPath: repoPath, credentials: credentials)
    }

    public func pullFastForward(repoPath: String) throws -> PullOutcome {
        try RepositoryGuard.pullFastForward(repoPath: repoPath)
    }

    public func push(repoPath: String, credentials: Credentials?) throws -> PushOutcome {
        try RepositoryGuard.push(repoPath: repoPath, credentials: credentials)
    }
}

/// One step of the sync workflow.
public enum SyncStep: Sendable, Hashable {
    case fetch
    case pull
    case push
}

/// What one completed workflow did.
public struct SyncSummary: Sendable, Hashable {
    public let fetchedRefs: Int
    public let pulledFiles: Int
    public let pushedCommits: Int
}

/// Where the sync workflow stands.
public enum SyncState: Sendable, Hashable {
    case idle
    case running(SyncStep)
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

    public var isBusy: Bool {
        if case .running = state { return true }
        return false
    }

    public var onStateChanged: (() -> Void)?
    /// Fires on the main actor when a network step needs credentials for `host`.
    /// Resume with `nil` to cancel the flow; the model retries otherwise.
    public var onCredentialsNeeded: ((String, @escaping (Credentials?) -> Void) -> Void)?

    private let repoPath: String
    private let performer: SyncPerforming
    private let store: CredentialStoring
    private var generation = 0

    public init(repoPath: String, performer: SyncPerforming, store: CredentialStoring) {
        self.repoPath = repoPath
        self.performer = performer
        self.store = store
    }

    /// Re-reads the upstream status. Cheap and local; a no-op while a workflow runs.
    public func refreshStatus() async {
        guard !isBusy else { return }
        let repoPath = repoPath
        let performer = performer
        let status = await Task.detached(priority: .utility) {
            try? performer.preflight(repoPath: repoPath)
        }.value
        guard !isBusy else { return }
        if let status {
            remoteStatus = status
            onStateChanged?()
        }
    }

    /// Returns terminal states to `.idle` so the next workflow starts clean.
    public func reset() {
        guard !isBusy else { return }
        state = .idle
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

    private func workflow(_ body: (SyncModel) async throws -> SyncSummary?) async {
        guard !isBusy, preflightAllows() else { return }
        generation += 1
        let generation = generation
        do {
            let summary = try await body(self)
            guard generation == self.generation else { return }
            state = .done(
                summary ?? SyncSummary(fetchedRefs: 0, pulledFiles: 0, pushedCommits: 0))
        } catch let failure as StepFailure {
            guard generation == self.generation else { return }
            state =
                failure.diverged
                ? .diverged(message: failure.message)
                : .failed(step: failure.step, message: failure.message)
        } catch {
            guard generation == self.generation else { return }
            state = .failed(step: .fetch, message: "\(error)")
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

    private func runFetch() async throws -> FetchOutcome {
        try await networkStep(.fetch) { performer, repoPath, credentials in
            try performer.fetch(repoPath: repoPath, credentials: credentials)
        }
    }

    private func runPull() async throws -> PullOutcome {
        enter(.pull)
        let performer = performer
        let repoPath = repoPath
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<PullOutcome, SyncError> in
            do {
                return .success(try performer.pullFastForward(repoPath: repoPath))
            } catch let error as SyncError {
                return .failure(error)
            } catch {
                return .failure(.failure(message: "\(error)"))
            }
        }.value
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
        try await networkStep(.push) { performer, repoPath, credentials in
            try performer.push(repoPath: repoPath, credentials: credentials)
        }
    }

    /// Runs one network step with the credential loop: stored credentials (or none)
    /// first, then prompt-and-retry on `authRequired`, storing only what worked.
    private func networkStep<Outcome: Sendable>(
        _ step: SyncStep,
        _ operation: @escaping @Sendable (SyncPerforming, String, Credentials?) throws -> Outcome
    ) async throws -> Outcome {
        enter(step)
        let host = remoteStatus?.host ?? ""
        var credentials = (try? store.credentials(host: host)) ?? nil
        var fromPrompt = false
        for attempt in 0..<Self.promptLimit {
            let performer = performer
            let repoPath = repoPath
            let attemptCredentials = credentials
            let result = await Task.detached(priority: .userInitiated) {
                () -> Result<Outcome, SyncError> in
                do {
                    return .success(try operation(performer, repoPath, attemptCredentials))
                } catch let error as SyncError {
                    return .failure(error)
                } catch {
                    return .failure(.failure(message: "\(error)"))
                }
            }.value
            switch result {
            case let .success(outcome):
                if fromPrompt, let credentials {
                    try? store.store(credentials, host: host)
                }
                return outcome
            case let .failure(error):
                guard case .authRequired = error else {
                    throw StepFailure(
                        step: step, message: error.message, diverged: isDiverged(error))
                }
                guard attempt + 1 < Self.promptLimit else {
                    throw StepFailure(
                        step: step,
                        message: "'\(host)' kept rejecting the credentials",
                        diverged: false
                    )
                }
                guard let prompted = await promptForCredentials(host: host) else {
                    throw StepFailure(
                        step: step, message: "authentication canceled", diverged: false)
                }
                credentials = prompted
                fromPrompt = true
            }
        }
        throw StepFailure(
            step: step,
            message: "'\(host)' kept rejecting the credentials",
            diverged: false
        )
    }

    private func promptForCredentials(host: String) async -> Credentials? {
        guard let onCredentialsNeeded else { return nil }
        return await withCheckedContinuation { continuation in
            onCredentialsNeeded(host) { credentials in
                continuation.resume(returning: credentials)
            }
        }
    }

    private func enter(_ step: SyncStep) {
        state = .running(step)
        onStateChanged?()
    }

    private func isDiverged(_ error: SyncError) -> Bool {
        if case .diverged = error { return true }
        return false
    }
}
