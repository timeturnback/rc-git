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
