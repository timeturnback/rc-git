import Foundation
import Testing

@testable import RcGitKit

@Suite struct HostKeyStoreTests {
    private static let key = "rcgit.acceptedHostKeys"

    @Test func unknownHostHasNoFingerprint() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            #expect(store.fingerprint(host: "example.test") == nil)
        }
    }

    @Test func storedFingerprintReadsBack() throws {
        try withScratchDefaults { defaults in
            UserDefaultsHostKeyStore(defaults: defaults)
                .store(fingerprint: "SHA256:aaa", host: "example.test")
            let reopened = UserDefaultsHostKeyStore(defaults: defaults)
            #expect(reopened.fingerprint(host: "example.test") == "SHA256:aaa")
        }
    }

    @Test func hostsStayIndependent() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            store.store(fingerprint: "SHA256:aaa", host: "one.test")
            store.store(fingerprint: "SHA256:bbb", host: "two.test")
            #expect(store.fingerprint(host: "one.test") == "SHA256:aaa")
            #expect(store.fingerprint(host: "two.test") == "SHA256:bbb")
        }
    }

    @Test func overwritingOneHostLeavesTheOther() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            store.store(fingerprint: "SHA256:aaa", host: "one.test")
            store.store(fingerprint: "SHA256:bbb", host: "two.test")
            store.store(fingerprint: "SHA256:ccc", host: "one.test")
            #expect(store.fingerprint(host: "one.test") == "SHA256:ccc")
            #expect(store.fingerprint(host: "two.test") == "SHA256:bbb")
        }
    }

    @Test func forgettingOneHostLeavesTheOther() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            store.store(fingerprint: "SHA256:aaa", host: "one.test")
            store.store(fingerprint: "SHA256:bbb", host: "two.test")
            store.forget(host: "one.test")
            #expect(store.fingerprint(host: "one.test") == nil)
            #expect(store.fingerprint(host: "two.test") == "SHA256:bbb")
        }
    }

    @Test func nonStringValueHidesNoOtherHost() throws {
        try withScratchDefaults { defaults in
            defaults.set(["good.test": "SHA256:aaa", "bad.test": 42], forKey: Self.key)
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            #expect(store.fingerprint(host: "good.test") == "SHA256:aaa")
            #expect(store.fingerprint(host: "bad.test") == nil)

            store.store(fingerprint: "SHA256:bbb", host: "new.test")
            #expect(store.fingerprint(host: "good.test") == "SHA256:aaa")
            #expect(store.fingerprint(host: "new.test") == "SHA256:bbb")
        }
    }
}
