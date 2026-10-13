import Foundation
import Testing

@testable import RcGitKit

/// A provider whose answers are released by the test: each query waits until `release`
/// hands it a result, so a query is in flight for exactly as long as the test wants.
@MainActor
private final class GatedProvider {
    private var waiting: [String: CheckedContinuation<PathSearchResult, any Error>] = [:]
    private(set) var calls: [String] = []

    func search(_ query: String) async throws -> PathSearchResult {
        calls.append(query)
        return try await withCheckedThrowingContinuation { waiting[query] = $0 }
    }

    func release(_ query: String, _ paths: [String]) {
        let entries = paths.map {
            PathIndexEntry(path: $0, oid: Oid(bytes: []), kind: .blob, hidden: false)
        }
        waiting.removeValue(forKey: query)?.resume(
            returning: PathSearchResult(entries: entries, truncated: false))
    }

    func fail(_ query: String) {
        waiting.removeValue(forKey: query)?.resume(throwing: GatedFailure())
    }

    /// Waits until `query` has reached the provider, so `release` has a continuation to resume.
    func arrived(_ query: String) async {
        while waiting[query] == nil { await Task.yield() }
    }
}

private struct GatedFailure: Error {}

@MainActor
@Suite struct PathSearchSessionTests {
    private func session(_ provider: GatedProvider) -> (PathSearchSession, () -> [String]) {
        let session = PathSearchSession()
        session.provider = { try await provider.search($0) }
        var opened: [String] = []
        session.onOpen = { opened.append($0.path) }
        return (session, { opened })
    }

    @Test func returnDuringANewerQueryOpensItsFirstMatchNotTheOlderList() async {
        let provider = GatedProvider()
        let (session, opened) = session(provider)
        let empty = session.search("")
        await provider.arrived("")
        provider.release("", ["a/first.txt", "b/compile.rs"])
        await empty?.value

        let comp = session.search("comp")
        await provider.arrived("comp")
        session.open(row: 0)
        #expect(opened().isEmpty)

        provider.release("comp", ["b/compile.rs"])
        await comp?.value
        #expect(opened() == ["b/compile.rs"])
    }

    @Test func returnBeforeTheFirstListLandsOpensItsFirstMatch() async {
        let provider = GatedProvider()
        let (session, opened) = session(provider)
        session.reset()
        let empty = session.search("")
        await provider.arrived("")
        session.open(row: 0)
        #expect(opened().isEmpty)

        provider.release("", ["README.md"])
        await empty?.value
        #expect(opened() == ["README.md"])
    }

    @Test func anOlderQueryLandingLateIsDroppedAndOpensNothing() async {
        let provider = GatedProvider()
        let (session, opened) = session(provider)
        let old = session.search("c")
        await provider.arrived("c")
        let new = session.search("co")
        await provider.arrived("co")
        session.open(row: 0)

        provider.release("c", ["c/stale.txt"])
        await old?.value
        #expect(session.results.isEmpty)
        #expect(opened().isEmpty)

        provider.release("co", ["co/fresh.txt"])
        await new?.value
        #expect(session.results.map(\.path) == ["co/fresh.txt"])
        #expect(opened() == ["co/fresh.txt"])
    }

    @Test func returnOnALandedListOpensTheSelectedRow() async {
        let provider = GatedProvider()
        let (session, opened) = session(provider)
        let query = session.search("x")
        await provider.arrived("x")
        provider.release("x", ["x1", "x2"])
        await query?.value

        session.open(row: 1)
        session.open(row: 5)
        #expect(opened() == ["x2"])
    }

    @Test func aPendingOpenIsDroppedOnCancelOrFailure() async {
        let provider = GatedProvider()
        let (session, opened) = session(provider)
        let cancelled = session.search("a")
        await provider.arrived("a")
        session.open(row: 0)
        session.cancelPendingOpen()
        provider.release("a", ["a.txt"])
        await cancelled?.value

        let failing = session.search("b")
        await provider.arrived("b")
        session.open(row: 0)
        provider.fail("b")
        await failing?.value
        #expect(session.results.isEmpty)

        let next = session.search("c")
        await provider.arrived("c")
        provider.release("c", ["c.txt"])
        await next?.value
        #expect(opened().isEmpty)
        #expect(provider.calls == ["a", "b", "c"])
    }

    @Test func resetDropsAnInFlightResultAndThePendingOpen() async {
        let provider = GatedProvider()
        let (session, opened) = session(provider)
        let query = session.search("a")
        await provider.arrived("a")
        session.open(row: 0)
        session.reset()

        provider.release("a", ["a.txt"])
        await query?.value
        #expect(session.results.isEmpty)
        #expect(opened().isEmpty)
    }
}
