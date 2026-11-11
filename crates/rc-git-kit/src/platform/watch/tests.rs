use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn main_layout(path: &str, git_dir: &str) -> bool {
    is_ref_relevant(path, git_dir, git_dir)
}

const GIT_DIR: &str = "/tmp/repo/.git";

#[test]
fn ref_paths_are_relevant() {
    for path in [
        "/tmp/repo/.git/HEAD",
        "/tmp/repo/.git/packed-refs",
        "/tmp/repo/.git/refs",
        "/tmp/repo/.git/refs/",
        "/tmp/repo/.git/refs/heads/main",
        "/tmp/repo/.git/refs/remotes/origin/main",
        "/tmp/repo/.git/",
    ] {
        assert!(main_layout(path, GIT_DIR), "{path}");
    }
}

#[test]
fn object_and_index_churn_is_ignored() {
    for path in [
        "/tmp/repo/.git/objects/ab/cdef",
        "/tmp/repo/.git/index",
        "/tmp/repo/.git/logs/HEAD",
        "/tmp/repo/.git/COMMIT_EDITMSG",
        "/tmp/repo/.git/refs/heads/main.lock",
        "/tmp/repo/.git/HEAD.lock",
        "/tmp/other/.git/HEAD",
        "/tmp/repo/.github/refs",
    ] {
        assert!(!main_layout(path, GIT_DIR), "{path}");
    }
}

#[test]
fn config_edits_are_relevant_but_its_lock_is_not() {
    assert!(main_layout("/tmp/repo/.git/config", GIT_DIR));
    assert!(!main_layout("/tmp/repo/.git/config.lock", GIT_DIR));
    assert!(!main_layout("/tmp/repo/.git/configx", GIT_DIR));
}

#[test]
fn a_trailing_slash_on_the_git_dir_is_tolerated() {
    assert!(main_layout("/tmp/repo/.git/HEAD", "/tmp/repo/.git/"));
}

#[test]
fn a_sibling_sharing_the_git_dir_prefix_is_dropped() {
    assert!(!main_layout("/tmp/repo/.gitrefs/heads/x", GIT_DIR));
    assert!(!main_layout("/tmp/repo/.git2/HEAD", GIT_DIR));
}

#[test]
fn realpath_events_match_a_canonical_root() {
    let canonical = "/private/tmp/repo/.git";
    assert!(main_layout(
        "/private/tmp/repo/.git/refs/heads/main",
        canonical
    ));
    assert!(!main_layout("/private/tmp/repo/.git/index", canonical));
}

#[test]
fn a_linked_worktree_sees_the_shared_refs_and_its_own_head() {
    let common = "/repo/.git";
    let linked = "/repo/.git/worktrees/linked";
    assert!(is_ref_relevant(
        "/repo/.git/refs/heads/feat",
        "/repo/.git/worktrees/feat",
        common
    ));
    for path in [
        "/repo/.git/refs/heads/wt",
        "/repo/.git/refs/stash",
        "/repo/.git/packed-refs",
        "/repo/.git/config",
        "/repo/.git",
        "/repo/.git/worktrees/linked/HEAD",
        "/repo/.git/worktrees/linked/refs/bisect/bad",
    ] {
        assert!(is_ref_relevant(path, linked, common), "{path}");
    }
}

#[test]
fn a_linked_worktree_ignores_other_worktrees_and_its_own_churn() {
    let common = "/repo/.git";
    let linked = "/repo/.git/worktrees/linked";
    for path in [
        "/repo/.git/worktrees/other/HEAD",
        "/repo/.git/worktrees/other/refs/bisect/bad",
        "/repo/.git/worktrees/linked/index",
        "/repo/.git/worktrees/linked/HEAD.lock",
        "/repo/.git/worktrees/linked/logs/HEAD",
        "/repo/.git/worktrees/linked",
        "/repo/.git/worktrees",
        "/repo/.git/worktrees/linkedrefs",
        "/repo/.git/worktrees/linkedrefs/heads/x",
        "/repo/.git/refs/heads/wt.lock",
        "/repo/.git/packed-refs.lock",
        "/repo/.git/objects/ab/cdef",
        "/repo/.git/index",
    ] {
        assert!(!is_ref_relevant(path, linked, common), "{path}");
    }
    assert!(!is_ref_relevant(
        "/repo/.git/worktrees/wtrefs",
        "/repo/.git/worktrees/wt",
        common
    ));
}

fn dirs(work: &str, git: &str, common: &str) -> WorktreeDirs {
    WorktreeDirs {
        work_dir: work.to_owned(),
        git_dir: git.to_owned(),
        common_dir: common.to_owned(),
    }
}

