import CRcGit
import Foundation

/// The policy's answer for a proposed action, as the guard crate decided it. The app renders
/// this; it never classifies an action itself.
public struct GuardDecision: Sendable, Hashable {
    public enum Kind: Sendable, Hashable {
        case allow
        case confirm
        case deny
    }

    public enum Risk: Sendable, Hashable {
        case safe
        case recoverable
        case destructive
    }

    public let kind: Kind
    public let risk: Risk
    /// One sentence saying what will happen. Confirm only.
    public let summary: String
    /// Text the user must type exactly; `nil` means a plain confirm button is enough.
    public let typedPhrase: String?
    /// Whether a backup ref is written before the action runs.
    public let backupRef: Bool
    /// Why the action is refused. Deny only.
    public let reason: String?
    /// Whether every commit of the branch is reachable elsewhere. Branch deletion only.
    public let merged: Bool

    init(c: UnsafeMutablePointer<RcGitGuardDecision>) {
        let value = c.pointee
        kind =
            switch value.kind {
            case RcGitDecisionKind_Allow: .allow
            case RcGitDecisionKind_Confirm: .confirm
            default: .deny
            }
        risk =
            switch value.risk {
            case RcGitRisk_Safe: .safe
            case RcGitRisk_Recoverable: .recoverable
            default: .destructive
            }
        summary = value.summary.map { String(cString: $0) } ?? ""
        typedPhrase = value.typed_phrase.map { String(cString: $0) }
        backupRef = value.backup_ref
        reason = value.reason.map { String(cString: $0) }
        merged = value.merged
    }
}

/// One hunk of a worktree diff, named by its unified header ranges.
public struct HunkStamp: Sendable, Hashable {
    public let oldStart: UInt32
    public let oldLines: UInt32
    public let newStart: UInt32
    public let newLines: UInt32

    public init(oldStart: UInt32, oldLines: UInt32, newStart: UInt32, newLines: UInt32) {
        self.oldStart = oldStart
        self.oldLines = oldLines
        self.newStart = newStart
        self.newLines = newLines
    }

    var c: RcGitHunkStamp {
        RcGitHunkStamp(
            old_start: oldStart, old_lines: oldLines, new_start: newStart, new_lines: newLines)
    }
}

/// What of one file goes into a commit.
public enum CommitSelection: Sendable, Hashable {
    /// The whole file as it is on disk; addition, modification, or deletion is inferred.
    case whole(path: String)
    /// Only the given hunks of a tracked, modified text file. An empty list is refused, never
    /// read as the whole file.
    case hunks(path: String, stamps: [HunkStamp])

    var path: String {
        switch self {
        case let .whole(path), let .hunks(path, _): path
        }
    }
}

/// What a completed commit produced.
public struct CommitOutcome: Sendable, Hashable {
    /// The new commit.
    public let oid: Oid
    /// Short name of the branch that advanced.
    public let branch: String
    /// Whether `.git/index` was updated to the new commit. `false` when that write failed after
    /// the branch moved: the commit stands.
    public let indexRefreshed: Bool
    /// Whether `logs/HEAD` recorded the commit, or none was due. `false` when the append failed
    /// after the branch moved: the commit stands.
    public let reflogWritten: Bool

    /// What the user should read beside a commit that landed with its housekeeping incomplete,
    /// `nil` when everything was written.
    public var warning: String? {
        var lines: [String] = []
        if !indexRefreshed {
            lines.append(
                "The commit was made, but .git/index was not updated; run "
                    + "`git reset -q -- <paths>` so git status agrees with it.")
        }
        if !reflogWritten {
            lines.append(
                "The commit was made, but .git/logs/HEAD did not record it, so HEAD@{1} "
                    + "skips it.")
        }
        return lines.isEmpty ? nil : lines.joined(separator: "\n")
    }
}

/// The one write path: every mutation goes through the guard crate's policy, and the core
/// never writes. Every call opens the repository fresh by path, so nothing here can touch the
/// read-only handles.
public enum RepositoryGuard {
    /// Title for an alert about a failed read of the repository.
    public static let readFailureTitle = "rc-git could not read the repository"

