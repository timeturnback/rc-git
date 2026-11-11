//! Ported from `HiddenFilesTests.swift` (the preference test moves with the settings store).

use rc_git_kit::hidden_files::{is_hidden_name, is_hidden_path};

#[test]
fn hides_version_control_and_operating_system_clutter() {
    for name in [
        ".git",
        ".svn",
        ".hg",
        "CVS",
        ".DS_Store",
        "Thumbs.db",
        ".Spotlight-V100",
        ".Trashes",
        ".fseventsd",
        "._README.md",
        "._",
    ] {
        assert!(is_hidden_name(name.as_bytes()), "{name} should be hidden");
    }
}

#[test]
fn keeps_dotfiles_worth_seeing() {
    for name in [
        ".env",
        ".gitignore",
        ".gitkeep",
        ".github",
        ".claude",
        ".vscode",
        "node_modules",
        "cvs",
        ".ds_store",
        "_README.md",
        "a._b",
        "",
    ] {
        assert!(
            !is_hidden_name(name.as_bytes()),
            "{name} should stay visible"
        );
    }
}

#[test]
fn a_path_is_hidden_when_any_component_is() {
    for hidden in [
        ".DS_Store",
        "src/.DS_Store",
        "vendor/lib/.git/HEAD",
        "assets/._logo.png",
        "a//.git/",
        "/.DS_Store",
        "src/é/._x",
        "x/CVS/y",
    ] {
        assert!(is_hidden_path(hidden.as_bytes()), "{hidden}");
    }
    for visible in [
        ".claude/worktrees/feature/.env",
        "docs/.github-notes.md",
        "",
        "/",
        "src/é/x",
    ] {
        assert!(!is_hidden_path(visible.as_bytes()), "{visible}");
    }
}

#[test]
fn a_non_utf8_component_is_matched_by_its_bytes() {
    assert!(is_hidden_path(b"f\xffo/._x"));
    assert!(!is_hidden_path(b"f\xffo/x"));
}
