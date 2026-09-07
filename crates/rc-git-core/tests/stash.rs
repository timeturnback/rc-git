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
