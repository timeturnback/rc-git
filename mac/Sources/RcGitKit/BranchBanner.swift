/// What the sidebar banner names in the modes that show it instead of the ref list. Files
/// names what the tree browses: the working tree and the branch under it, one commit picked
/// from the history, or a ref. Changes names the branch `HEAD` is on, because the change list
/// always compares the working tree with `HEAD`, whatever Files browses.
public struct BranchBanner: Equatable, Sendable {
    /// Which icon stands beside the title.
    public enum Symbol: Equatable, Sendable {
        case ref(RefKind)
        case commit
        case workingTree
        case detached
        case nothing
    }

    public let symbol: Symbol
    public let title: String
    /// A quieter second line: the branch under the working tree, or a commit's message.
    public let detail: String?

    public init(symbol: Symbol, title: String, detail: String? = nil) {
        self.symbol = symbol
        self.title = title
        self.detail = detail
    }

    /// Nothing is browsed yet: the repository is still loading, or it has no refs.
    public static let nothing = BranchBanner(symbol: .nothing, title: "No branch")

    /// The banner as one line, for the window subtitle. Empty when nothing is browsed.
    public var summary: String {
        guard symbol != .nothing else { return "" }
        guard let detail else { return title }
        return "\(title) — \(detail)"
    }

    /// What Files browses, in the order the model holds it: the working tree, then a commit
    /// from the history, then the selected ref.
    public static func browsing(
        selectedRef: RefEntry?, browsedCommit: CommitSummary?, browsingWorkingTree: Bool,
        head: Head?
    ) -> BranchBanner {
        if browsingWorkingTree {
            return BranchBanner(symbol: .workingTree, title: "Working Tree", detail: under(head))
        }
        if let commit = browsedCommit {
            return BranchBanner(symbol: .commit, title: commit.oid.short, detail: commit.title)
        }
        if let ref = selectedRef {
            return BranchBanner(symbol: .ref(ref.kind), title: ref.shortName)
        }
        return .nothing
    }

    /// The branch `HEAD` is on, for Changes.
    public static func checkedOut(head: Head?) -> BranchBanner {
        if let name = head?.refName {
            let unborn = head?.target == nil
            return BranchBanner(
                symbol: .ref(.localBranch), title: branchName(name),
                detail: unborn ? "No commits yet" : nil)
        }
        if let target = head?.target {
            return BranchBanner(symbol: .detached, title: "Detached HEAD", detail: target.short)
        }
        return .nothing
    }

    /// "on main", or "detached at 1a2b3c4", for the line under the working tree.
    private static func under(_ head: Head?) -> String? {
        if let name = head?.refName {
            return "on \(branchName(name))"
        }
        if let target = head?.target {
            return "detached at \(target.short)"
        }
        return nil
    }

    /// `HEAD` names its branch in full; the banner shows it the way the ref list does.
    private static func branchName(_ refName: String) -> String {
        let prefix = "refs/heads/"
        return refName.hasPrefix(prefix) ? String(refName.dropFirst(prefix.count)) : refName
    }
}