fn relevant(path: &str) -> bool {
    is_worktree_relevant(
        path,
        &dirs("/repo", "/repo/.git", "/repo/.git"),
        &PrunedRoots::default(),
    )
}

fn roots(items: &[&str]) -> HashSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn file_edits_inside_the_worktree_are_relevant() {
    assert!(relevant("/repo/src/main.rs"));
    assert!(relevant("/repo/new.txt"));
    assert!(relevant("/repo"));
}

#[test]
fn everything_under_the_git_dir_is_dropped() {
    assert!(!relevant("/repo/.git/index"));
    assert!(!relevant("/repo/.git/refs/heads/main"));
    assert!(!relevant("/repo/.git"));
}

#[test]
fn an_exclude_file_edit_is_relevant() {
    assert!(relevant("/repo/.git/info/exclude"));
    assert!(!relevant("/repo/.git/info/attributes"));
}

#[test]
fn a_linked_worktree_reads_the_common_dir_exclude_file() {
    let linked = dirs(
        "/elsewhere/linked",
        "/repo/.git/worktrees/linked",
        "/repo/.git",
    );
    let check = |path: &str| is_worktree_relevant(path, &linked, &PrunedRoots::default());
    assert!(
        check("/repo/.git/info/exclude"),
        "outside the worktree, but its rules apply"
    );
    assert!(check("/elsewhere/linked/a.txt"));
    assert!(
        !check("/repo/.git/worktrees/linked/info/exclude"),
        "git never reads it"
    );
    assert!(!check("/repo/.git/info/attributes"));
    assert!(!check("/repo/.git/index"));
    assert!(
        !check("/repo/a.txt"),
        "the main worktree's files are not this list"
    );
}

#[test]
fn a_dot_git_prefixed_sibling_is_not_the_git_dir() {
    assert!(relevant("/repo/.github/workflows/ci.yml"));
}

#[test]
fn paths_outside_the_worktree_are_dropped() {
    assert!(!relevant("/elsewhere/file"));
    let slashed = dirs("/repo/", "/repo/.git", "/repo/.git");
    assert!(
        is_worktree_relevant("/repo/file", &slashed, &PrunedRoots::default()),
        "a trailing slash on the configured root is normalized"
    );
}

#[test]
fn a_sibling_sharing_the_work_dir_prefix_is_dropped() {
    assert!(!relevant("/repo2/x"));
}

#[test]
fn writes_below_a_pruned_ignored_folder_are_dropped() {
    let pruned = PrunedRoots {
        ignored: roots(&["target", "tools/build"]),
        nested: HashSet::new(),
    };
    let layout = dirs("/repo", "/repo/.git", "/repo/.git");
    let check = |path: &str| is_worktree_relevant(path, &layout, &pruned);
    assert!(!check("/repo/target/debug/x.o"));
    assert!(!check("/repo/target/x"));
    assert!(!check("/repo/tools/build/out/y.o"));
    assert!(
        check("/repo/target"),
        "a file replacing the folder may not be ignored"
    );
    assert!(
        check("/repo/target2/x"),
        "a sibling sharing the prefix is not below it"
    );
    assert!(check("/repo/tools/x"));
    assert!(
        check("/repo/src/target/x"),
        "only the pruned path, not every folder so named"
    );
    assert!(
        check("/repo/.gitignore"),
        "an ignore-rule edit can un-ignore the folder"
    );
    assert!(check("/repo"));
}

#[test]
fn writes_inside_a_nested_repository_are_dropped_but_its_git_stays_relevant() {
    let pruned = PrunedRoots {
        ignored: HashSet::new(),
        nested: roots(&["vendor/lib"]),
    };
    let layout = dirs("/repo", "/repo/.git", "/repo/.git");
    let check = |path: &str| is_worktree_relevant(path, &layout, &pruned);
    assert!(!check("/repo/vendor/lib/a.txt"));
    assert!(!check("/repo/vendor/lib/.git/index"));
    assert!(!check("/repo/vendor/lib/.git/objects/ab/cdef"));
    assert!(
        check("/repo/vendor/lib/.git"),
        "removing it makes the files untracked"
    );
    assert!(
        check("/repo/vendor/lib"),
        "a file replacing the folder is untracked"
    );
    assert!(
        check("/repo/vendor/lib2/a.txt"),
        "a sibling sharing the prefix is not below it"
    );
    assert!(check("/repo/vendor/x"));
}

#[test]
fn no_ignored_folders_keeps_every_worktree_write() {
    assert!(relevant("/repo/target/debug/x.o"));
}

