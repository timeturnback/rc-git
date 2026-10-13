import Foundation

/// One spelling per directory on disk, the one FSEvents reports: absolute, symlinks resolved,
/// `/private` kept. `standardizingPath` and `resolvingSymlinksInPath` both strip `/private`,
/// so neither is used.
public enum CanonicalPath {
    /// The realpath of `path`. A relative path resolves against the working directory first,
    /// so `.` never comes back even when `path` does not exist; then the absolute path is
    /// returned as is.
    public static func resolve(_ path: String) -> String {
        let absolute = URL(fileURLWithPath: path).standardizedFileURL.path
        guard let resolved = realpath(absolute, nil) else { return absolute }
        defer { free(resolved) }
        return String(cString: resolved)
    }
}

extension Repository {
    /// What tells two open repositories apart: the realpath of this worktree's git directory.
    /// A working directory and its `.git`, any symlinked or `..` spelling of either, and
    /// `/var` against `/private/var` all give the same key; each linked worktree has its own.
    public var tabKey: String {
        CanonicalPath.resolve(gitDirectory)
    }
}
