import Foundation
import Security

/// Stores one username+token per HTTPS host. The production implementation is the login
/// keychain; tests inject an in-memory double.
public protocol CredentialStoring: Sendable {
    func credentials(host: String) throws -> Credentials?
    func store(_ credentials: Credentials, host: String) throws
    func delete(host: String) throws
}

/// The login keychain, one `kSecClassInternetPassword` item per host: the host is
/// `kSecAttrServer`, the username rides along as `kSecAttrAccount`, and the token is the
/// item data. Kept deliberately thin — every branch is one SecItem call — because the real
/// keychain cannot run under `swift test`.
///
/// The app is ad-hoc signed, so each rebuild is a new code identity and the keychain
/// re-prompts per host ("Always Allow" makes it once per rebuild). A stable signing
/// identity in `scripts/build-app.sh` is the real fix.
public struct KeychainCredentialStore: CredentialStoring {
    public init() {}

    public func credentials(host: String) throws -> Credentials? {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            query(host: host).merging([
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

    public func store(_ credentials: Credentials, host: String) throws {
        let attributes: [String: Any] = [
            kSecAttrAccount as String: credentials.username,
            kSecValueData as String: Data(credentials.token.utf8),
            kSecAttrLabel as String: "rc-git — \(host)",
        ]
        let add = SecItemAdd(
            query(host: host).merging(attributes) { _, new in new } as CFDictionary, nil)
        if add == errSecSuccess { return }
        guard add == errSecDuplicateItem else {
            throw RepositoryError("keychain write failed for '\(host)' (status \(add))")
        }
        let update = SecItemUpdate(
            query(host: host) as CFDictionary, attributes as CFDictionary)
        guard update == errSecSuccess else {
            throw RepositoryError("keychain update failed for '\(host)' (status \(update))")
        }
    }

    public func delete(host: String) throws {
        let status = SecItemDelete(query(host: host) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw RepositoryError("keychain delete failed for '\(host)' (status \(status))")
        }
    }

    private func query(host: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassInternetPassword,
            kSecAttrServer as String: host,
            kSecAttrProtocol as String: kSecAttrProtocolHTTPS,
        ]
    }
}
