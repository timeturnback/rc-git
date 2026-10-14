import Foundation

/// Turns a module specifier written in a JavaScript or TypeScript file into the repository
/// path of the file it names, the way a bundler would: a relative path against the importing
/// file, anything else through the `paths` and `baseUrl` of the nearest `tsconfig.json` or
/// `jsconfig.json`. A specifier that names a package from a registry resolves to `nil`, since
/// nothing in the tree answers for it.
///
/// Every answer comes from the two closures, so resolution reads only the directories on the
/// way to a candidate and, for a specifier that is not relative, the config files above the
/// importing file. Nothing is walked and nothing is kept between calls.
public struct ImportResolver {
    /// The children of a directory by repository path, `""` for the root. `nil` when the path
    /// is not a directory.
    private let list: (String) -> [TreeEntry]?
    /// The bytes of a config file by repository path. `nil` when it cannot be read as text.
    private let read: (String) -> [UInt8]?

    /// Extensions tried on a specifier that leaves its own out, in TypeScript's order.
    static let extensions = [
        ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs", ".json",
    ]
    /// The sources a compiled extension may stand for: TypeScript's ESM convention writes
    /// `./module.js` for a file that is `module.ts` in the tree.
    static let sourceExtensions = [
        ".js": [".ts", ".tsx"], ".jsx": [".tsx"], ".mjs": [".mts"], ".cjs": [".cts"],
    ]
    static let configNames = ["tsconfig.json", "jsconfig.json"]
    /// How many `extends` a config may chain through before the rest is ignored.
    static let extendsDepth = 4
    /// How many config files one `resolve` may read, across ancestors, references and
    /// `extends`, so a committed config that fans out cannot multiply the work of one click.
    static let configReadLimit = 16

    public init(list: @escaping (String) -> [TreeEntry]?, read: @escaping (String) -> [UInt8]?) {
        self.list = list
        self.read = read
    }

    /// Resolves `specifier` as written in the file at `sourcePath`. `nil` when it names
    /// nothing in the tree.
    public func resolve(_ specifier: String, from sourcePath: String) -> String? {
        let specifier = String(specifier.prefix { $0 != "?" })
        guard !specifier.isEmpty, !specifier.hasPrefix("/") else { return nil }
        let directory = sourcePath.split(separator: "/").dropLast().map(String.init)
        let last = specifier.split(separator: "/", omittingEmptySubsequences: false).last
        if specifier.hasPrefix("./") || specifier.hasPrefix("../") || last == "." || last == ".." {
            // `.`, `..` and a trailing slash name a directory, never a file beside it.
            let namesDirectory = last == "." || last == ".." || last == ""
            return RepoPath.flatten(directory, specifier).flatMap(
                namesDirectory ? indexFile : probe)
        }
        return resolveThroughConfig(specifier, from: directory, budget: ReadBudget())
    }

    // MARK: Files

    /// The file a path names once extensions are settled: the path itself, the path with a
    /// source extension, the source of a compiled extension, or the index file of the
    /// directory it names. An empty path is the repository root.
    private func probe(_ components: [String]) -> String? {
        guard let name = components.last else { return indexFile(in: []) }
        let parent = Array(components.dropLast())
        guard let siblings = list(parent.joined(separator: "/")) else { return nil }
        var candidates = [name]
        candidates += Self.extensions.map { name + $0 }
        for (compiled, sources) in Self.sourceExtensions where name.hasSuffix(compiled) {
            let stem = name.dropLast(compiled.count)
            candidates += sources.map { String(stem) + $0 }
        }
        for candidate in candidates where Self.isFile(candidate, in: siblings) {
            return (parent + [candidate]).joined(separator: "/")
        }
        guard siblings.contains(where: { $0.name == name && $0.kind.isDirectory }) else {
            return nil
        }
        return indexFile(in: components)
    }

    private func indexFile(in directory: [String]) -> String? {
        guard let entries = list(directory.joined(separator: "/")) else { return nil }
        for fileExtension in Self.extensions {
            let name = "index" + fileExtension
            if Self.isFile(name, in: entries) {
                return (directory + [name]).joined(separator: "/")
            }
        }
        return nil
    }

