//! Ported from `CanonicalPathTests.swift`: every spelling of one repository gives one tab
//! key, the spelling `FSEvents` reports.
#![expect(
    clippy::unwrap_used,
    reason = "path helpers fail the test on a fixture path with no root"
)]

use std::path::{Path, PathBuf};

use rc_git_core::Repository;
use rc_git_kit::canonical_path::{resolve, tab_key, tab_path};
use rc_git_testkit::Fixture;

/// `path` under the other name of its `/private` root: `/var/x` for `/private/var/x` and the
/// reverse. `/tmp` and `/var` are both symlinks into `/private` on macOS.
fn other_private_spelling(path: &Path) -> PathBuf {
    match path.strip_prefix("/private") {
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => Path::new("/private").join(path.strip_prefix("/").unwrap()),
    }
}

fn spelled(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

#[test]
fn every_spelling_of_a_repository_gives_one_key() {
    let fixture = Fixture::standard();
    let aliases = Fixture::empty_dir();
    let parent = fixture.path().parent().unwrap();
    let name = fixture.path().file_name().unwrap();
    aliases.symlink(&parent.to_string_lossy(), "parent");

    let expected = std::fs::canonicalize(fixture.path().join(".git")).unwrap();
    assert!(expected.starts_with("/private/"), "{}", expected.display());
    let spellings = [
        fixture.path().to_path_buf(),
        spelled(fixture.path(), "/.git"),
        spelled(fixture.path(), "/"),
        spelled(fixture.path(), "/.git/"),
        spelled(fixture.path(), "/src/.."),
        aliases.path().join("parent").join(name),
        other_private_spelling(fixture.path()),
        other_private_spelling(fixture.path()).join(".git"),
    ];
    for spelling in spellings {
        let repository = Repository::open(&spelling).unwrap();
        assert_eq!(tab_key(&repository), expected, "{}", spelling.display());
    }
}

#[test]
fn two_repositories_get_two_keys() {
    let first = Fixture::standard();
    let second = Fixture::standard();
    assert_ne!(
        tab_key(&Repository::open(first.path()).unwrap()),
        tab_key(&Repository::open(second.path()).unwrap())
    );
}

#[test]
fn a_linked_worktree_has_its_own_key() {
    let fixture = Fixture::standard();
    let outside = Fixture::empty_dir();
    let linked = outside.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "wt",
        &linked.to_string_lossy(),
    ]);
    assert_ne!(
        tab_key(&Repository::open(fixture.path()).unwrap()),
        tab_key(&Repository::open(&linked).unwrap())
    );
}

#[test]
fn a_repository_opened_through_its_git_dir_shows_its_working_directory() {
    let fixture = Fixture::standard();
    let work_dir = resolve(fixture.path());
    assert!(work_dir.starts_with("/private/"), "{}", work_dir.display());
    for spelling in [
        fixture.path().to_path_buf(),
        spelled(fixture.path(), "/.git"),
        spelled(fixture.path(), "/.git/"),
    ] {
        let repository = Repository::open(&spelling).unwrap();
        assert_eq!(
            tab_path(&repository, &spelling),
            work_dir,
            "{}",
            spelling.display()
        );
    }
}

#[test]
fn a_bare_repository_shows_the_path_it_was_opened_through() {
    let fixture = Fixture::standard();
    let (_holder, bare_path) = fixture.bare_copy();
    let repository = Repository::open(&bare_path).unwrap();
    assert_eq!(tab_path(&repository, &bare_path), resolve(&bare_path));
}

#[test]
fn resolve_keeps_private_and_makes_a_relative_path_absolute() {
    assert_eq!(resolve(Path::new("/var")), Path::new("/private/var"));
    assert_eq!(
        resolve(Path::new("/tmp/../var/")),
        Path::new("/private/var")
    );
    let missing = Path::new("/private/var/rc-git-no-such-dir-4f1c9a");
    assert_eq!(resolve(missing), missing);
    assert_eq!(
        resolve(Path::new("/private/var/rc-git-no-such-dir-4f1c9a/./x/..")),
        missing,
        "a missing path still loses . and .."
    );
    assert!(resolve(Path::new(".")).is_absolute());
    assert_eq!(resolve(Path::new(".")), std::fs::canonicalize(".").unwrap());
}
