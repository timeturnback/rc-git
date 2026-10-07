/// Where a relative reference written in a repository file lands in the repository.
enum RepoPath {
    /// Appends `relative` to the `base` components and flattens `.`, `..` and empty segments.
    /// `nil` when the path climbs out of the repository; an empty result is the repository root.
    static func flatten(_ base: [String], _ relative: String) -> [String]? {
        var components = base
        for component in relative.split(separator: "/") {
            switch component {
            case ".":
                continue
            case "..":
                guard !components.isEmpty else { return nil }
                components.removeLast()
            default:
                components.append(String(component))
            }
        }
        return components
    }
}
