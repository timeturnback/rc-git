import AppKit
import RcGitKit

/// Row symbols. Every icon in the sidebar is an SF Symbol, so the source list tints them all
/// the same way — the accent color for folders and refs, neutral gray for files — rather than
/// mixing in Finder document icons, whose colors come from whichever app claims the extension.
@MainActor
enum Icons {
    private static var byName: [String: NSImage] = [:]

    static func image(for entry: TreeEntry) -> NSImage? {
        switch entry.kind {
        case .tree: symbol(named: "folder.fill")
        case .submodule: symbol(named: "folder.fill.badge.gearshape")
        case .symlink: symbol(named: "link")
        case .blob, .executableBlob: symbol(named: "doc")
        }
    }

    /// How a source list tints `entry`'s symbol: folders take the accent color like every
    /// other sidebar item, files stay gray so a long listing does not turn into a wall of
    /// color.
    static func tint(for entry: TreeEntry) -> NSTintConfiguration {
        switch entry.kind {
        case .tree, .submodule: .default
        case .symlink, .blob, .executableBlob: .monochrome
        }
    }

    static func symbol(for kind: RefKind) -> NSImage? {
        switch kind {
        case .localBranch: symbol(named: "arrow.triangle.branch")
        case .remoteBranch: symbol(named: "cloud")
        case .tag: symbol(named: "tag")
        case .other: symbol(named: "circle")
        }
    }

    /// The working tree, in the ref list and on the branch banner.
    static var workingTree: NSImage? { symbol(named: "internaldrive") }

    /// The branch banner's icon. The working tree matches its row in the ref list.
    static func symbol(for symbol: BranchBanner.Symbol) -> NSImage? {
        switch symbol {
        case let .ref(kind): self.symbol(for: kind)
        case .commit, .detached: self.symbol(named: "smallcircle.filled.circle")
        case .workingTree: workingTree
        case .nothing: nil
        }
    }

    private static func symbol(named name: String) -> NSImage? {
        if let cached = byName[name] { return cached }
        let image = NSImage(systemSymbolName: name, accessibilityDescription: nil)
        byName[name] = image
        return image
    }
}
