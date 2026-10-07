import Foundation
import Testing

@testable import RcGitKit

/// Module specifiers to repository paths, over an in-memory tree.
@Suite struct ImportResolverTests {
    /// A resolver over `files`, path to content. Directories are whatever the paths imply.
    private func resolver(_ files: [String: String]) -> ImportResolver {
        ImportResolver(
            list: { directory in
                let prefix = directory.isEmpty ? "" : directory + "/"
                var kinds: [String: EntryKind] = [:]
                for path in files.keys where path.hasPrefix(prefix) {
                    let rest = path.dropFirst(prefix.count).split(separator: "/")
                    guard let name = rest.first else { continue }
                    kinds[String(name)] = rest.count > 1 ? .tree : .blob
                }
                if kinds.isEmpty, !directory.isEmpty { return nil }
                return kinds.map { TreeEntry(name: $0.key, oid: oid(1), kind: $0.value) }
            },
            read: { path in files[path].map { Array($0.utf8) } })
    }

    private func sources(_ paths: String...) -> [String: String] {
        Dictionary(uniqueKeysWithValues: paths.map { ($0, "") })
    }

    @Test func relativeSpecifiersResolveAgainstTheImportingFile() {
        let tree = resolver(
            sources("src/app/page.tsx", "src/app/util.ts", "src/lib/format.tsx", "src/app/a.css"))
        #expect(tree.resolve("./util", from: "src/app/page.tsx") == "src/app/util.ts")
        #expect(tree.resolve("../lib/format", from: "src/app/page.tsx") == "src/lib/format.tsx")
        #expect(tree.resolve("./a.css", from: "src/app/page.tsx") == "src/app/a.css")
        #expect(tree.resolve("./util.ts", from: "src/app/page.tsx") == "src/app/util.ts")
    }

    @Test func aMissingFileAndAClimbOutOfTheRepositoryResolveToNothing() {
        let tree = resolver(sources("src/page.tsx"))
        #expect(tree.resolve("./nope", from: "src/page.tsx") == nil)
        #expect(tree.resolve("./nope/deeper", from: "src/page.tsx") == nil)
        #expect(tree.resolve("../../outside", from: "src/page.tsx") == nil)
        #expect(tree.resolve("", from: "src/page.tsx") == nil)
        #expect(tree.resolve("/etc/passwd", from: "src/page.tsx") == nil)
    }

    @Test func typescriptSourcesAreTriedBeforeJavascriptAndDeclarations() {
        let tree = resolver(sources("a.ts", "a.js", "b.d.ts", "b.js", "c.jsx", "main.ts"))
        #expect(tree.resolve("./a", from: "main.ts") == "a.ts")
        #expect(tree.resolve("./b", from: "main.ts") == "b.d.ts")
        #expect(tree.resolve("./c", from: "main.ts") == "c.jsx")
    }

    @Test func aDirectoryResolvesToItsIndexAndLosesToAFileOfTheSameName() {
        let tree = resolver(
            sources(
                "ui/index.tsx", "kit/index.js", "both.ts", "both/index.ts", "empty/x.css", "m.ts"))
        #expect(tree.resolve("./ui", from: "m.ts") == "ui/index.tsx")
        #expect(tree.resolve("./kit", from: "m.ts") == "kit/index.js")
        #expect(tree.resolve("./both", from: "m.ts") == "both.ts")
        #expect(tree.resolve("./empty", from: "m.ts") == nil, "a directory with no index")
        #expect(tree.resolve(".", from: "ui/Button.tsx") == "ui/index.tsx")
        #expect(tree.resolve("..", from: "both/deep/x.ts") == "both/index.ts")
        #expect(tree.resolve("./both/", from: "m.ts") == "both/index.ts")
    }

