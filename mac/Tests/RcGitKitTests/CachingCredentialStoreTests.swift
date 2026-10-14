import Foundation
import Testing

@testable import RcGitKit

/// Counts every call into it; a read can be told to throw.
private final class CountingCredentialStore: CredentialStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String: Credentials] = [:]
    private var reads = 0
    private var writes = 0
    private var failingReads = 0

    init(_ items: [String: Credentials] = [:]) {
        self.items = items
    }

    var readCount: Int { lock.withLock { reads } }
    var writeCount: Int { lock.withLock { writes } }

    /// The next `count` reads throw.
    func failNextReads(_ count: Int) {
        lock.withLock { failingReads = count }
    }

    func credentials(host: String, kind: CredentialKind) throws -> Credentials? {
        try lock.withLock {
            reads += 1
            if failingReads > 0 {
                failingReads -= 1
                throw RepositoryError("keychain read failed for '\(host)' (status -25293)")
            }
            return items[host]
        }
    }

    func store(_ credentials: Credentials, host: String, kind: CredentialKind) throws {
        lock.withLock {
            writes += 1
            items[host] = credentials
        }
    }
}

@Suite struct CachingCredentialStoreTests {
    private let saved = Credentials(username: "user", token: "saved")

    @Test func twoReadsCostOneInnerRead() throws {
        let inner = CountingCredentialStore(["example.com": saved])
        let cache = CachingCredentialStore(wrapping: inner)

        let first = try cache.credentials(host: "example.com", kind: .httpsToken)
        let second = try cache.credentials(host: "example.com", kind: .httpsToken)

        #expect(first?.token == "saved")
        #expect(second?.token == "saved")
        #expect(inner.readCount == 1)
    }

    @Test func aThrowingReadIsNotCached() throws {
        let inner = CountingCredentialStore(["example.com": saved])
        inner.failNextReads(1)
        let cache = CachingCredentialStore(wrapping: inner)

        #expect(throws: RepositoryError.self) {
            try cache.credentials(host: "example.com", kind: .httpsToken)
        }
        let retried = try cache.credentials(host: "example.com", kind: .httpsToken)

        #expect(retried?.token == "saved")
        #expect(inner.readCount == 2)
    }

    @Test func aMissIsAskedAgain() throws {
        let inner = CountingCredentialStore()
        let cache = CachingCredentialStore(wrapping: inner)

        #expect(try cache.credentials(host: "example.com", kind: .httpsToken) == nil)
        #expect(try cache.credentials(host: "example.com", kind: .httpsToken) == nil)
        #expect(inner.readCount == 2)
    }

    @Test func aStoredSecretIsReadBackWithoutAnInnerRead() throws {
        let inner = CountingCredentialStore()
        let cache = CachingCredentialStore(wrapping: inner)

        try cache.store(saved, host: "example.com", kind: .httpsToken)
        let read = try cache.credentials(host: "example.com", kind: .httpsToken)

        #expect(read?.token == "saved")
        #expect(inner.writeCount == 1)
        #expect(inner.readCount == 0)
    }

    @Test func kindsAreCachedApart() throws {
        let inner = CountingCredentialStore(["example.com": saved])
        let cache = CachingCredentialStore(wrapping: inner)

        _ = try cache.credentials(host: "example.com", kind: .httpsToken)
        _ = try cache.credentials(host: "example.com", kind: .sshPassphrase)

        #expect(inner.readCount == 2)
    }
}