    /// Title for an alert about a guarded action that failed or was refused, named by the
    /// verb its button and challenge carry: "Delete Branch" titles "rc-git could not delete
    /// branch".
    public static func failureTitle(verb: String) -> String {
        "rc-git could not \(verb.lowercased())"
    }

    /// The branch or tag name a name sheet hands on: the field without surrounding
    /// whitespace, or `nil` while nothing is left, which keeps the sheet's Create disabled.
    /// Whether git accepts the name is the guard's call, made when the ref is created.
    public static func enteredRefName(_ raw: String) -> String? {
        let name = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return name.isEmpty ? nil : name
    }

    /// What deleting a branch would need.
    public static func planBranchDelete(repoPath: String, name: String) throws -> GuardDecision {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let plan = rcgit_guard_plan_branch_delete(repoPath, name, &error) else {
            throw RepositoryError.take(error, fallback: "cannot plan deleting '\(name)'")
        }
        defer { rcgit_guard_decision_free(plan) }
        return GuardDecision(c: plan)
    }

    /// What deleting a tag would need. Pure policy.
    public static func planTagDelete(name: String) -> GuardDecision? {
        decision(rcgit_guard_plan_tag_delete(name))
    }

    /// Creates a branch at `revspec`, never overwriting an existing one.
    public static func createBranch(repoPath: String, name: String, revspec: String) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_branch_create(repoPath, name, revspec, &error) == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot create branch '\(name)'")
        }
    }

    /// Creates a lightweight tag at `revspec`, never overwriting an existing one.
    public static func createTag(repoPath: String, name: String, revspec: String) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_tag_create(repoPath, name, revspec, &error) == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot create tag '\(name)'")
        }
    }

    /// Deletes a branch; `input` answers the challenge. Returns the backup ref, if written.
    @discardableResult
    public static func deleteBranch(repoPath: String, name: String, input: String?) throws
        -> String?
    {
        var backup: UnsafeMutablePointer<CChar>? = nil
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = rcgit_guard_branch_delete(repoPath, name, input, &backup, &error)
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot delete branch '\(name)'")
        }
        return RepositoryError.takeString(backup)
    }

    /// Deletes a tag; `input` answers the challenge. Returns the backup ref, if written.
    @discardableResult
    public static func deleteTag(repoPath: String, name: String, input: String?) throws
        -> String?
    {
        var backup: UnsafeMutablePointer<CChar>? = nil
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = rcgit_guard_tag_delete(repoPath, name, input, &backup, &error)
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot delete tag '\(name)'")
        }
        return RepositoryError.takeString(backup)
    }

    /// What committing `files` selected files would need. Pure policy.
    public static func planCommit(files: Int) -> GuardDecision? {
        decision(rcgit_guard_plan_commit(files))
    }

    /// What stashing `files` selected files would need. Pure policy.
    public static func planStashPush(files: Int) -> GuardDecision? {
        decision(rcgit_guard_plan_stash_push(files))
    }

    /// What applying a stash entry would need. Pure policy.
    public static func planStashApply() -> GuardDecision? {
        decision(rcgit_guard_plan_stash_apply())
    }

    /// What popping `entry` would need; the challenge names its position and message. Pure
    /// policy.
    public static func planStashPop(_ entry: StashEntry) -> GuardDecision? {
        decision(rcgit_guard_plan_stash_pop(entry.index, entry.message))
    }

    /// What dropping `entry` would need; the challenge names its position and message. Pure
    /// policy.
    public static func planStashDrop(_ entry: StashEntry) -> GuardDecision? {
        decision(rcgit_guard_plan_stash_drop(entry.index, entry.message))
    }

    /// What discarding changes in `files` files would need. Pure policy.
    public static func planDiscard(files: Int) -> GuardDecision? {
        decision(rcgit_guard_plan_discard(files))
    }

    /// The repository's own `user.name`/`user.email`, read fresh from its config; `nil` when
    /// it sets either one empty or not at all. This is the identity a commit records ahead of
    /// any fallback.
    public static func configuredIdentity(repoPath: String) throws -> CommitIdentity? {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let identity = rcgit_guard_configured_identity(repoPath, &error) else {
            if let error {
                throw RepositoryError.take(error, fallback: "cannot read the commit identity")
            }
            return nil
        }
        defer { rcgit_identity_free(identity) }
        return CommitIdentity(
            name: String(cString: identity.pointee.name),
            email: String(cString: identity.pointee.email))
    }

    /// Commits `selections` to the checked-out branch. `expectedHead` is the commit the
    /// change list was built against, `nil` on an unborn branch; the commit is refused when
    /// `HEAD` moved. `fallback` is recorded only when the repository's own config sets no
    /// identity; it never overrides one the repository sets. Throws `IdentityRequiredError`
    /// when neither gives an identity, `RepositoryError` for every other failure.
    public static func commit(
        repoPath: String, message: String, fallback: CommitIdentity?, expectedHead: Oid?,
        selections: [CommitSelection], input: String?
    ) throws -> CommitOutcome {
        var error: UnsafeMutablePointer<CChar>? = nil
        let ownedPaths = selections.map { strdup($0.path) }
        var stampBuffers: [UnsafeMutablePointer<RcGitHunkStamp>?] = []
        defer {
            ownedPaths.forEach { free($0) }
            stampBuffers.forEach { $0?.deallocate() }
        }
        var files: [RcGitCommitFile] = []
        for (index, selection) in selections.enumerated() {
            switch selection {
            case .whole:
                stampBuffers.append(nil)
                files.append(
                    RcGitCommitFile(path: ownedPaths[index], hunks: nil, hunk_count: 0))
            case let .hunks(_, stamps):
                // Never NULL, even for no stamps: NULL asks the core for the whole file.
                let buffer = UnsafeMutablePointer<RcGitHunkStamp>.allocate(
                    capacity: max(stamps.count, 1))
                for (offset, stamp) in stamps.enumerated() {
                    buffer[offset] = stamp.c
                }
                stampBuffers.append(buffer)
                files.append(
                    RcGitCommitFile(
                        path: ownedPaths[index], hunks: buffer, hunk_count: stamps.count))
            }
        }
        var out: UnsafeMutablePointer<RcGitCommitResult>? = nil
        let status = withOwnedCStrings([message, input]) { strings in
            withFallback(fallback) { fallbackPtr in
                withOptionalOid(expectedHead) { headPtr in
                    files.withUnsafeBufferPointer { buffer in
                        var request = RcGitCommitRequest(
                            message: strings[0], fallback: fallbackPtr, expected_head: headPtr,
                            files: buffer.baseAddress, file_count: files.count)
                        return rcgit_guard_commit(repoPath, &request, strings[1], &out, &error)
                    }
                }
            }
        }
        guard status == RCGIT_OK, let result = out else {
            throw actionError(status, error, fallback: "cannot commit")
        }
        defer { rcgit_commit_result_free(result) }
        return CommitOutcome(
            oid: Oid(c: result.pointee.oid),
            branch: String(cString: result.pointee.branch),
            indexRefreshed: result.pointee.index_refreshed,
            reflogWritten: result.pointee.reflog_written)
    }

    /// Stashes the changes at `paths` and restores those files to `HEAD` state. `fallback` is
    /// recorded only when the repository's own config sets no identity. Throws
    /// `IdentityRequiredError` when neither gives one, `RepositoryError` otherwise.
    public static func stashPush(
        repoPath: String, paths: [String], message: String?, fallback: CommitIdentity?,
        input: String?
    ) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = withOwnedCStrings(paths) { pathPointers in
            withOwnedCStrings([message, input]) { strings in
                withFallback(fallback) { fallbackPtr in
                    pathPointers.withUnsafeBufferPointer { buffer in
                        var request = RcGitStashRequest(
                            paths: buffer.baseAddress, path_count: paths.count,
                            message: strings[0], fallback: fallbackPtr)
                        return rcgit_guard_stash_push(repoPath, &request, strings[1], &error)
                    }
                }
            }
        }
        guard status == RCGIT_OK else {
            throw actionError(status, error, fallback: "cannot stash")
        }
    }

    /// Writes `entry` back to the working tree, keeping it. Refused when the stash at
    /// `entry.index` is no longer `entry.oid`.
    public static func stashApply(repoPath: String, entry: StashEntry, input: String?) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = entry.oid.withC { expected in
            rcgit_guard_stash_apply(repoPath, entry.index, expected, input, &error)
        }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot apply the stash")
        }
    }

    /// Applies `entry` and drops it, refused as `stashApply` is. Returns the backup ref, if
    /// written.
    @discardableResult
    public static func stashPop(repoPath: String, entry: StashEntry, input: String?) throws
        -> String?
    {
        var backup: UnsafeMutablePointer<CChar>? = nil
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = entry.oid.withC { expected in
            rcgit_guard_stash_pop(repoPath, entry.index, expected, input, &backup, &error)
        }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot pop the stash")
        }
        return RepositoryError.takeString(backup)
    }

    /// Deletes `entry` without applying it, refused as `stashApply` is. Returns the backup
    /// ref, if written.
    @discardableResult
    public static func stashDrop(repoPath: String, entry: StashEntry, input: String?) throws
        -> String?
    {
        var backup: UnsafeMutablePointer<CChar>? = nil
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = entry.oid.withC { expected in
            rcgit_guard_stash_drop(repoPath, entry.index, expected, input, &backup, &error)
        }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot drop the stash")
        }
        return RepositoryError.takeString(backup)
    }

    /// Discards every change at `paths`; `input` answers the typed-phrase challenge.
    public static func discard(repoPath: String, paths: [String], input: String?) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = withOwnedCStrings(paths) { pathPointers in
            withOwnedCStrings([input]) { strings in
                pathPointers.withUnsafeBufferPointer { buffer in
                    rcgit_guard_discard(
                        repoPath, buffer.baseAddress, paths.count, strings[0], &error)
                }
            }
        }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot discard")
        }
    }

    /// Discards only the given hunks of one file, keeping its other changes.
    public static func discardHunks(
        repoPath: String, path: String, stamps: [HunkStamp], input: String?
    ) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        let cStamps = stamps.map(\.c)
        let status = withOwnedCStrings([input]) { strings in
            cStamps.withUnsafeBufferPointer { buffer in
                rcgit_guard_discard_hunks(
                    repoPath, path, buffer.baseAddress, stamps.count, strings[0], &error)
            }
        }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot discard hunks of '\(path)'")
        }
    }

    private static func decision(_ pointer: UnsafeMutablePointer<RcGitGuardDecision>?)
        -> GuardDecision?
    {
        guard let pointer else { return nil }
        defer { rcgit_guard_decision_free(pointer) }
        return GuardDecision(c: pointer)
    }

    /// Runs `body` with `fallback` as a C identity, or with NULL when there is none.
    private static func withFallback<T>(
        _ fallback: CommitIdentity?, _ body: (UnsafePointer<RcGitFallbackIdentity>?) -> T
    ) -> T {
        guard let fallback else { return body(nil) }
        return withOwnedCStrings([fallback.name, fallback.email]) { strings in
            var identity = RcGitFallbackIdentity(name: strings[0], email: strings[1])
            return withUnsafePointer(to: &identity) { body($0) }
        }
    }

    /// Runs `body` with each optional string duplicated as a C string (`nil` stays NULL),
    /// freeing them afterwards, so a call with several optional parameters stays flat.
    private static func withOwnedCStrings<T>(
        _ strings: [String?], _ body: ([UnsafePointer<CChar>?]) -> T
    ) -> T {
        let owned = strings.map { $0.map { strdup($0) } ?? nil }
        defer { owned.forEach { free($0) } }
        return body(owned.map { UnsafePointer($0) })
    }
}