    @Test func aCompiledExtensionFindsItsSource() {
        let tree = resolver(sources("a.ts", "b.tsx", "c.mts", "d.js", "main.ts"))
        #expect(tree.resolve("./a.js", from: "main.ts") == "a.ts")
        #expect(tree.resolve("./b.js", from: "main.ts") == "b.tsx")
        #expect(tree.resolve("./b.jsx", from: "main.ts") == "b.tsx")
        #expect(tree.resolve("./c.mjs", from: "main.ts") == "c.mts")
        #expect(tree.resolve("./d.js", from: "main.ts") == "d.js", "the file as written wins")
    }

    @Test func aQuerySuffixIsNotPartOfThePath() {
        let tree = resolver(sources("src/icon.svg", "src/main.ts"))
        #expect(tree.resolve("./icon.svg?react", from: "src/main.ts") == "src/icon.svg")
    }

    @Test func pathAliasesComeFromTheNearestConfigWithCommentsAndTrailingCommas() {
        var files = sources("src/components/Button.tsx", "src/app/page.tsx", "src/env.ts")
        files["tsconfig.json"] = """
            {
              // what the bundler also reads
              "compilerOptions": {
                "strict": true, /* unrelated */
                "paths": {
                  "@/*": ["./src/*"],
                  "env": ["./src/env.ts"],
                },
              },
            }
            """
        let tree = resolver(files)
        #expect(
            tree.resolve("@/components/Button", from: "src/app/page.tsx")
                == "src/components/Button.tsx")
        #expect(tree.resolve("env", from: "src/app/page.tsx") == "src/env.ts")
        #expect(tree.resolve("@/missing", from: "src/app/page.tsx") == nil)
        #expect(tree.resolve("react", from: "src/app/page.tsx") == nil, "a registry package")
    }

    @Test func theLongestPatternWinsAndTargetsAreTriedInOrder() {
        var files = sources("src/x.ts", "generated/api/x.ts", "fallback/only.ts", "main.ts")
        files["tsconfig.json"] = """
            {"compilerOptions": {"paths": {
              "@/*": ["./src/*", "./fallback/*"],
              "@/api/*": ["./generated/api/*"]
            }}}
            """
        let tree = resolver(files)
        #expect(tree.resolve("@/x", from: "main.ts") == "src/x.ts")
        #expect(tree.resolve("@/api/x", from: "main.ts") == "generated/api/x.ts")
        #expect(tree.resolve("@/only", from: "main.ts") == "fallback/only.ts")
    }

    @Test func baseUrlPlacesBareSpecifiersAndPathTargets() {
        var files = sources("app/src/components/Button.tsx", "app/src/lib/x.ts", "app/src/main.ts")
        files["app/jsconfig.json"] = """
            {"compilerOptions": {"baseUrl": "./src", "paths": {"~lib/*": ["lib/*"]}}}
            """
        let tree = resolver(files)
        #expect(
            tree.resolve("components/Button", from: "app/src/main.ts")
                == "app/src/components/Button.tsx")
        #expect(tree.resolve("~lib/x", from: "app/src/main.ts") == "app/src/lib/x.ts")
    }

    @Test func anExtendedConfigSuppliesWhatTheNearestLeavesUnset() {
        var files = sources("packages/ui/src/Button.tsx", "packages/web/src/page.tsx")
        files["tsconfig.base.json"] = """
            {"compilerOptions": {"paths": {"@ui/*": ["./packages/ui/src/*"]}}}
            """
        files["packages/web/tsconfig.json"] = """
            {"extends": "../../tsconfig.base", "compilerOptions": {"strict": true}}
            """
        let tree = resolver(files)
        #expect(
            tree.resolve("@ui/Button", from: "packages/web/src/page.tsx")
                == "packages/ui/src/Button.tsx",
            "targets start from the config that wrote them")
    }

    @Test func anExtendsCycleEnds() {
        var files = sources("src/a.ts")
        files["tsconfig.json"] = #"{"extends": "./other.json"}"#
        files["other.json"] = #"{"extends": "./tsconfig.json"}"#
        #expect(resolver(files).resolve("@/a", from: "src/a.ts") == nil)
    }

    @Test func aSolutionConfigHandsOverToTheConfigsItReferences() {
        var files = sources("src/lib/x.ts", "src/main.tsx")
        files["tsconfig.json"] = """
            {"files": [], "references": [{"path": "./tsconfig.node.json"}, \
            {"path": "./tsconfig.app.json"}]}
            """
        files["tsconfig.node.json"] = #"{"compilerOptions": {"composite": true}}"#
        files["tsconfig.app.json"] = #"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#
        #expect(resolver(files).resolve("@/lib/x", from: "src/main.tsx") == "src/lib/x.ts")
    }

    @Test func aReferenceMayNameADirectoryAndTheLastExtendedConfigWins() {
        var files = sources("app/src/x.ts", "lib/x.ts", "lib/y.ts")
        files["tsconfig.json"] = #"{"references": [{"path": "./app"}]}"#
        files["app/tsconfig.json"] = #"{"extends": ["../base.json", "./local.json"]}"#
        files["base.json"] = #"{"compilerOptions": {"paths": {"@/*": ["./lib/*"]}}}"#
        files["app/local.json"] = #"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#
        #expect(resolver(files).resolve("@/x", from: "lib/y.ts") == "app/src/x.ts")
    }

    @Test func anInheritedBaseUrlPlacesTheTargetsOfPathsWrittenBelowIt() {
        var files = sources("lib/y.ts", "app/src/main.ts")
        files["base.json"] = #"{"compilerOptions": {"baseUrl": "./lib"}}"#
        files["app/tsconfig.json"] = """
            {"extends": "../base.json", "compilerOptions": {"paths": {"~/*": ["./*"]}}}
            """
        let tree = resolver(files)
        #expect(tree.resolve("~/y", from: "app/src/main.ts") == "lib/y.ts")
        #expect(tree.resolve("y", from: "app/src/main.ts") == "lib/y.ts")
    }

    @Test func aConfigFartherUpAnswersWhenTheNearestCannot() {
        var files = sources("shared/x.ts", "apps/web/src/page.tsx")
        files["apps/web/tsconfig.json"] = #"{"compilerOptions": {"strict": true}}"#
        files["tsconfig.json"] = #"{"compilerOptions": {"paths": {"shared/*": ["./shared/*"]}}}"#
        #expect(
            resolver(files).resolve("shared/x", from: "apps/web/src/page.tsx") == "shared/x.ts")
    }

    @Test func anUnreadableConfigResolvesNothing() {
        var files = sources("src/a.ts")
        files["tsconfig.json"] = "{ not json"
        #expect(resolver(files).resolve("@/a", from: "src/a.ts") == nil)
    }
}

