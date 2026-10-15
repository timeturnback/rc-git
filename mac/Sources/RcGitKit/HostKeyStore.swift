import Foundation

/// One host the user approved, and the fingerprint they approved for it.
public struct HostKeyApproval: Hashable, Sendable {
    public let host: String
    public let fingerprint: String

    public init(host: String, fingerprint: String) {
        self.host = host
        self.fingerprint = fingerprint
    }
}

/// Fingerprints of ssh host keys the user chose to trust, one per host. rc-git never
/// writes `~/.ssh/known_hosts`; approvals live app-side. The production implementation
/// is `UserDefaults` — fingerprints are public values, not secrets.
public protocol HostKeyStoring: Sendable {
    func fingerprint(host: String) -> String?
    func store(fingerprint: String, host: String)
    /// Drops the approval for `host`, so a legitimately rotated key can be approved anew.
    func forget(host: String)
    /// Every approval, in no particular order; `ApprovedHostsModel` sorts them for display.
    func approvals() -> [HostKeyApproval]
}

/// `UserDefaults`-backed store: one dictionary of host → `SHA256:...` fingerprint.
///
/// `@unchecked Sendable` because `UserDefaults` is documented thread-safe and the struct holds
/// nothing else. Entries are read one by one, so a value that is not a string under one host
/// neither hides nor wipes the approvals stored for the others.
public struct UserDefaultsHostKeyStore: HostKeyStoring, @unchecked Sendable {
    private static let key = "rcgit.acceptedHostKeys"
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func fingerprint(host: String) -> String? {
        stored()[host]
    }

    public func store(fingerprint: String, host: String) {
        var entries = stored()
        entries[host] = fingerprint
        defaults.set(entries, forKey: Self.key)
    }

    public func forget(host: String) {
        var entries = stored()
        entries[host] = nil
        defaults.set(entries, forKey: Self.key)
    }

    public func approvals() -> [HostKeyApproval] {
        stored().map { HostKeyApproval(host: $0.key, fingerprint: $0.value) }
    }

    private func stored() -> [String: String] {
        (defaults.dictionary(forKey: Self.key) ?? [:]).compactMapValues { $0 as? String }
    }
}

/// The approved ssh hosts as Settings lists them, sorted by host, with Remove. Removing an
/// approval is how a user recovers from a legitimately rotated key: the next fetch from that
/// host meets the new key as unknown and offers the trust sheet again, rather than refusing it
/// as a changed key.
@MainActor
public final class ApprovedHostsModel {
    public private(set) var rows: [HostKeyApproval] = []
    public var onChanged: (() -> Void)?
    private let store: HostKeyStoring

    public init(store: HostKeyStoring = UserDefaultsHostKeyStore()) {
        self.store = store
        rows = Self.sorted(store.approvals())
    }

    /// Re-reads the store: a window may have approved a host since the list was last shown.
    public func reload() {
        rows = Self.sorted(store.approvals())
        onChanged?()
    }

    /// Forgets the approvals at `indexes` into `rows`; an index past the end is ignored.
    public func remove(at indexes: [Int]) {
        let hosts = indexes.filter(rows.indices.contains).map { rows[$0].host }
        guard !hosts.isEmpty else { return }
        for host in hosts {
            store.forget(host: host)
        }
        reload()
    }

    static func sorted(_ approvals: [HostKeyApproval]) -> [HostKeyApproval] {
        approvals.sorted {
            $0.host.localizedStandardCompare($1.host) == .orderedAscending
        }
    }
}
