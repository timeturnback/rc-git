import Foundation

/// Who a commit or stash is recorded as: `user.name` and `user.email`.
public struct CommitIdentity: Sendable, Hashable {
    public let name: String
    public let email: String

    public init(name: String, email: String) {
        self.name = name
        self.email = email
    }

    /// `Name <email>`, the way git prints an author.
    public var display: String { "\(name) <\(email)>" }

    /// Turns typed input into an identity git can record. Both fields are trimmed of
    /// surrounding whitespace and newlines; what is left must be non-empty and free of `<`,
    /// `>` and control characters (newlines included). git's signature format puts the
    /// email between angle brackets on one line, so the guard refuses those characters at
    /// write time; checking here keeps a bad identity from ever being saved.
    public static func parse(name: String, email: String) -> Result<CommitIdentity, IdentityProblem>
    {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let email = email.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty { return .failure(.empty(.name)) }
        if email.isEmpty { return .failure(.empty(.email)) }
        if !isRecordable(name) { return .failure(.forbiddenCharacter(.name)) }
        if !isRecordable(email) { return .failure(.forbiddenCharacter(.email)) }
        return .success(CommitIdentity(name: name, email: email))
    }

    private static func isRecordable(_ field: String) -> Bool {
        field.unicodeScalars.allSatisfy { scalar in
            scalar != "<" && scalar != ">" && scalar.properties.generalCategory != .control
        }
    }
}

/// Why typed input is not an identity git can record.
public enum IdentityProblem: Error, Sendable, Hashable {
    public enum Field: Sendable, Hashable {
        case name
        case email
    }

    case empty(Field)
    case forbiddenCharacter(Field)

    /// One sentence the identity sheet shows under its fields.
    public var message: String {
        switch self {
        case .empty(.name): "Enter a name."
        case .empty(.email): "Enter an email address."
        case .forbiddenCharacter(.name):
            "The name cannot contain “<”, “>”, or line breaks. Enter the email in its own field."
        case .forbiddenCharacter(.email):
            "Enter the email without “<” or “>”, and on one line."
        }
    }
}

/// The identity rc-git remembers for each repository, kept in `UserDefaults` under `key` as
/// a dictionary from repository path to name and email. It is only ever a fallback: the guard
/// records it when that repository's own config sets no `user.name`/`user.email`, and one
/// repository's answer never applies to another.
public struct CommitIdentityStore {
    public static let key = "rc-git.commitIdentities"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The identity saved for `repoPath`. A stored value that no longer parses is treated as
    /// absent, so it can never break a commit.
    public func identity(for repoPath: String) -> CommitIdentity? {
        guard let fields = entries[Self.normalized(repoPath)],
            let name = fields["name"], let email = fields["email"],
            case let .success(identity) = CommitIdentity.parse(name: name, email: email)
        else { return nil }
        return identity
    }

    public func save(_ identity: CommitIdentity, for repoPath: String) {
        var updated = entries
        updated[Self.normalized(repoPath)] = ["name": identity.name, "email": identity.email]
        defaults.set(updated, forKey: Self.key)
    }

    public func clear(for repoPath: String) {
        var updated = entries
        updated[Self.normalized(repoPath)] = nil
        defaults.set(updated, forKey: Self.key)
    }

    private var entries: [String: [String: String]] {
        defaults.dictionary(forKey: Self.key) as? [String: [String: String]] ?? [:]
    }

    /// One key per folder however the path was spelled: no trailing slash, no `.` or `..`.
    private static func normalized(_ repoPath: String) -> String {
        (repoPath as NSString).standardizingPath
    }
}

/// Who the next commit or stash will be recorded as, decided in the guard's order: the
/// repository's own config, else the identity saved for it, else nobody yet.
public enum CommitAuthor: Sendable, Hashable {
    case configured(CommitIdentity)
    case saved(CommitIdentity)
    case unset
    /// The repository's config could not be read; the message says why.
    case unreadable(String)

    public static func resolve(configured: CommitIdentity?, saved: CommitIdentity?)
        -> CommitAuthor
    {
        if let configured { return .configured(configured) }
        if let saved { return .saved(saved) }
        return .unset
    }

    /// The line shown next to the Commit button.
    public var label: String {
        switch self {
        case let .configured(identity): "Author: \(identity.display) (repository config)"
        case let .saved(identity): "Author: \(identity.display) (saved in rc-git)"
        case .unset: "Author: none set; Commit asks for one"
        case .unreadable: "Author: could not read this repository’s config"
        }
    }
}

/// Keeps `author` current for one repository. Reads of the repository's config run off the
/// main actor and carry a generation, so a slow read never overwrites a newer answer.
@MainActor
public final class CommitAuthorModel {
    public let repoPath: String
    /// `nil` until the first read lands.
    public private(set) var author: CommitAuthor?
    public var onChange: (() -> Void)?

    private let store: CommitIdentityStore
    private let readConfigured: @Sendable (String) throws -> CommitIdentity?
    private var generation = 0

    public init(
        repoPath: String, store: CommitIdentityStore = CommitIdentityStore(),
        readConfigured: @escaping @Sendable (String) throws -> CommitIdentity? = {
            try RepositoryGuard.configuredIdentity(repoPath: $0)
        }
    ) {
        self.repoPath = repoPath
        self.store = store
        self.readConfigured = readConfigured
    }

    /// The fallback to hand the guard: the identity saved for this repository.
    public var saved: CommitIdentity? { store.identity(for: repoPath) }

    /// Re-reads the repository's config and re-decides `author`.
    public func refresh() async {
        generation += 1
        let current = generation
        let path = repoPath
        let read = readConfigured
        let result = await RepositoryError.run { try read(path) }
        guard current == generation else { return }
        switch result {
        case let .success(configured):
            author = .resolve(configured: configured, saved: saved)
        case let .failure(error):
            author = .unreadable(error.message)
        }
        onChange?()
    }

    public func save(_ identity: CommitIdentity) async {
        store.save(identity, for: repoPath)
        await refresh()
    }

    public func clear() async {
        store.clear(for: repoPath)
        await refresh()
    }
}
