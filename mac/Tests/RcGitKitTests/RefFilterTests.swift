import Testing

@testable import RcGitKit

private let target = Oid(hex: "1a2b3c4d5e6f708192a3b4c5d6e7f80910a1b2c3")!

private func ref(_ shortName: String, _ kind: RefKind = .localBranch) -> RefEntry {
    let namespace: String
    switch kind {
    case .localBranch: namespace = "refs/heads/"
    case .remoteBranch: namespace = "refs/remotes/"
    case .tag: namespace = "refs/tags/"
    case .other: namespace = "refs/"
    }
    return RefEntry(
        fullName: namespace + shortName, shortName: shortName, kind: kind, target: target)
}

private func names(_ sections: [RefSection]) -> [[String]] {
    sections.map { $0.refs.map(\.shortName) }
}

@Suite struct RefFilterTests {
    let mixed = RefListing(refs: [
        ref("other-thing", .other),
        ref("v1.0", .tag),
        ref("origin/main", .remoteBranch),
        ref("main"),
        ref("feature"),
    ])

    @Test func sectionsComeInFixedOrder() {
        let sections = mixed.sections(matching: "")
        #expect(sections.map(\.title) == ["Branches", "Remotes", "Tags", "Other"])
        #expect(sections.map(\.kind) == [.localBranch, .remoteBranch, .tag, .other])
        #expect(
            names(sections) == [["feature", "main"], ["origin/main"], ["v1.0"], ["other-thing"]])
    }

    @Test func emptySectionsAreDropped() {
        let sections = mixed.sections(matching: "main")
        #expect(sections.map(\.title) == ["Branches", "Remotes"])
        #expect(names(sections) == [["main"], ["origin/main"]])
        #expect(RefListing(refs: []).sections(matching: "").isEmpty)
    }

    @Test func matchingIgnoresCase() {
        let listing = RefListing(refs: [ref("Main"), ref("feature/MAIN-fix"), ref("dev")])
        #expect(names(listing.sections(matching: "MAIN")) == [["feature/MAIN-fix", "Main"]])
        #expect(names(listing.sections(matching: "main")) == [["feature/MAIN-fix", "Main"]])
        #expect(listing.bestMatch(for: "MAIN")?.shortName == "Main")
    }

    @Test func refsSortInNaturalNumberOrder() {
        let listing = RefListing(refs: [ref("v10", .tag), ref("v2", .tag), ref("v1", .tag)])
        #expect(names(listing.sections(matching: "")) == [["v1", "v2", "v10"]])
    }

    @Test func aWhitespaceFilterFiltersNothing() {
        #expect(names(mixed.sections(matching: "  \t ")) == names(mixed.sections(matching: "")))
        #expect(RefListing.query(from: " \n") == nil)
        #expect(RefListing.query(from: " Main ") == "main")
        #expect(mixed.bestMatch(for: "   ") == nil)
    }

    @Test func noMatchGivesNil() {
        #expect(mixed.bestMatch(for: "zzz") == nil)
        #expect(mixed.sections(matching: "zzz").isEmpty)
    }

    @Test func anExactNameBeatsEarlierSubstringMatches() {
        let listing = RefListing(refs: [
            ref("chore/maintenance"), ref("domain-model"), ref("feature/main-fix"), ref("main"),
        ])
        #expect(listing.bestMatch(for: "main")?.shortName == "main")
        // The displayed order stays natural; only the pick moves.
        #expect(
            names(listing.sections(matching: "main"))
                == [["chore/maintenance", "domain-model", "feature/main-fix", "main"]])
    }

    @Test func aPrefixBeatsALaterComponentAndASubstring() {
        let listing = RefListing(refs: [
            ref("chore/maintenance"), ref("domain-model"), ref("feature/main-fix"), ref("main"),
        ])
        #expect(listing.bestMatch(for: "ma")?.shortName == "main")
    }

    @Test func aLastComponentMatchBeatsASubstring() {
        let listing = RefListing(refs: [ref("domain-model"), ref("origin/main", .remoteBranch)])
        #expect(listing.bestMatch(for: "main")?.shortName == "origin/main")
    }

    @Test func theLocalBranchWinsATieWithItsRemote() {
        #expect(mixed.bestMatch(for: "main")?.fullName == "refs/heads/main")
        let prefixTie = RefListing(refs: [
            ref("origin/main", .remoteBranch), ref("main-a", .tag), ref("main-b"),
        ])
        #expect(prefixTie.bestMatch(for: "main")?.fullName == "refs/heads/main-b")
    }

    @Test func aTieInOneSectionGoesToNaturalOrder() {
        let listing = RefListing(refs: [ref("main-10"), ref("main-2")])
        #expect(listing.bestMatch(for: "main")?.shortName == "main-2")
    }
}