    private static func isFile(_ name: String, in entries: [TreeEntry]) -> Bool {
        entries.contains { entry in
            guard entry.name == name else { return false }
            switch entry.kind {
            case .blob, .executableBlob, .symlink: return true
            case .tree, .submodule: return false
            }
        }
    }

    // MARK: Config

    /// What a config says about module names, with every path already made repository-relative.
    private struct ModulePaths {
        /// `compilerOptions.baseUrl`, the directory bare specifiers and `paths` start from.
        var baseURL: [String]?
        /// `compilerOptions.paths` and the directory of the config that wrote them, which is
        /// what the targets start from when no `baseUrl` is set.
        var paths: (patterns: [String: [String]], directory: [String])?
        /// Where a solution-style config points: the configs it hands its files to.
        var references: [[String]] = []

        /// Takes `baseURL` and `paths` from an extended config wherever this one has none.
        mutating func fillUnset(from inherited: ModulePaths) {
            baseURL = baseURL ?? inherited.baseURL
            paths = paths ?? inherited.paths
        }
    }

    /// The config reads one resolution has left, shared by everything it reads.
    private final class ReadBudget {
        private var remaining = ImportResolver.configReadLimit

        var isSpent: Bool { remaining == 0 }

        /// Takes one read from the budget. `false` once it is spent.
        func spend() -> Bool {
            guard remaining > 0 else { return false }
            remaining -= 1
            return true
        }
    }

    /// Resolves through the configs above `directory`, nearest first, and stops at the first
    /// that places the specifier. TypeScript asks only the nearest; asking the rest costs a
    /// read per config on a specifier nothing places, and finds the alias a root config
    /// declares for an editor. A config that only points at others, as a Vite project's does,
    /// has each one it references tried in order.
    private func resolveThroughConfig(
        _ specifier: String, from directory: [String], budget: ReadBudget
    ) -> String? {
        var current = directory
        while true {
            if let entries = list(current.joined(separator: "/")) {
                for name in Self.configNames where Self.isFile(name, in: entries) {
                    if let found = resolve(
                        specifier, withConfigAt: current + [name], budget: budget)
                    {
                        return found
                    }
                }
            }
            guard !current.isEmpty else { return nil }
            current.removeLast()
        }
    }

    private func resolve(_ specifier: String, withConfigAt path: [String], budget: ReadBudget)
        -> String?
    {
        guard let config = modulePaths(at: path, visited: [], budget: budget) else { return nil }
        if let found = resolve(specifier, with: config) { return found }
        for reference in config.references {
            guard !budget.isSpent else { return nil }
            let file = configFile(at: reference)
            guard let referenced = modulePaths(at: file, visited: [], budget: budget),
                let found = resolve(specifier, with: referenced)
            else { continue }
            return found
        }
        return nil
    }

    private func resolve(_ specifier: String, with config: ModulePaths) -> String? {
        if let paths = config.paths,
            let (targets, wildcard) = Self.match(specifier, in: paths.patterns)
        {
            let base = config.baseURL ?? paths.directory
            for target in targets where !target.hasPrefix("/") {
                let path = target.replacingOccurrences(of: "*", with: wildcard)
                if let found = RepoPath.flatten(base, path).flatMap(probe) { return found }
            }
        }
        guard let baseURL = config.baseURL else { return nil }
        return RepoPath.flatten(baseURL, specifier).flatMap(probe)
    }

