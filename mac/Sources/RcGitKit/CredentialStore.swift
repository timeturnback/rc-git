import Foundation
import Security

/// What a stored secret is for, so one host can hold both an HTTPS token and an ssh key
/// passphrase without colliding.
public enum CredentialKind: Sendable, Hashable {
    case httpsToken
    case sshPassphrase
}

/// Stores one secret per host and kind: username+token for HTTPS, the key passphrase
/// (username empty) for ssh. The production implementation is the login keychain;
/// tests inject an in-memory double.
public protocol CredentialStoring: Sendable {
    func credentials(host: String, kind: CredentialKind) throws -> Credentials?
    func store(_ credentials: Credentials, host: String, kind: CredentialKind) throws
}

/// The login keychain, one rc-git `kSecClassInternetPassword` item per host and kind: the
/// host is `kSecAttrServer`, the kind picks `kSecAttrProtocol`, the username rides along as
/// `kSecAttrAccount`, and the token is the item data. Every query carries the security domain
/// `rc-git`, part of an internet password's primary key, so it never reads, overwrites or
/// deletes another program's item for the same host (git-credential-osxkeychain, Safari).
/// Items from builds before the domain are no longer read; the user enters the token again.
/// Kept deliberately thin — every branch is one SecItem call — because the real
/// keychain cannot run under `swift test`.
///
/// The app is ad-hoc signed, so each rebuild is a new code identity and the keychain
/// re-prompts per host ("Always Allow" makes it once per rebuild). A stable signing
/// identity in `scripts/build-app.sh` is the real fix.
public struct KeychainCredentialStore: CredentialStoring {
    static let securityDomain = "rc-git"

    public init() {}

    public func credentials(host: String, kind: CredentialKind) throws -> Credentials? {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            Self.query(host: host, kind: kind).merging([
                kSecReturnAttributes as String: true,
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]) { current, _ in current } as CFDictionary,
            &result
        )
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess,
            let item = result as? [String: Any],
            let data = item[kSecValueData as String] as? Data,
            let token = String(data: data, encoding: .utf8)
        else {
            throw RepositoryError("keychain read failed for '\(host)' (status \(status))")
        }
        let username = item[kSecAttrAccount as String] as? String ?? ""
        return Credentials(username: username, token: token)
    }

    public func store(_ credentials: Credentials, host: String, kind: CredentialKind) throws {
        let label =
            switch kind {
            case .httpsToken: "rc-git — \(host)"
            case .sshPassphrase: "rc-git — \(host) (ssh key)"
            }
        let attributes: [String: Any] = [
            kSecAttrAccount as String: credentials.username,
            kSecValueData as String: Data(credentials.token.utf8),
            kSecAttrLabel as String: label,
        ]
        let removed = SecItemDelete(Self.query(host: host, kind: kind) as CFDictionary)
        guard removed == errSecSuccess || removed == errSecItemNotFound else {
            throw RepositoryError("keychain write failed for '\(host)' (status \(removed))")
        }
        let add = SecItemAdd(
            Self.query(host: host, kind: kind).merging(attributes) { _, new in new }
                as CFDictionary,
            nil)
        guard add == errSecSuccess else {
            throw RepositoryError("keychain write failed for '\(host)' (status \(add))")
        }
    }

    /// The attributes that identify rc-git's item for `host` and `kind`.
    static func query(host: String, kind: CredentialKind) -> [String: Any] {
        let protocolAttr =
            switch kind {
            case .httpsToken: kSecAttrProtocolHTTPS
            case .sshPassphrase: kSecAttrProtocolSSH
            }
        return [
            kSecClass as String: kSecClassInternetPassword,
            kSecAttrServer as String: host,
            kSecAttrProtocol as String: protocolAttr,
            kSecAttrSecurityDomain as String: securityDomain,
        ]
    }
}

/// Remembers what another store answered, so one launch asks the keychain for each secret
/// once: under an ad-hoc signature every keychain read can raise an access dialog, and
/// "Allow" covers a single read. Only a found secret is kept; a miss and a failure are asked
/// again next time. Writes go through to the wrapped store first.
///
/// `@unchecked Sendable` because an `NSLock` guards `cache`, and the wrapped store is called
/// only while holding it, so concurrent misses for one secret share a single read.
public final class CachingCredentialStore: CredentialStoring, @unchecked Sendable {
    private struct Key: Hashable {
        let host: String
        let kind: CredentialKind
    }

    private let inner: CredentialStoring
    private let lock = NSLock()
    private var cache: [Key: Credentials] = [:]

    public init(wrapping inner: CredentialStoring) {
        self.inner = inner
    }

    public func credentials(host: String, kind: CredentialKind) throws -> Credentials? {
        let key = Key(host: host, kind: kind)
        return try lock.withLock {
            if let hit = cache[key] { return hit }
            let found = try inner.credentials(host: host, kind: kind)
            if let found { cache[key] = found }
            return found
        }
    }

    public func store(_ credentials: Credentials, host: String, kind: CredentialKind) throws {
        try lock.withLock {
            try inner.store(credentials, host: host, kind: kind)
            cache[Key(host: host, kind: kind)] = credentials
        }
    }
}
