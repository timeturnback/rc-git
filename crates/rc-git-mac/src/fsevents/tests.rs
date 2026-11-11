//! Real `FSEvents` on testkit fixtures, which live under `/var/folders` and are reported by
//! `FSEvents` as `/private/var/folders`.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use rc_git_core::Repository;
use rc_git_testkit::Fixture;

use super::*;

const BOUND: Duration = Duration::from_secs(5);

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn signal() -> (mpsc::Receiver<()>, OnChange) {
    let (sender, receiver) = mpsc::channel();
    let sender = Mutex::new(sender);
    let on_change: OnChange = Arc::new(move || {
        let _ = sender.lock().unwrap().send(());
    });
    (receiver, on_change)
}

fn ref_watcher(repository: &Repository, on_change: OnChange) -> FsEventsWatcher {
    FsEventsWatcher::refs(
        &text(repository.git_dir()),
        &text(repository.common_dir()),
        on_change,
    )
}

fn worktree_watcher(repository: &Repository, on_change: OnChange) -> FsEventsWatcher {
    let dirs = WorktreeDirs {
        work_dir: text(repository.work_dir().unwrap()),
        git_dir: text(repository.git_dir()),
        common_dir: text(repository.common_dir()),
    };
    FsEventsWatcher::worktree(&dirs, on_change)
}

/// Adds a linked worktree on a new branch, outside the fixture; returns its path.
fn linked_worktree(fixture: &Fixture, outside: &Fixture) -> String {
    let linked = text(&outside.path().join("linked"));
    fixture.git(&["worktree", "add", "-q", "-b", "wt", &linked]);
    linked
}

#[test]
fn a_ref_update_under_the_temporary_directory_fires() {
    let fixture = Fixture::standard();
    let repository = Repository::open(fixture.path()).unwrap();
    let (fired, on_change) = signal();
    let watcher = ref_watcher(&repository, on_change);
    watcher.start().unwrap();
    watcher.start().unwrap();
    fixture.git(&["update-ref", "refs/heads/x", "HEAD"]);
    assert!(fired.recv_timeout(BOUND).is_ok());
}

#[test]
fn a_commit_in_a_linked_worktree_fires() {
    let fixture = Fixture::standard();
    let outside = Fixture::empty_dir();
    let linked = linked_worktree(&fixture, &outside);
    let repository = Repository::open(&linked).unwrap();
    assert_ne!(repository.common_dir(), repository.git_dir());
    let (fired, on_change) = signal();
    let watcher = ref_watcher(&repository, on_change);
    watcher.start().unwrap();
    fixture.git(&["-C", &linked, "commit", "-q", "--allow-empty", "-m", "x"]);
    assert!(fired.recv_timeout(BOUND).is_ok());
}

#[test]
fn a_worktree_edit_under_the_temporary_directory_fires() {
    let fixture = Fixture::standard();
    let repository = Repository::open(fixture.path()).unwrap();
    let (fired, on_change) = signal();
    let watcher = worktree_watcher(&repository, on_change);
    watcher.start().unwrap();
    fixture.write("edited.txt", b"edited\n");
    assert!(fired.recv_timeout(BOUND).is_ok());
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test writes the fixture's info/exclude, the edit the watcher must see"
)]
fn a_common_dir_exclude_edit_fires_for_a_linked_worktree() {
    let fixture = Fixture::standard();
    let outside = Fixture::empty_dir();
    let linked = linked_worktree(&fixture, &outside);
    let repository = Repository::open(&linked).unwrap();
    let (fired, on_change) = signal();
    let watcher = worktree_watcher(&repository, on_change);
    assert_eq!(watcher.watched_paths().len(), 2);
    watcher.start().unwrap();
    let info = repository.common_dir().join("info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("exclude"), b"*.log\n").unwrap();
    assert!(fired.recv_timeout(BOUND).is_ok());
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test reaches the fixture through a symlink it makes and removes"
)]
fn a_repository_reached_through_a_symlink_fires() {
    let fixture = Fixture::standard();
    let links = tempfile::tempdir().unwrap();
    let link = links.path().join("link");
    std::os::unix::fs::symlink(fixture.path(), &link).unwrap();
    let git = text(&link.join(".git"));
    let (fired, on_change) = signal();
    let watcher = FsEventsWatcher::refs(&git, &git, on_change);
    watcher.start().unwrap();
    fixture.git(&["update-ref", "refs/heads/x", "HEAD"]);
    assert!(fired.recv_timeout(BOUND).is_ok());
}

#[test]
fn a_stopped_watcher_stays_quiet_and_object_churn_is_filtered() {
    let fixture = Fixture::standard();
    let repository = Repository::open(fixture.path()).unwrap();
    let (fired, on_change) = signal();
    let watcher = ref_watcher(&repository, on_change);
    watcher.start().unwrap();
    let _ = fixture.hash_blob(b"a blob no fixture holds\n");
    assert!(
        fired.recv_timeout(Duration::from_secs(1)).is_err(),
        "an object write is not a ref change"
    );
    watcher.stop();
    watcher.stop();
    fixture.git(&["update-ref", "refs/heads/y", "HEAD"]);
    assert!(fired.recv_timeout(Duration::from_secs(1)).is_err());
}

#[test]
fn writes_below_a_pruned_folder_do_not_fire() {
    let fixture = Fixture::standard();
    let repository = Repository::open(fixture.path()).unwrap();
    let (fired, on_change) = signal();
    let watcher = worktree_watcher(&repository, on_change);
    fixture.write("target/keep.txt", b"x\n");
    watcher.set_pruned_dirs(&["target".to_owned()], &[]);
    watcher.start().unwrap();
    fixture.write("target/debug.o", b"object\n");
    assert!(fired.recv_timeout(Duration::from_secs(1)).is_err());
    fixture.write("src/new.rs", b"fn x() {}\n");
    assert!(fired.recv_timeout(BOUND).is_ok());
}

#[test]
fn the_factory_makes_live_watchers() {
    let fixture = Fixture::standard();
    let repository = Repository::open(fixture.path()).unwrap();
    let (fired, on_change) = signal();
    let watcher = FsEventsWatchers.ref_watcher(
        &text(repository.git_dir()),
        &text(repository.common_dir()),
        on_change,
    );
    watcher.start().unwrap();
    fixture.git(&["update-ref", "refs/heads/z", "HEAD"]);
    assert!(fired.recv_timeout(BOUND).is_ok());
}