/// Where the checked-out branch pushes and pulls, as of the last fetch.
public struct RemoteStatus: Sendable, Hashable {
    /// Upstream remote name, e.g. `origin`. Empty when `hasUpstream` is false.
    public let remote: String
    /// Fetch URL of the remote, userinfo removed.
    public let url: String
    /// Host (with port when present) parsed from the URL — the Keychain key.
    public let host: String
    /// Short name of the checked-out branch.
    public let branch: String
    public let ahead: Int
    public let behind: Int
    /// Whether a remote with a fetch URL was resolved for the branch: `branch.<name>.remote`,
    /// else `origin` when it exists. True even before a tracking ref exists; `ahead` and
    /// `behind` are then 0.
    public let hasUpstream: Bool
    /// Whether the upstream is an ssh remote, as the guard parsed it. `url` cannot say:
    /// with the user removed, an scp-like `git@host:path` displays as `host:path`.
    public let usesSsh: Bool
}

/// A per-host username and token. Its description, debug description and mirror print the
/// username and `<redacted>` in place of the token, since default reflection would print every
/// stored property. Still never log it.
public struct Credentials: Sendable {
    public let username: String
    public let token: String

    public init(username: String, token: String) {
        self.username = username
        self.token = token
    }
}

extension Credentials: CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    public var description: String {
        "Credentials(username: \(username), token: <redacted>)"
    }

    public var debugDescription: String { description }

    public var customMirror: Mirror {
        Mirror(self, children: ["username": username, "token": "<redacted>"])
    }
}

