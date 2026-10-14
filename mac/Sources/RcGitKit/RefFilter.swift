import Foundation

/// One group of the ref list: a title and its refs in natural order.
public struct RefSection: Sendable, Hashable {
    public let title: String
    public let kind: RefKind
    public let refs: [RefEntry]

    public init(title: String, kind: RefKind, refs: [RefEntry]) {
        self.title = title
        self.kind = kind
        self.refs = refs
    }
}

extension RefListing {
    /// The order the ref list shows its sections in, which is also the tie-break order of
    /// `bestMatch(for:)`.
    static let sectionOrder: [(title: String, kind: RefKind)] = [
        ("Branches", .localBranch),
        ("Remotes", .remoteBranch),
        ("Tags", .tag),
        ("Other", .other),
    ]

    /// The title of the ref list's last section: the references whose target could not be
    /// read. Its rows are listed so a broken ref does not look like a missing one, and are
    /// never picked.
    public static let unreadableTitle = "Unreadable"

    /// The broken refs whose full name contains `filter`, ignoring case, sorted naturally. A
    /// blank filter keeps every one; an empty result means the section is left out.
    public func unreadable(matching filter: String) -> [BrokenRef] {
        guard let query = Self.query(from: filter) else { return sortedBroken }
        return sortedBroken.filter { $0.fullName.lowercased().contains(query) }
    }

    /// `refs` grouped into the ref list's sections, each sorted naturally (`v2` before `v10`);
    /// a kind with no refs has no section. The one place the ref list sorts.
    static func naturalSections(of refs: [RefEntry]) -> [RefSection] {
        let byKind = Dictionary(grouping: refs, by: \.kind)
        return sectionOrder.compactMap { title, kind in
            guard let members = byKind[kind] else { return nil }
            let sorted = members.sorted {
                $0.shortName.localizedStandardCompare($1.shortName) == .orderedAscending
            }
            return RefSection(title: title, kind: kind, refs: sorted)
        }
    }

    /// `broken` sorted naturally by full name.
    static func naturalOrder(of broken: [BrokenRef]) -> [BrokenRef] {
        broken.sorted { $0.fullName.localizedStandardCompare($1.fullName) == .orderedAscending }
    }

    /// The filter as matching uses it: trimmed and lowercased, or `nil` when it is blank and
    /// so filters nothing.
    public static func query(from filter: String) -> String? {
        let query = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return query.isEmpty ? nil : query
    }

    /// Whether the Working Tree row stays listed under `filter`: always for a blank filter,
    /// otherwise only when "working tree" contains it, ignoring case, like every ref row.
    public static func workingTreeMatches(_ filter: String) -> Bool {
        query(from: filter).map { "working tree".contains($0) } ?? true
    }

    /// The refs whose short name contains `filter`, ignoring case, grouped in section order
    /// and sorted naturally (`v2` before `v10`) within each. Empty sections are dropped; a
    /// blank filter keeps every ref. Filters the listing's sorted sections and never sorts.
    public func sections(matching filter: String) -> [RefSection] {
        guard let query = Self.query(from: filter) else { return sortedSections }
        return sortedSections.compactMap { section in
            let matches = section.refs.filter { Self.rank($0, query: query) != nil }
            return matches.isEmpty
                ? nil : RefSection(title: section.title, kind: section.kind, refs: matches)
        }
    }

    /// The ref Return should pick for `filter`: an exact short name first, then a short-name
    /// prefix, then a last path component that starts with the filter (`main` finds
    /// `origin/main`), then any other substring. Ties go to the earlier section, then to the
    /// natural order. `nil` when the filter is blank or nothing matches.
    public func bestMatch(for filter: String) -> RefEntry? {
        guard let query = Self.query(from: filter) else { return nil }
        var best: (ref: RefEntry, rank: Int)?
        for ref in sections(matching: filter).flatMap(\.refs) {
            guard let rank = Self.rank(ref, query: query) else { continue }
            if best.map({ rank < $0.rank }) ?? true {
                best = (ref, rank)
            }
        }
        return best?.ref
    }

    /// How well `ref` matches a non-empty lowercased query, lower is better; `nil` when it
    /// does not match at all.
    private static func rank(_ ref: RefEntry, query: String) -> Int? {
        let name = ref.shortName.lowercased()
        let lastComponent = name.split(separator: "/").last.map(String.init) ?? name
        if name == query { return 0 }
        if name.hasPrefix(query) { return 1 }
        if lastComponent.hasPrefix(query) { return 2 }
        if name.contains(query) { return 3 }
        return nil
    }
}
