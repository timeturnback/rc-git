import Foundation

/// Which bundle is running. The two carry different bundle identifiers, so their tabs,
/// recents, window frames, and trusted host keys never mix.
public enum AppVariant: String, Sendable, Hashable {
    /// `RCGit.app`, installed in `/Applications` by `scripts/install-app.sh`.
    case release
    /// `RCGit-dev.app`, built into the checkout by `scripts/build-app.sh --dev`.
    case dev
}

/// What `scripts/build-app.sh` stamped into the bundle: which variant it is, the checkout
/// it was built from, and the commit it was built at. An unstamped bundle — a bare
/// `swift run`, or a bundle assembled by hand — reports `.dev` with no source root, which
/// is what keeps the self-update path off.
public struct AppBuild: Sendable, Hashable {
    /// Where `scripts/install-app.sh` puts the release bundle, and what a self-update
    /// replaces and relaunches.
    public static let installedPath = "/Applications/RCGit.app"
    /// What `scripts/build-app.sh --dev` names the dev bundle inside its own checkout.
    public static let devBundleName = "RCGit-dev.app"

    public let variant: AppVariant
    /// `CFBundleName`: `rc-git`, or `rc-git (dev)` for the dev bundle.
    public let name: String
    /// `CFBundleShortVersionString`, e.g. `0.1.0`.
    public let version: String
    /// `CFBundleVersion`: the commit count at build time.
    public let build: String
    /// Absolute path of the checkout the bundle was built from, when it was stamped.
    public let sourceRoot: String?
    /// Full hex of the commit the bundle was built at, when it was stamped.
    public let commit: String?
    /// Whether tracked files were modified in the checkout at build time.
    public let dirty: Bool
    public let buildDate: Date?

    public init(
        variant: AppVariant,
        name: String,
        version: String,
        build: String,
        sourceRoot: String?,
        commit: String?,
        dirty: Bool,
        buildDate: Date?
    ) {
        self.variant = variant
        self.name = name
        self.version = version
        self.build = build
        self.sourceRoot = sourceRoot
        self.commit = commit
        self.dirty = dirty
        self.buildDate = buildDate
    }

    /// Reads the stamp out of an `Info.plist` dictionary. Missing keys degrade: an
    /// unstamped bundle is a dev build that cannot update itself.
    public init(info: [String: Any]) {
        let string = { (key: String) -> String? in
            guard let value = info[key] as? String, !value.isEmpty else { return nil }
            return value
        }
        variant = AppVariant(rawValue: string("RCGitVariant") ?? "") ?? .dev
        name = string("CFBundleName") ?? "rc-git"
        version = string("CFBundleShortVersionString") ?? "0"
        build = string("CFBundleVersion") ?? "0"
        sourceRoot = string("RCGitSourceRoot")
        commit = string("RCGitSourceCommit").flatMap { $0 == "unknown" ? nil : $0 }
        dirty = string("RCGitSourceDirty") == "true"
        buildDate = string("RCGitBuildDate").flatMap(AppBuild.parseDate)
    }

    /// The running bundle's stamp.
    public static let current = AppBuild(info: Bundle.main.infoDictionary ?? [:])

    /// First seven hex digits of the build commit, the length git itself abbreviates to.
    public var shortCommit: String? {
        commit.map { String($0.prefix(7)) }
    }

    /// What this bundle's update does, or `nil` when it carries no checkout to build from.
    /// The release bundle fast-forwards the checkout before building; the dev bundle
    /// rebuilds the working tree as it stands, which is the whole point of the edit-run
    /// loop.
    public var updateMode: UpdateMode? {
        guard sourceRoot != nil else { return nil }
        return variant == .release ? .update : .rebuild
    }

    /// The bundle an update launches when the build finishes: the installed release app,
    /// or the dev bundle in the checkout that built it.
    public var relaunchPath: String? {
        switch variant {
        case .release: Self.installedPath
        case .dev: sourceRoot.map { "\($0)/\(Self.devBundleName)" }
        }
    }

    /// `0.1.0 (42) · a3f91c2`, with a `+` on the commit when the checkout was dirty.
    public var versionLine: String {
        var line = "\(version) (\(build))"
        if let shortCommit {
            line += " · \(shortCommit)\(dirty ? "+" : "")"
        }
        return line
    }

    private static func parseDate(_ text: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: text)
    }
}
