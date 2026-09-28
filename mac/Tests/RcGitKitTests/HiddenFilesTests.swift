import Testing

@testable import RcGitKit

@Suite struct HiddenFilesTests {
    @Test func hidesVersionControlAndOperatingSystemClutter() {
        for name in [
            ".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db", ".Spotlight-V100", ".Trashes",
            ".fseventsd", "._README.md", "._",
        ] {
            #expect(HiddenFiles.isHidden(name: name), "\(name) should be hidden")
        }
    }

    @Test func keepsDotfilesWorthSeeing() {
        for name in [
            ".env", ".gitignore", ".gitkeep", ".github", ".claude", ".vscode", "node_modules",
            "cvs", ".ds_store", "_README.md", "a._b", "",
        ] {
            #expect(!HiddenFiles.isHidden(name: name), "\(name) should stay visible")
        }
    }

    @Test func aPathIsHiddenWhenAnyComponentIs() {
        #expect(HiddenFiles.isHidden(path: ".DS_Store"))
        #expect(HiddenFiles.isHidden(path: "src/.DS_Store"))
        #expect(HiddenFiles.isHidden(path: "vendor/lib/.git/HEAD"))
        #expect(HiddenFiles.isHidden(path: "assets/._logo.png"))
        #expect(!HiddenFiles.isHidden(path: ".claude/worktrees/feature/.env"))
        #expect(!HiddenFiles.isHidden(path: "docs/.github-notes.md"))
        #expect(!HiddenFiles.isHidden(path: ""))
    }
}
