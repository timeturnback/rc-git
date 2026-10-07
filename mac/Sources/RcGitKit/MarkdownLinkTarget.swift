import Foundation

/// What a markdown link points at once it is read against the file showing it. The preview
/// resolves every link through this, so clicking, copying, and the context menu all agree on
/// what a destination means.
public enum MarkdownLinkTarget: Sendable, Hashable {
    /// Something a browser handles: `https:`, `mailto:`, or any other scheme.
    case external(URL)
    /// Another file in the repository, with the heading fragment the link asked for.
    case repositoryPath(String, fragment: String?)
    /// A jump inside the file already on screen.
    case fragment(String)

    /// Resolves `destination` as written in a file at `sourcePath`. Returns `nil` for an empty
    /// destination, for a relative path that climbs above the repository root, and for a
    /// scheme the URL parser rejects.
    ///
    /// `sourcePath` is the repository-relative path of the file being shown; when it is `nil`
    /// a relative destination is read as if the file sat at the repository root.
    public static func resolve(_ destination: String, from sourcePath: String?)
        -> MarkdownLinkTarget?
    {
        let trimmed = destination.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if trimmed.hasPrefix("#") { return .fragment(String(trimmed.dropFirst())) }
        if trimmed.hasPrefix("//") { return URL(string: "https:\(trimmed)").map(Self.external) }
        if let scheme = Self.scheme(of: trimmed) {
            guard scheme != "file" else { return nil }
            return URL(string: trimmed).map(Self.external)
        }
        return repositoryTarget(trimmed, from: sourcePath)
    }

    private static func repositoryTarget(_ destination: String, from sourcePath: String?)
        -> MarkdownLinkTarget?
    {
        let withoutQuery = destination.split(separator: "?", maxSplits: 1)[0]
        let parts = withoutQuery.split(
            separator: "#", maxSplits: 1, omittingEmptySubsequences: false)
        let fragment = parts.count > 1 && !parts[1].isEmpty ? String(parts[1]) : nil
        let rawPath = String(parts[0]).removingPercentEncoding ?? String(parts[0])
        guard !rawPath.isEmpty else {
            return fragment.map(Self.fragment)
        }
        let base = rawPath.hasPrefix("/") ? [] : Self.directory(of: sourcePath)
        guard let path = Self.normalize(base: base, relative: rawPath) else { return nil }
        return .repositoryPath(path, fragment: fragment)
    }

    /// The scheme of an absolute URL, or `nil` when the destination is a path. A bare
    /// `C:` style prefix is not a scheme here; a scheme needs at least two letters.
    private static func scheme(of destination: String) -> String? {
        guard let colon = destination.firstIndex(of: ":") else { return nil }
        let candidate = destination[destination.startIndex..<colon]
        guard candidate.count >= 2,
            candidate.allSatisfy({ $0.isLetter || $0.isNumber || $0 == "+" || $0 == "-" })
        else { return nil }
        if let slash = destination.firstIndex(of: "/"), slash < colon { return nil }
        return candidate.lowercased()
    }

    private static func directory(of path: String?) -> [String] {
        guard let path else { return [] }
        return path.split(separator: "/").dropLast().map(String.init)
    }

    /// Joins and flattens `.` and `..`. Returns `nil` when the link climbs out of the
    /// repository or lands on its root, neither of which names a file in the tree.
    private static func normalize(base: [String], relative: String) -> String? {
        RepoPath.flatten(base, relative).flatMap { $0.isEmpty ? nil : $0.joined(separator: "/") }
    }
}
