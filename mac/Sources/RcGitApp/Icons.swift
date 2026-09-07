import AppKit
import RcGitKit
import UniformTypeIdentifiers

/// File and folder icons, cached per extension because `NSWorkspace` lookups are not free
/// and a tree shows the same few extensions thousands of times.
@MainActor
enum Icons {
    private static var byExtension: [String: NSImage] = [:]
    private static let folder = NSWorkspace.shared.icon(for: .folder)
    private static let symlink = NSWorkspace.shared.icon(for: .symbolicLink)
    private static let generic = NSWorkspace.shared.icon(for: .data)

    static func image(for entry: TreeEntry) -> NSImage {
        switch entry.kind {
        case .tree, .submodule:
            return folder
        case .symlink:
            return symlink
        case .blob, .executableBlob:
            let ext = (entry.name as NSString).pathExtension.lowercased()
            if let cached = byExtension[ext] { return cached }
            let type = ext.isEmpty ? nil : UTType(filenameExtension: ext)
            let image = type.map { NSWorkspace.shared.icon(for: $0) } ?? generic
            byExtension[ext] = image
            return image
        }
    }

    static func symbol(for kind: RefKind) -> NSImage? {
        let name: String
        switch kind {
        case .localBranch: name = "arrow.triangle.branch"
        case .remoteBranch: name = "cloud"
        case .tag: name = "tag"
        case .other: name = "circle"
        }
        return NSImage(systemSymbolName: name, accessibilityDescription: nil)
    }
}
