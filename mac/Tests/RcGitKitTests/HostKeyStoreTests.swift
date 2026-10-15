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

    @Test func approvalsListEveryStringEntry() throws {
        try withScratchDefaults { defaults in
            defaults.set(["good.test": "SHA256:aaa", "bad.test": 42], forKey: Self.key)
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            let good = HostKeyApproval(host: "good.test", fingerprint: "SHA256:aaa")
            #expect(store.approvals() == [good])
        }
    }
}

@MainActor
@Suite struct ApprovedHostsModelTests {
    @Test func rowsAreSortedByHost() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            store.store(fingerprint: "SHA256:bbb", host: "host10.test")
            store.store(fingerprint: "SHA256:aaa", host: "host2.test")
            store.store(fingerprint: "SHA256:ccc", host: "Alpha.test")
            let model = ApprovedHostsModel(store: store)
            #expect(model.rows.map(\.host) == ["Alpha.test", "host2.test", "host10.test"])
        }
    }

    @Test func removeForgetsOnlyTheChosenHosts() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            store.store(fingerprint: "SHA256:aaa", host: "one.test")
            store.store(fingerprint: "SHA256:bbb", host: "two.test")
            store.store(fingerprint: "SHA256:ccc", host: "three.test")
            let model = ApprovedHostsModel(store: store)
            var changes = 0
            model.onChanged = { changes += 1 }

            model.remove(at: [0, 2])

            #expect(store.fingerprint(host: "one.test") == nil)
            #expect(store.fingerprint(host: "two.test") == nil)
            #expect(store.fingerprint(host: "three.test") == "SHA256:ccc")
            #expect(
                model.rows == [HostKeyApproval(host: "three.test", fingerprint: "SHA256:ccc")])
            #expect(changes == 1)
        }
    }

    @Test func removeIgnoresAnIndexPastTheEnd() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            store.store(fingerprint: "SHA256:aaa", host: "one.test")
            let model = ApprovedHostsModel(store: store)
            var changes = 0
            model.onChanged = { changes += 1 }

            model.remove(at: [1, -1])

            #expect(store.fingerprint(host: "one.test") == "SHA256:aaa")
            #expect(changes == 0)
        }
    }

    @Test func reloadPicksUpAnApprovalMadeElsewhere() throws {
        try withScratchDefaults { defaults in
            let store = UserDefaultsHostKeyStore(defaults: defaults)
            let model = ApprovedHostsModel(store: store)
            #expect(model.rows.isEmpty)
            store.store(fingerprint: "SHA256:aaa", host: "one.test")
            model.reload()
            #expect(
                model.rows == [HostKeyApproval(host: "one.test", fingerprint: "SHA256:aaa")])
        }
    }
}