/// Import links end to end through the model: the cache, and opening what a link names.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct ModelImportTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let commit = oid(0x11)
    let rootTree = oid(0x12)
    let srcTree = oid(0x13)
    let app = oid(0x21)
    let button = oid(0x22)
    let config = oid(0x23)

    init() {
        repo.listing = RefListing(refs: [
            branch("main", at: commit)
        ])
        repo.headValue = Head(refName: "refs/heads/main", target: commit)
        repo.treeOfMap = [commit: rootTree]
        repo.trees[rootTree] = [
            TreeEntry(name: "src", oid: srcTree, kind: .tree),
            TreeEntry(name: "tsconfig.json", oid: config, kind: .blob),
        ]
        repo.trees[srcTree] = [
            TreeEntry(name: "App.tsx", oid: app, kind: .blob),
            TreeEntry(name: "Button.tsx", oid: button, kind: .blob),
        ]
        repo.blobs[app] = textBlob("import Button from './Button';\n")
        repo.blobs[button] = textBlob("export default function Button() {}\n")
        repo.blobs[config] = textBlob(#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#)
    }

    @Test func aRelativeImportOpensTheFileItNames() async {
        await model.attach(repo)
        #expect(await model.openImport("./Button", from: "src/App.tsx"))
        #expect(model.openedFilePath == "src/Button.tsx")
        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.oid == button)
    }

    @Test func anAliasedImportOpensThroughTheConfig() async {
        await model.attach(repo)
        #expect(await model.openImport("@/Button", from: "src/App.tsx"))
        #expect(model.openedFilePath == "src/Button.tsx")
    }

    @Test func aPackageImportOpensNothingAndLeavesTheFileInPlace() async {
        await model.attach(repo)
        #expect(await model.openPath("src/App.tsx"))
        #expect(!(await model.openImport("react", from: "src/App.tsx")))
        #expect(!(await model.openImport("./missing", from: "src/App.tsx")))
        #expect(model.openedFilePath == "src/App.tsx")
    }

    @Test func resolutionReadsEachDirectoryOnce() async {
        await model.attach(repo)
        #expect(await model.openImport("@/Button", from: "src/App.tsx"))
        let calls = repo.lock.withLock { repo.treeEntryCalls }
        #expect(calls == 2, "the root and src, however many candidates were probed")
        #expect(await model.openImport("./Button", from: "src/App.tsx"))
        #expect(repo.lock.withLock { repo.treeEntryCalls } == calls, "served from the tree cache")
    }

    @Test func importsResolveOnDiskWhileBrowsingTheWorkingTree() async {
        repo.workDir = "/checkout"
        repo.worktreeListings[""] = WorktreeListing(entries: [
            WorktreeEntry(name: "src", kind: .tree, status: .unchanged, oid: nil, size: 0)
        ])
        repo.worktreeListings["src"] = WorktreeListing(entries: [
            WorktreeEntry(name: "App.tsx", kind: .blob, status: .unchanged, oid: app, size: 1),
            WorktreeEntry(name: "New.tsx", kind: .blob, status: .untracked, oid: nil, size: 1),
        ])
        repo.worktreeFiles["src/New.tsx"] = textBlob("export const fresh = 1;\n")
        await model.attach(repo)
        #expect(model.browsingWorkingTree)
        #expect(await model.openImport("./New", from: "src/App.tsx"))
        #expect(model.openedFilePath == "src/New.tsx", "an untracked file is on disk only")
        #expect(!(await model.openImport("./Button", from: "src/App.tsx")), "not on disk")
    }

    @Test func aResolutionTheUserMovedPastOpensNothing() async {
        await model.attach(repo)
        repo.delays[srcTree] = 1.0
        async let stale = model.openImport("./Button", from: "src/App.tsx")
        try? await Task.sleep(for: .milliseconds(100))
        await model.openFile(TreeEntry(name: "tsconfig.json", oid: config, kind: .blob))
        #expect(await stale, "a stale resolution is not a dead link")
        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.name == "tsconfig.json", "the newer open stays on screen")
    }

    @Test func linksAreCachedByBlobAlongsideTheHighlightSpans() async {
        await model.attach(repo)
        let entry = TreeEntry(name: "App.tsx", oid: app, kind: .blob)
        let document = TextDocument(
            bytes: Array("import Button from './Button';\n".utf8), lineStarts: [0],
            validUTF8: true)
        _ = await model.highlights(for: entry, document: document)
        let first = await model.importLinks(for: entry, document: document)
        #expect(first?.count == 2)
        // Same blob id, different text: a cache hit answers with the first parse.
        let other = TextDocument(
            bytes: Array("const x = 1;\n".utf8), lineStarts: [0], validUTF8: true)
        #expect(await model.importLinks(for: entry, document: other) == first)

        let lossy = TextDocument(bytes: [0x66, 0xFF, 0x6F], lineStarts: [0], validUTF8: false)
        let none = await model.importLinks(
            for: TreeEntry(name: "bad.ts", oid: oid(0x43), kind: .blob), document: lossy)
        #expect(none == nil, "byte offsets into lossy text would misalign")
    }
}
