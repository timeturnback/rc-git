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

/// Who a commit or stash is recorded as, when the repository's own config sets no identity.
public struct CommitIdentity: Sendable, Hashable {
    public let name: String
    public let email: String

    public init(name: String, email: String) {
        self.name = name
        self.email = email
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
    /// Only the given hunks of a tracked, modified text file.
    case hunks(path: String, stamps: [HunkStamp])

    var path: String {
        switch self {
        case let .whole(path), let .hunks(path, _): path
        }
    }
}

/// What a completed commit produced.
public struct CommitOutcome: Sendable, Hashable {
    /// Hex id of the new commit.
    public let oid: String
    /// Short name of the branch that advanced.
    public let branch: String
}

/// The one write path: branch and tag management, gated by the guard crate's policy. Every
/// call opens the repository fresh by path, so nothing here can touch the read-only handles.
public enum RepositoryGuard {
    /// What deleting a branch would need.
    public static func planBranchDelete(repoPath: String, name: String) throws -> GuardDecision {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let plan = rcgit_guard_plan_branch_delete(repoPath, name, &error) else {
            throw take(error, fallback: "cannot plan deleting '\(name)'")
        }
        defer { rcgit_guard_decision_free(plan) }
        return GuardDecision(c: plan)
    }

    /// What deleting a tag would need. Pure policy.
    public static func planTagDelete(name: String) -> GuardDecision? {
        guard let plan = rcgit_guard_plan_tag_delete(name) else { return nil }
        defer { rcgit_guard_decision_free(plan) }
        return GuardDecision(c: plan)
    }

    /// Creates a branch at `revspec`, never overwriting an existing one.
    public static func createBranch(repoPath: String, name: String, revspec: String) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_branch_create(repoPath, name, revspec, &error) == RCGIT_OK else {
            throw take(error, fallback: "cannot create branch '\(name)'")
        }
    }

    /// Creates a lightweight tag at `revspec`, never overwriting an existing one.
    public static func createTag(repoPath: String, name: String, revspec: String) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_tag_create(repoPath, name, revspec, &error) == RCGIT_OK else {
            throw take(error, fallback: "cannot create tag '\(name)'")
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
            throw take(error, fallback: "cannot delete branch '\(name)'")
        }
        return takeString(backup)
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
            throw take(error, fallback: "cannot delete tag '\(name)'")
        }
        return takeString(backup)
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

    /// What popping a stash entry would need. Pure policy.
    public static func planStashPop() -> GuardDecision? {
        decision(rcgit_guard_plan_stash_pop())
    }

    /// What dropping `stash@{index}` would need. Pure policy.
    public static func planStashDrop(index: Int) -> GuardDecision? {
        decision(rcgit_guard_plan_stash_drop(index))
    }

    /// What discarding changes in `files` files would need. Pure policy.
    public static func planDiscard(files: Int) -> GuardDecision? {
        decision(rcgit_guard_plan_discard(files))
    }

    /// Commits `selections` to the checked-out branch. `expectedHead` is the commit the
    /// change list was built against, `nil` on an unborn branch; the commit is refused when
    /// `HEAD` moved. `author` fills in when the repository's own config has no identity.
    public static func commit(
        repoPath: String, message: String, author: CommitIdentity?, expectedHead: String?,
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
        let result = withOwnedCStrings([message, author?.name, author?.email, expectedHead, input])
        { strings in
            files.withUnsafeBufferPointer { buffer in
                rcgit_guard_commit(
                    repoPath, strings[0], strings[1], strings[2], strings[3],
                    buffer.baseAddress, files.count, strings[4], &error)
            }
        }
        guard let result else {
            throw take(error, fallback: "cannot commit")
        }
        defer { rcgit_commit_result_free(result) }
        return CommitOutcome(
            oid: String(cString: result.pointee.oid),
            branch: String(cString: result.pointee.branch))
    }

    /// Stashes the changes at `paths` and restores those files to `HEAD` state.
    public static func stashPush(
        repoPath: String, paths: [String], message: String?, author: CommitIdentity?,
        input: String?
    ) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        let ownedPaths = paths.map { strdup($0) }
        defer { ownedPaths.forEach { free($0) } }
        let pathPointers = ownedPaths.map { UnsafePointer($0) }
        let status = withOwnedCStrings([message, author?.name, author?.email, input]) { strings in
            pathPointers.withUnsafeBufferPointer { buffer in
                rcgit_guard_stash_push(
                    repoPath, buffer.baseAddress, paths.count, strings[0], strings[1],
                    strings[2], strings[3], &error)
            }
        }
        guard status == RCGIT_OK else {
            throw take(error, fallback: "cannot stash")
        }
    }

    /// Writes `stash@{index}` back to the working tree, keeping the entry.
    public static func stashApply(repoPath: String, index: Int, input: String?) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_stash_apply(repoPath, index, input, &error) == RCGIT_OK else {
            throw take(error, fallback: "cannot apply the stash")
        }
    }

    /// Applies `stash@{index}` and drops it. Returns the backup ref, if written.
    @discardableResult
    public static func stashPop(repoPath: String, index: Int, input: String?) throws -> String? {
        var backup: UnsafeMutablePointer<CChar>? = nil
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_stash_pop(repoPath, index, input, &backup, &error) == RCGIT_OK else {
            throw take(error, fallback: "cannot pop the stash")
        }
        return takeString(backup)
    }

    /// Deletes `stash@{index}` without applying it. Returns the backup ref, if written.
    @discardableResult
    public static func stashDrop(repoPath: String, index: Int, input: String?) throws -> String? {
        var backup: UnsafeMutablePointer<CChar>? = nil
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_guard_stash_drop(repoPath, index, input, &backup, &error) == RCGIT_OK else {
            throw take(error, fallback: "cannot drop the stash")
        }
        return takeString(backup)
    }

    /// Discards every change at `paths`; `input` answers the typed-phrase challenge.
    public static func discard(repoPath: String, paths: [String], input: String?) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        let ownedPaths = paths.map { strdup($0) }
        defer { ownedPaths.forEach { free($0) } }
        let pathPointers = ownedPaths.map { UnsafePointer($0) }
        let status = withOwnedCStrings([input]) { strings in
            pathPointers.withUnsafeBufferPointer { buffer in
                rcgit_guard_discard(repoPath, buffer.baseAddress, paths.count, strings[0], &error)
            }
        }
        guard status == RCGIT_OK else {
            throw take(error, fallback: "cannot discard")
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
            throw take(error, fallback: "cannot discard hunks of '\(path)'")
        }
    }

    private static func decision(_ pointer: UnsafeMutablePointer<RcGitGuardDecision>?)
        -> GuardDecision?
    {
        guard let pointer else { return nil }
        defer { rcgit_guard_decision_free(pointer) }
        return GuardDecision(c: pointer)
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

    private static func take(_ error: UnsafeMutablePointer<CChar>?, fallback: String)
        -> RepositoryError
    {
        RepositoryError(takeString(error) ?? fallback)
    }

    private static func takeString(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { rcgit_string_free(pointer) }
        return String(cString: pointer)
    }
}
