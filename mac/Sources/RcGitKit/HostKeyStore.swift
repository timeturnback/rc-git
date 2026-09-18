import Foundation

/// Fingerprints of ssh host keys the user chose to trust, one per host. rc-git never
/// writes `~/.ssh/known_hosts`; approvals live app-side. The production implementation
/// is `UserDefaults` — fingerprints are public values, not secrets.
public protocol HostKeyStoring: Sendable {
    func fingerprint(host: String) -> String?
    func store(fingerprint: String, host: String)
}

/// `UserDefaults`-backed store: one dictionary of host → `SHA256:...` fingerprint.
public struct UserDefaultsHostKeyStore: HostKeyStoring {
    private static let key = "rcgit.acceptedHostKeys"

    public init() {}

    public func fingerprint(host: String) -> String? {
        let stored = UserDefaults.standard.dictionary(forKey: Self.key) as? [String: String]
        return stored?[host]
    }

    public func store(fingerprint: String, host: String) {
        var stored =
            UserDefaults.standard.dictionary(forKey: Self.key) as? [String: String] ?? [:]
        stored[host] = fingerprint
        UserDefaults.standard.set(stored, forKey: Self.key)
    }
}
