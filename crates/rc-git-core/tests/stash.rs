//! The stash list, read from the `refs/stash` reflog written by the real git CLI.

use rc_git_core::Repository;
use rc_git_testkit::Fixture;

#[test]
fn a_repository_without_a_stash_lists_nothing() {
    let fx = Fixture::standard();
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(repo.stash_list().unwrap(), []);
}

#[test]
fn stashes_list_newest_first_with_reflog_messages() {
    let fx = Fixture::standard();
    fx.write("alpha.txt", b"stash me\n");
    fx.git(&["stash", "push", "-q", "-m", "first stash"]);
    fx.write("alpha.txt", b"stash me too\n");
    fx.git(&["stash", "push", "-q", "-m", "second stash"]);

    let repo = Repository::open(fx.path()).unwrap();
    let stashes = repo.stash_list().unwrap();
    assert_eq!(stashes.len(), 2);
    assert_eq!(stashes[0].index, 0);
    assert!(
        stashes[0].message.ends_with("second stash"),
        "{}",
        stashes[0].message
    );
    assert_eq!(stashes[0].oid.to_hex(), fx.rev_parse("stash@{0}"));
    assert_eq!(stashes[1].index, 1);
    assert!(
        stashes[1].message.ends_with("first stash"),
        "{}",
        stashes[1].message
    );
    assert_eq!(stashes[1].oid.to_hex(), fx.rev_parse("stash@{1}"));
}

fn two_stashes() -> Fixture {
    let fx = Fixture::standard();
    fx.write("alpha.txt", b"kept\n");
    fx.git(&["stash", "push", "-q", "-m", "kept"]);
    fx.write("alpha.txt", b"second\n");
    fx.git(&["stash", "push", "-q", "-m", "second"]);
    fx
}

#[test]
fn an_unterminated_last_reflog_line_is_not_listed() {
    let fx = two_stashes();
    let mut log = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
    assert_eq!(log.pop(), Some(b'\n'));
    fx.write(".git/logs/refs/stash", &log);

    let repo = Repository::open(fx.path()).unwrap();
    let stashes = repo.stash_list().unwrap();
    assert_eq!(stashes.len(), 1, "{stashes:?}");
    assert_eq!(stashes[0].index, 0);
    assert!(
        stashes[0].message.ends_with("kept"),
        "{}",
        stashes[0].message
    );
    // `git rev-parse stash@{0}` answers the ref itself here; `git stash list` reads the log.
    assert_eq!(
        stashes[0].oid.to_hex(),
        fx.git_output(&["stash", "list", "--format=%H"])
    );
}

#[test]
fn a_malformed_reflog_line_does_not_hide_the_other_stashes() {
    let fx = two_stashes();
    let log = std::fs::read(fx.path().join(".git/logs/refs/stash")).unwrap();
    let split = log.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    let mut junk_appended = log.clone();
    junk_appended.extend_from_slice(b"junk\n");
    let mut blank_inside = log[..split].to_vec();
    blank_inside.push(b'\n');
    blank_inside.extend_from_slice(&log[split..]);

    for corrupt in [junk_appended, blank_inside] {
        fx.write(".git/logs/refs/stash", &corrupt);
        let repo = Repository::open(fx.path()).unwrap();
        let stashes = repo.stash_list().unwrap();
        assert_eq!(stashes.len(), 2, "{stashes:?}");
        assert_eq!(stashes[0].index, 0);
        assert_eq!(stashes[0].oid.to_hex(), fx.rev_parse("stash@{0}"));
        assert_eq!(stashes[1].index, 1);
        assert_eq!(stashes[1].oid.to_hex(), fx.rev_parse("stash@{1}"));
    }
}

#[test]
fn a_stash_ref_without_a_reflog_lists_nothing() {
    let fx = Fixture::standard();
    fx.git(&["update-ref", "refs/stash", "HEAD"]);
    let repo = Repository::open(fx.path()).unwrap();
    assert_eq!(repo.stash_list().unwrap(), []);
}

#[test]
fn a_corrupt_stash_ref_is_reported() {
    let fx = Fixture::standard();
    fx.write(".git/refs/stash", b"garbage\n");
    let repo = Repository::open(fx.path()).unwrap();
    let err = repo.stash_list().unwrap_err();
    assert!(err.to_string().contains("read stash reference"), "{err}");
}
