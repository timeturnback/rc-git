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
        return resolveThroughConfig(specifier, from: directory)
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
    }

    /// Resolves through the configs above `directory`, nearest first, and stops at the first
    /// that places the specifier. TypeScript asks only the nearest; asking the rest costs a
    /// read per config on a specifier nothing places, and finds the alias a root config
    /// declares for an editor. A config that only points at others, as a Vite project's does,
    /// has each one it references tried in order.
    private func resolveThroughConfig(_ specifier: String, from directory: [String]) -> String? {
        var current = directory
        while true {
            if let entries = list(current.joined(separator: "/")) {
                for name in Self.configNames where Self.isFile(name, in: entries) {
                    if let found = resolve(specifier, withConfigAt: current + [name]) {
                        return found
                    }
                }
            }
            guard !current.isEmpty else { return nil }
            current.removeLast()
        }
    }

    private func resolve(_ specifier: String, withConfigAt path: [String]) -> String? {
        guard let config = modulePaths(at: path, depth: 0) else { return nil }
        if let found = resolve(specifier, with: config) { return found }
        for reference in config.references {
            guard let referenced = modulePaths(at: configFile(at: reference), depth: 0),
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
    /// shipped in a package is not in the tree.
    private func modulePaths(at path: [String], depth: Int) -> ModulePaths? {
        guard let bytes = read(path.joined(separator: "/")),
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
        for reference in root["references"] as? [[String: Any]] ?? [] {
            guard let path = reference["path"] as? String,
                let target = RepoPath.flatten(directory, path)
            else { continue }
            result.references.append(target)
        }
        guard depth < Self.extendsDepth, result.baseURL == nil || result.paths == nil else {
            return result
        }
        let extended = (root["extends"] as? [String]) ?? (root["extends"] as? String).map { [$0] }
        for parent in (extended ?? []).reversed() where parent.hasPrefix(".") {
            guard let target = RepoPath.flatten(directory, parent),
                let inherited = modulePaths(at: configFile(at: target), depth: depth + 1)
            else { continue }
            result.baseURL = result.baseURL ?? inherited.baseURL
            result.paths = result.paths ?? inherited.paths
        }
        return result
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
