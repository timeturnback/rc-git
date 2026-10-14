import Foundation
import Testing

@testable import RcGitKit

@Suite struct CommitIdentityParseTests {
    @Test func aValidIdentityIsTrimmedOfSpacesAndNewlines() {
        let parsed = CommitIdentity.parse(name: "  Ada Lovelace\n", email: "\tlong@example.com \n")
        #expect(parsed == .success(CommitIdentity(name: "Ada Lovelace", email: "long@example.com")))
    }

    @Test func angleBracketsInTheNameAreRefused() {
        let parsed = CommitIdentity.parse(name: "Long <long@example.com>", email: "x@example.com")
        #expect(parsed == .failure(.forbiddenCharacter(.name)))
        for name in ["Long <", "Long >"] {
            #expect(
                CommitIdentity.parse(name: name, email: "x@example.com")
                    == .failure(.forbiddenCharacter(.name)), "\(name)")
        }
    }

    @Test func angleBracketsAroundTheEmailAreRefused() {
        let parsed = CommitIdentity.parse(name: "Long", email: "<long@example.com>")
        #expect(parsed == .failure(.forbiddenCharacter(.email)))
    }

    @Test func anEmbeddedNewlineOrControlCharacterIsRefused() {
        #expect(
            CommitIdentity.parse(name: "Ada\nLovelace", email: "long@example.com")
                == .failure(.forbiddenCharacter(.name)))
        #expect(
            CommitIdentity.parse(name: "Long", email: "long@\u{7}example.com")
                == .failure(.forbiddenCharacter(.email)))
    }

    @Test func emptyFieldsAreRefusedAfterTrimming() {
        #expect(CommitIdentity.parse(name: "  ", email: "a@b.c") == .failure(.empty(.name)))
        #expect(CommitIdentity.parse(name: "Long", email: "\n") == .failure(.empty(.email)))
    }

    @Test func nonAsciiNamesAndEmojiJoinersAreKept() {
        let name = "Nguyễn Văn A 👩‍💻"
        #expect(
            CommitIdentity.parse(name: name, email: "long@example.com")
                == .success(CommitIdentity(name: name, email: "long@example.com")))
    }
}

@Suite struct CommitIdentityStoreTests {
    let work = CommitIdentity(name: "Work Me", email: "me@work.example")

    @Test func anIdentityIsSavedForOneRepositoryOnly() throws {
        try withScratchDefaults { defaults in
            let store = CommitIdentityStore(defaults: defaults)
            store.save(work, for: "/repos/work")
            #expect(store.identity(for: "/repos/work") == work)
            #expect(store.identity(for: "/repos/work/") == work, "spelling does not split keys")
            #expect(store.identity(for: "/repos/other") == nil)
            store.clear(for: "/repos/work")
            #expect(store.identity(for: "/repos/work") == nil)
        }
    }

    @Test func aStoredValueThatNoLongerParsesReadsAsAbsent() throws {
        try withScratchDefaults { defaults in
            defaults.set(
                [
                    "/bad": ["name": "Long <a@b.c>", "email": "a@b.c"],
                    "/half": ["name": "Only Name"],
                ],
                forKey: CommitIdentityStore.key)
            let store = CommitIdentityStore(defaults: defaults)
            #expect(store.identity(for: "/bad") == nil)
            #expect(store.identity(for: "/half") == nil)

            defaults.set("not a dictionary", forKey: CommitIdentityStore.key)
            #expect(store.identity(for: "/bad") == nil)
            store.save(work, for: "/fresh")
            #expect(store.identity(for: "/fresh") == work)
        }
    }
}

@Suite struct CommitAuthorTests {
    let configured = CommitIdentity(name: "Repo", email: "repo@example.com")
    let saved = CommitIdentity(name: "Saved", email: "saved@example.com")

    @Test func theRepositoryConfigBeatsTheSavedIdentity() {
        #expect(
            CommitAuthor.resolve(configured: configured, saved: saved) == .configured(configured))
        #expect(CommitAuthor.resolve(configured: nil, saved: saved) == .saved(saved))
        #expect(CommitAuthor.resolve(configured: nil, saved: nil) == .unset)
    }

    @Test func theLabelNamesTheAuthorAndWhereItCameFrom() {
        #expect(
            CommitAuthor.configured(configured).label
                == "Author: Repo <repo@example.com> (repository config)")
        #expect(
            CommitAuthor.saved(saved).label == "Author: Saved <saved@example.com> (saved in rc-git)"
        )
    }
}

/// Answers `configuredIdentity` reads in order. The first read holds at `gate` until the test
/// opens it, so a test decides exactly when the stale answer lands.
private final class GatedConfigReader: @unchecked Sendable {
    let gate = CallGate()
    private let lock = NSLock()
    private var answers: [CommitIdentity?]
    private var calls = 0

    init(_ answers: [CommitIdentity?]) {
        self.answers = answers
    }

    var callCount: Int { lock.withLock { calls } }

    func read(_ path: String) -> CommitIdentity? {
        let (call, answer) = lock.withLock {
            calls += 1
            return (calls, answers.removeFirst())
        }
        if call == 1 { gate.pass() }
        return answer
    }
}

@MainActor @Suite struct CommitAuthorModelTests {
    let old = CommitIdentity(name: "Old", email: "old@example.com")
    let new = CommitIdentity(name: "New", email: "new@example.com")

    @Test func aStaleConfigReadIsDropped() async throws {
        try await withScratchDefaults { defaults in
            let reader = GatedConfigReader([old, new])
            let model = CommitAuthorModel(
                repoPath: "/repo", store: CommitIdentityStore(defaults: defaults),
                readConfigured: { reader.read($0) })
            var seen: [CommitAuthor?] = []
            model.onChange = { seen.append(model.author) }
            let slow = Task { await model.refresh() }
            await reader.gate.waitUntilArrived()
            await model.refresh()
            reader.gate.open()
            await slow.value
            #expect(reader.callCount == 2)
            #expect(model.author == .configured(new))
            #expect(seen == [.configured(new)], "\(seen)")
        }
    }

    @Test func savingAndClearingReDecideTheAuthor() async throws {
        try await withScratchDefaults { defaults in
            let model = CommitAuthorModel(
                repoPath: "/repo", store: CommitIdentityStore(defaults: defaults),
                readConfigured: { _ in nil })
            await model.refresh()
            #expect(model.author == .unset)
            await model.save(old)
            #expect(model.author == .saved(old))
            #expect(model.saved == old)
            await model.clear()
            #expect(model.author == .unset)
            #expect(model.saved == nil)
        }
    }

    @Test func anUnreadableConfigSaysSo() async throws {
        try await withScratchDefaults { defaults in
            let model = CommitAuthorModel(
                repoPath: "/repo", store: CommitIdentityStore(defaults: defaults),
                readConfigured: { _ in throw RepositoryError("cannot open /repo") })
            await model.refresh()
            #expect(model.author == .unreadable("cannot open /repo"))
        }
    }
}