    /// The `paths` entry a specifier falls under and the text its `*` stands for. An exact
    /// pattern wins; among wildcard patterns the longest prefix does, as in TypeScript. A tie
    /// goes to the longer suffix, since JSONSerialization does not keep declaration order.
    private static func match(_ specifier: String, in patterns: [String: [String]])
        -> (targets: [String], wildcard: String)?
    {
        if let exact = patterns[specifier] { return (exact, "") }
        var best: (targets: [String], wildcard: String, prefix: Int, suffix: Int)?
        for (pattern, targets) in patterns {
            let parts = pattern.split(
                separator: "*", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2 else { continue }
            let (prefix, suffix) = (parts[0], parts[1])
            guard specifier.count >= prefix.count + suffix.count, specifier.hasPrefix(prefix),
                specifier.hasSuffix(suffix)
            else { continue }
            if let best,
                !(prefix.count > best.prefix
                    || (prefix.count == best.prefix && suffix.count > best.suffix))
            {
                continue
            }
            let wildcard = specifier.dropFirst(prefix.count).dropLast(suffix.count)
            best = (targets, String(wildcard), prefix.count, suffix.count)
        }
        return best.map { ($0.targets, $0.wildcard) }
    }

    /// Reads one config and, for whatever it leaves unset, the configs it extends. `nil` when
    /// the file cannot be read or parsed. Only a relative `extends` is followed: a config
    /// shipped in a package is not in the tree. `visited` holds the configs further down the
    /// `extends` chain, so a cycle ends; `nil` too once `budget` is spent.
    private func modulePaths(at path: [String], visited: Set<[String]>, budget: ReadBudget)
        -> ModulePaths?
    {
        guard budget.spend(), let bytes = read(path.joined(separator: "/")),
            let root = try? JSONSerialization.jsonObject(
                with: Data(bytes), options: [.json5Allowed]) as? [String: Any]
        else { return nil }
        let directory = Array(path.dropLast())
        let options = root["compilerOptions"] as? [String: Any] ?? [:]
        var result = ModulePaths()
        if let baseURL = options["baseUrl"] as? String {
            result.baseURL = RepoPath.flatten(directory, baseURL)
        }
        if let patterns = options["paths"] as? [String: [String]] {
            result.paths = (patterns, directory)
        }
        result.references = Self.references(in: root, directory: directory)
        inherit(into: &result, from: root, at: path, visited: visited, budget: budget)
        return result
    }

    /// The configs a solution-style config hands its files to, as repository paths.
    private static func references(in root: [String: Any], directory: [String]) -> [[String]] {
        let entries = root["references"] as? [[String: Any]] ?? []
        return entries.compactMap { reference in
            (reference["path"] as? String).flatMap { RepoPath.flatten(directory, $0) }
        }
    }

    /// Fills what `result` leaves unset from the configs the one at `path` extends, the last
    /// entry first. A config already on the chain, or named twice, is read once at most.
    private func inherit(
        into result: inout ModulePaths, from root: [String: Any], at path: [String],
        visited: Set<[String]>, budget: ReadBudget
    ) {
        let chain = visited.union([path])
        guard chain.count <= Self.extendsDepth, result.baseURL == nil || result.paths == nil
        else { return }
        let directory = Array(path.dropLast())
        var seen = chain
        for parent in Self.extended(by: root).reversed() where parent.hasPrefix(".") {
            guard !budget.isSpent else { return }
            guard let file = unseenConfig(parent, directory: directory, seen: &seen),
                let inherited = modulePaths(at: file, visited: chain, budget: budget)
            else { continue }
            result.fillUnset(from: inherited)
        }
    }

    /// The `extends` entries of a config: one string or a list of them.
    private static func extended(by root: [String: Any]) -> [String] {
        if let list = root["extends"] as? [String] { return list }
        return (root["extends"] as? String).map { [$0] } ?? []
    }

    /// The config file an `extends` entry names, or `nil` when it leaves the repository or
    /// names a config already in `seen`, which then holds it.
    private func unseenConfig(_ parent: String, directory: [String], seen: inout Set<[String]>)
        -> [String]?
    {
        guard let target = RepoPath.flatten(directory, parent), seen.insert(target).inserted
        else { return nil }
        let file = configFile(at: target)
        guard file == target || seen.insert(file).inserted else { return nil }
        return file
    }

    /// The config file a reference or an `extends` names: the path itself, the path with
    /// `.json` when that was left out, or the `tsconfig.json` of the directory it names.
    private func configFile(at target: [String]) -> [String] {
        guard let name = target.last else { return ["tsconfig.json"] }
        let parent = Array(target.dropLast())
        guard let siblings = list(parent.joined(separator: "/")) else { return target }
        if Self.isFile(name, in: siblings) { return target }
        if Self.isFile(name + ".json", in: siblings) { return parent + [name + ".json"] }
        return target + ["tsconfig.json"]
    }
}