/// A flag the app raises to stop a fetch or a push that is already running. The guard
/// polls it between steps and inside the ssh transport's reads, so a cancel lands
/// within a fraction of a second instead of waiting for a dead connection to notice.
///
/// One token belongs to one operation. Hold it until that operation returns: the Rust
/// side borrows it for the whole call, and `deinit` frees it.
public final class CancelToken: @unchecked Sendable {
    fileprivate let handle: OpaquePointer?
    private let lock = NSLock()
    private var signalled = false

    public init() {
        handle = rcgit_cancel_new()
    }

    /// Whether [`signal`](signal()) has been called, so a caller unwinding an operation
    /// can tell a cancel from a genuine failure.
    public var isSignalled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return signalled
    }

    /// Asks the running operation to stop. Safe from any thread, and the point of the
    /// type: the thread inside the operation is blocked and cannot ask itself.
    public func signal() {
        lock.lock()
        signalled = true
        lock.unlock()
        rcgit_cancel_signal(handle)
    }

    deinit {
        rcgit_cancel_free(handle)
    }
}

extension Optional where Wrapped == CancelToken {
    /// The C flag, or NULL when the caller passed none.
    fileprivate var pointer: OpaquePointer? { self?.handle }
}

/// A commit or stash refused because it has no author identity: the repository's own config
/// sets no `user.name`/`user.email` and no fallback was passed. Ask for one and retry with it
/// as the fallback.
public struct IdentityRequiredError: Error, Sendable, Hashable, CustomStringConvertible {
    public let message: String