#[test]
fn worktree_realpath_events_match_a_canonical_root() {
    let layout = dirs(
        "/private/tmp/r",
        "/private/tmp/r/.git",
        "/private/tmp/r/.git",
    );
    assert!(is_worktree_relevant(
        "/private/tmp/r/a.txt",
        &layout,
        &PrunedRoots::default()
    ));
    assert!(!is_worktree_relevant(
        "/private/tmp/r/.git/index",
        &layout,
        &PrunedRoots::default()
    ));
}

#[test]
fn the_exclude_file_lives_under_the_common_dir() {
    assert_eq!(exclude_file("/repo/.git/"), "/repo/.git/info/exclude");
}

#[test]
fn excluded_paths_cover_object_ref_and_log_churn_inside_the_worktree_only() {
    assert_eq!(
        excluded_paths("/repo", "/repo/.git"),
        ["/repo/.git/objects", "/repo/.git/refs", "/repo/.git/logs"]
    );
    assert!(excluded_paths("/elsewhere/linked", "/repo/.git/worktrees/linked").is_empty());
    assert!(excluded_paths("/repo", "/repo2/.git").is_empty());
}

#[test]
fn a_linked_worktree_also_watches_the_common_info_folder() {
    assert_eq!(watched_paths("/repo", "/repo/.git"), ["/repo"]);
    assert_eq!(
        watched_paths("/elsewhere/linked", "/repo/.git"),
        ["/elsewhere/linked", "/repo/.git/info"]
    );
    assert_eq!(
        watched_paths("/repo", "/repo2/.git"),
        ["/repo", "/repo2/.git/info"]
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test builds a symlinked folder to resolve; nothing outside its temp dir"
)]
fn filters_resolve_a_symlinked_directory_to_its_realpath() {
    let dir = tempfile::tempdir().unwrap();
    let real = canonical_path::resolve(dir.path());
    let git = real.join(".git");
    std::fs::create_dir(&git).unwrap();
    let link = real.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let linked_git = link.join(".git").to_string_lossy().into_owned();
    let filter = RefFilter::new(&linked_git, &linked_git);
    assert_eq!(filter.watched_path(), git.to_string_lossy());
    let event = format!("{}/refs/heads/x", git.to_string_lossy());
    assert!(filter.any_relevant(&[event]));
}

fn counter() -> (Arc<AtomicUsize>, OnChange) {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    (
        count,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

#[test]
fn a_manual_watcher_fires_only_while_started_and_only_for_relevant_paths() {
    let factory = ManualWatchers::new();
    let (count, on_change) = counter();
    let watcher = factory.ref_watcher(
        "/nonexistent-rc-git/.git",
        "/nonexistent-rc-git/.git",
        on_change,
    );
    assert_eq!(
        factory.emit(&["/nonexistent-rc-git/.git/HEAD"]),
        0,
        "not started"
    );
    watcher.start().unwrap();
    assert_eq!(factory.emit(&["/nonexistent-rc-git/.git/index"]), 0);
    assert_eq!(
        factory.emit(&[
            "/nonexistent-rc-git/.git/index",
            "/nonexistent-rc-git/.git/HEAD"
        ]),
        1
    );
    watcher.stop();
    assert_eq!(factory.emit(&["/nonexistent-rc-git/.git/HEAD"]), 0);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn a_manual_worktree_watcher_filters_on_the_latest_pruned_dirs() {
    let factory = ManualWatchers::new();
    let (count, on_change) = counter();
    let layout = dirs(
        "/nonexistent-rc-git",
        "/nonexistent-rc-git/.git",
        "/nonexistent-rc-git/.git",
    );
    let watcher = factory.worktree_watcher(&layout, on_change);
    watcher.start().unwrap();
    assert_eq!(factory.emit(&["/nonexistent-rc-git/target/x.o"]), 1);
    watcher.set_pruned_dirs(&["target".to_owned()], &[]);
    assert_eq!(factory.emit(&["/nonexistent-rc-git/target/x.o"]), 0);
    drop(watcher);
    assert_eq!(
        factory.emit(&["/nonexistent-rc-git/a.txt"]),
        0,
        "dropping stops it"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(factory.count(), 1);
}

#[test]
fn a_start_error_names_the_call_and_path() {
    let error = WatcherStartError {
        call: "FSEventStreamStart",
        path: "/repo".to_owned(),
    };
    assert_eq!(
        error.to_string(),
        "FSEventStreamStart failed for /repo: changes on disk will not refresh this window"
    );
}