    public var description: String { message }
}

/// How a remote operation failed, typed so the caller can prompt or explain.
public enum SyncError: Error, Sendable, Hashable {
    /// The host wants credentials it did not get, or rejected the given ones. Over ssh
    /// it means every key was offered and none accepted: no passphrase can fix that.
    case authRequired(message: String)
    /// An encrypted ssh key file stayed locked, with no passphrase or the wrong one; `key`
    /// is the file's path, to name in the passphrase prompt.
    case passphraseRequired(message: String, key: String)
    /// Local and remote history diverged; reconcile in git.
    case diverged(message: String)
    /// The ssh host's key is not known; `hostKey` is `"<key type> <fingerprint>"` for
    /// the user to verify before trusting.
    case hostKeyUnknown(message: String, hostKey: String)
    /// The app signalled the operation's `CancelToken`. Nothing was half-applied; the
    /// operation stopped between steps.
    case cancelled(message: String)
    /// Everything else, message ready to show.
    case failure(message: String)

    public var message: String {
        switch self {
        case let .authRequired(message), let .cancelled(message), let .diverged(message),
            let .failure(message), let .hostKeyUnknown(message, _),
            let .passphraseRequired(message, _):
            message
        }
    }
}

/// What a fetch changed.
public struct FetchOutcome: Sendable, Hashable {
    /// Remote-tracking refs created or moved.
    public let updatedRefs: Int
}

/// What a fast-forward pull did.
public struct PullOutcome: Sendable, Hashable {
    /// Files the working tree changed; 0 when the branch was already up to date.
    public let filesChanged: Int
}

/// What a push did.
public struct PushOutcome: Sendable, Hashable {
    /// Commits uploaded; 0 when the remote branch was already up to date.
    public let pushedCommits: Int
}

/// What a branch switch did.
public struct CheckoutOutcome: Sendable, Hashable {
    /// Short name of the branch now checked out.
    public let branch: String
    /// The commit `HEAD` resolves to afterwards.
    public let newHead: Oid
    /// Files the working tree changed.
    public let filesChanged: Int
    /// Whether `HEAD` was already on this branch, so nothing was written.
    public let alreadyOnBranch: Bool
    /// Whether `logs/HEAD` recorded the switch, or none was due. `false` when the append failed
    /// after `HEAD` moved: the switch stands.
    public let reflogWritten: Bool

    /// What the user should read beside a switch that landed without its `logs/HEAD` line,
    /// `nil` when it was written or none was due.
    public var warning: String? {
        guard !reflogWritten else { return nil }
        return "Switched to '\(branch)', but .git/logs/HEAD did not record it, so "
            + "`git checkout -` and HEAD@{1} skip the switch."
    }
}

extension RepositoryGuard {
    /// The checked-out branch's upstream and ahead/behind counts. Local, no network.
    public static func remotePreflight(repoPath: String) throws -> RemoteStatus {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let status = rcgit_guard_remote_preflight(repoPath, &error) else {
            throw RepositoryError.take(error, fallback: "cannot read the upstream status")
        }
        defer { rcgit_remote_status_free(status) }
        let value = status.pointee
        return RemoteStatus(
            remote: value.remote.map { String(cString: $0) } ?? "",
            url: value.url.map { String(cString: $0) } ?? "",
            host: value.host.map { String(cString: $0) } ?? "",
            branch: value.branch.map { String(cString: $0) } ?? "",
            ahead: value.ahead,
            behind: value.behind,
            hasUpstream: value.has_upstream,
            usesSsh: value.uses_ssh
        )
    }

    /// Fetches from the branch's upstream remote. `credentials` may be `nil` for an
    /// anonymous attempt (over ssh, the token field carries the key passphrase); `host` is
    /// the host they were issued for, and an HTTPS 401 from any other host gets none (`nil`:
    /// the host the remote URL names); `acceptedHostKey` is a fingerprint the user approved
    /// for an unknown host.
    public static func fetch(
        repoPath: String, credentials: Credentials?, host: String? = nil,
        acceptedHostKey: String? = nil, cancel: CancelToken? = nil
    ) throws(SyncError) -> FetchOutcome {
        var error: UnsafeMutablePointer<CChar>? = nil
        var hostkey: UnsafeMutablePointer<CChar>? = nil
        var out = RcGitFetchOutcome(updated_refs: 0)
        let status = withOwnedCStrings([
            credentials?.username, credentials?.token, host, acceptedHostKey,
        ]) { strings in
            var auth = RcGitRemoteAuth(
                username: strings[0], token: strings[1], host: strings[2],
                accepted_hostkey: strings[3], ssh_dir: nil)
            return rcgit_guard_fetch(repoPath, &auth, cancel.pointer, &out, &hostkey, &error)
        }
        guard status == RCGIT_OK else {
            throw syncError(status, error, hostkey, fallback: "cannot fetch")
        }
        return FetchOutcome(updatedRefs: out.updated_refs)
    }

    /// Fast-forwards the checked-out branch to its tracking ref. No network; fetch first.
    public static func pullFastForward(repoPath: String) throws(SyncError) -> PullOutcome {
        var error: UnsafeMutablePointer<CChar>? = nil
        var out = RcGitPullOutcome(
            new_head: RcGitOid(), files_changed: 0, already_up_to_date: false)
        let status = rcgit_guard_pull_ff(repoPath, &out, &error)
        guard status == RCGIT_OK else {
            throw syncError(status, error, fallback: "cannot pull")
        }
        return PullOutcome(filesChanged: out.files_changed)
    }

    /// Checks out an existing local branch: working tree, index, then `HEAD`. Refuses the
    /// whole switch when a path it touches has uncommitted work. No network.
    public static func checkout(repoPath: String, branch: String) throws(SyncError)
        -> CheckoutOutcome
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        var out = RcGitCheckoutOutcome(
            new_head: RcGitOid(), files_changed: 0, already_on_branch: false,
            reflog_written: false)
        let status = rcgit_guard_checkout(repoPath, branch, &out, &error)
        guard status == RCGIT_OK else {
            throw syncError(status, error, fallback: "cannot switch to '\(branch)'")
        }
        return CheckoutOutcome(
            branch: branch,
            newHead: Oid(c: out.new_head),
            filesChanged: out.files_changed,
            alreadyOnBranch: out.already_on_branch,
            reflogWritten: out.reflog_written
        )
    }

    /// Pushes the checked-out branch to its upstream. Plain push only. `credentials` and
    /// `host` as for `fetch`.
    public static func push(
        repoPath: String, credentials: Credentials?, host: String? = nil,
        acceptedHostKey: String? = nil, cancel: CancelToken? = nil
    ) throws(SyncError) -> PushOutcome {
        var error: UnsafeMutablePointer<CChar>? = nil
        var hostkey: UnsafeMutablePointer<CChar>? = nil
        var out = RcGitPushOutcome(pushed_commits: 0, already_up_to_date: false)
        let status = withOwnedCStrings([
            credentials?.username, credentials?.token, host, acceptedHostKey,
        ]) { strings in
            var auth = RcGitRemoteAuth(
                username: strings[0], token: strings[1], host: strings[2],
                accepted_hostkey: strings[3], ssh_dir: nil)
            return rcgit_guard_push(repoPath, &auth, cancel.pointer, &out, &hostkey, &error)
        }
        guard status == RCGIT_OK else {
            throw syncError(status, error, hostkey, fallback: "cannot push")
        }
        return PushOutcome(pushedCommits: out.pushed_commits)
    }

    /// The typed error for a failed commit or stash push: `IdentityRequiredError` for
    /// `RCGIT_IDENTITY_REQUIRED`, else `RepositoryError`.
    private static func actionError(
        _ status: Int32, _ error: UnsafeMutablePointer<CChar>?, fallback: String
    ) -> any Error {
        let failure = RepositoryError.take(error, fallback: fallback)
        if status == RCGIT_IDENTITY_REQUIRED {
            return IdentityRequiredError(message: failure.message)
        }
        return failure
    }

    private static func syncError(
        _ status: Int32, _ error: UnsafeMutablePointer<CChar>?,
        _ hostkey: UnsafeMutablePointer<CChar>? = nil, fallback: String
    ) -> SyncError {
        let message = RepositoryError.takeString(error) ?? fallback
        switch status {
        case RCGIT_AUTH_REQUIRED: return .authRequired(message: message)
        case RCGIT_CANCELLED: return .cancelled(message: message)
        case RCGIT_DIVERGED: return .diverged(message: message)
        case RCGIT_HOSTKEY_UNKNOWN:
            return .hostKeyUnknown(
                message: message, hostKey: RepositoryError.takeString(hostkey) ?? "")
        case RCGIT_PASSPHRASE_REQUIRED:
            return .passphraseRequired(
                message: message, key: RepositoryError.takeString(hostkey) ?? "")
        default: return .failure(message: message)
        }
    }
}
