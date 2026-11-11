//! Ported from `OpenTabsTests.swift` and `ClosedTabsTests.swift`; the `UserDefaults` cases
//! run against `MemoryTabStore`.

use std::collections::HashSet;

use rc_git_kit::tabs::{
    ClosedTabs, HandOff, MemoryTabStore, OpenTabs, clamped_selection, directory_exists, merged,
    mount_folder, restored_paths, restored_selection,
};
use rc_git_testkit::Fixture;

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|path| (*path).to_owned()).collect()
}

fn set(list: &[&str]) -> HashSet<String> {
    list.iter().map(|path| (*path).to_owned()).collect()
}

fn tabs() -> OpenTabs<MemoryTabStore> {
    OpenTabs::new(MemoryTabStore::default())
}

#[test]
fn clamps_selection_into_the_list() {
    assert_eq!(clamped_selection(1, 3), 1);
    assert_eq!(clamped_selection(0, 3), 0);
    assert_eq!(clamped_selection(2, 3), 2);
    assert_eq!(clamped_selection(3, 3), 0);
    assert_eq!(clamped_selection(5, 3), 0);
    assert_eq!(clamped_selection(-1, 3), 0);
    assert_eq!(clamped_selection(i64::MIN, 3), 0);
    assert_eq!(clamped_selection(0, 0), 0);
}

#[test]
fn persists_through_the_store() {
    let mut tabs = tabs();
    assert!(tabs.paths().is_empty());
    assert_eq!(tabs.selected_index(), 0);
    tabs.save(&paths(&["/one", "/two", "/three"]), 2);
    assert_eq!(tabs.paths(), paths(&["/one", "/two", "/three"]));
    assert_eq!(tabs.selected_index(), 2);
    tabs.save(&[], 2);
    assert!(tabs.paths().is_empty());
    assert_eq!(tabs.selected_index(), 0);
}

/// The handover writes the same state as a record and then flushes it; a failed flush is
/// reported so the caller can log it.
#[test]
fn hands_off_the_same_state_it_records_and_flushes() {
    let mut tabs = tabs();
    tabs.save(&paths(&["/one", "/two", "/three"]), 2);
    let outcome = tabs.hand_off(&paths(&["/two", "/one"]), 1);
    assert_eq!(outcome, HandOff::Flushed { count: 2 });
    assert_eq!(tabs.paths(), paths(&["/two", "/one", "/three"]));
    assert_eq!(tabs.selected_index(), 1);
    assert_eq!(tabs.store().flushes, 1);

    let mut failing = OpenTabs::new(MemoryTabStore {
        flush_fails: true,
        ..MemoryTabStore::default()
    });
    assert_eq!(
        failing.hand_off(&paths(&["/a"]), 0),
        HandOff::NotFlushed { count: 1 }
    );
    assert_eq!(failing.paths(), paths(&["/a"]), "the record still lands");
}

#[test]
fn merge_keeps_tabs_that_are_no_longer_open() {
    assert_eq!(
        merged(&paths(&["/two"]), &paths(&["/one", "/two", "/three"])),
        paths(&["/two", "/one", "/three"])
    );
    assert_eq!(
        merged(&[], &paths(&["/one", "/two"])),
        paths(&["/one", "/two"])
    );
    assert_eq!(merged(&paths(&["/one"]), &[]), paths(&["/one"]));
    assert_eq!(
        merged(&paths(&["/one", "/one"]), &paths(&["/one"])),
        paths(&["/one"])
    );
}

fn outside_volumes(path: &str) -> bool {
    !path.starts_with("/Volumes/")
}

/// A repository that failed to open survives the restore only while the folder that holds
/// it is unreachable — a disk or share not mounted yet at launch.
#[test]
fn restore_keeps_failed_paths_whose_parent_is_unreachable() {
    let restored = restored_paths(
        &paths(&["/Users/me/open"]),
        &paths(&[
            "/Volumes/Ext/repo",
            "/Users/me/gone",
            "/Volumes/Share/other",
        ]),
        outside_volumes,
    );
    assert_eq!(
        restored,
        paths(&[
            "/Users/me/open",
            "/Volumes/Ext/repo",
            "/Volumes/Share/other"
        ])
    );
}

/// Nothing opening is not a reason to forget the list: every unmounted repository waits for
/// the next launch, and only the ones gone from a reachable folder are dropped.
#[test]
fn restore_with_nothing_opened_keeps_unreachable_paths() {
    let restored = restored_paths(
        &[],
        &paths(&["/Volumes/Ext/a", "/Users/me/gone"]),
        outside_volumes,
    );
    assert_eq!(restored, paths(&["/Volumes/Ext/a"]));
    assert!(restored_paths(&[], &paths(&["/a"]), |_| true).is_empty());
    assert!(restored_paths(&[], &[], directory_exists).is_empty());
}

/// A volume root's parent is `/Volumes`, which exists whether or not the disk is mounted, so
/// the root itself decides: missing means unmounted, and the path is kept.
#[test]
fn restore_keeps_an_unmounted_volume_root() {
    let mounted = set(&["/", "/Volumes/", "/Volumes/Here/", "/Users/me/"]);
    let restored = restored_paths(
        &[],
        &paths(&[
            "/Volumes/Ext",
            "/Volumes/Here",
            "/Volumes/Gone/",
            "/Users/me/gone",
        ]),
        |path| {
            let folder = if path.ends_with('/') {
                path.to_owned()
            } else {
                format!("{path}/")
            };
            mounted.contains(&folder)
        },
    );
    assert_eq!(restored, paths(&["/Volumes/Ext", "/Volumes/Gone/"]));
}

#[test]
fn mount_folder_is_the_volume_root_or_the_containing_folder() {
    assert_eq!(mount_folder("/Volumes/Ext"), "/Volumes/Ext");
    assert_eq!(mount_folder("/Volumes/Ext/"), "/Volumes/Ext/");
    assert_eq!(mount_folder("/Volumes/Ext/repo"), "/Volumes/Ext/");
    assert_eq!(mount_folder("/Users/me/repo"), "/Users/me/");
    assert_eq!(mount_folder("/Users/me/repo/"), "/Users/me/");
}

#[test]
fn directory_exists_needs_a_folder() {
    let fixture = Fixture::empty_dir();
    fixture.write("file", b"");
    let folder = fixture.path().to_string_lossy().into_owned();
    assert!(directory_exists(&folder));
    assert!(!directory_exists(&format!("{folder}/nope")));
    assert!(!directory_exists(&format!("{folder}/file")));
}

/// Without `dropping` — ⇧⌘W and ⌘Q — what is recorded is the live order plus everything that
/// was saved before it.
#[test]
fn record_without_dropping_keeps_closed_tabs() {
    let mut tabs = tabs();
    tabs.record(&paths(&["/one", "/two"]), 1, None);
    assert_eq!(tabs.paths(), paths(&["/one", "/two"]));

    tabs.record(&paths(&["/one"]), 0, None);
    assert_eq!(tabs.paths(), paths(&["/one", "/two"]));
    assert_eq!(tabs.selected_index(), 0);

    tabs.record(&[], 0, None);
    assert_eq!(tabs.paths(), paths(&["/one", "/two"]));

    tabs.record(&paths(&["/three"]), 0, None);
    assert_eq!(tabs.paths(), paths(&["/three", "/one", "/two"]));
}

/// ⌘W on its own drops that tab's repository; the rest of the saved list stays.
#[test]
fn record_drops_the_tab_closed_on_its_own() {
    let mut tabs = tabs();
    tabs.save(&paths(&["/one", "/two", "/three"]), 0);
    tabs.record(&paths(&["/one"]), 0, Some("/two"));
    assert_eq!(tabs.paths(), paths(&["/one", "/three"]));

    tabs.record(&[], 0, Some("/one"));
    assert_eq!(tabs.paths(), paths(&["/three"]));

    tabs.record(&paths(&["/three"]), 0, Some("/gone"));
    assert_eq!(tabs.paths(), paths(&["/three"]));
}

/// The restore at launch replaces the list, so a repository that no longer opens is not
/// retried on every launch.
#[test]
fn save_replaces_the_list_so_restore_can_prune() {
    let mut tabs = tabs();
    tabs.record(&paths(&["/one", "/gone", "/two"]), 2, None);
    let opened = paths(&["/one", "/two"]);
    let selected = restored_selection(
        &tabs.paths(),
        i64::try_from(tabs.selected_index()).unwrap(),
        &opened,
        None,
    );
    tabs.save(&opened, selected);
    assert_eq!(tabs.paths(), opened);
    assert_eq!(tabs.selected_index(), 1);
}

#[test]
fn restored_selection_follows_the_selected_path() {
    let saved = paths(&["/a", "/b", "/c"]);
    assert_eq!(
        restored_selection(&saved, 2, &paths(&["/b", "/c"]), None),
        1
    );
    assert_eq!(
        restored_selection(&saved, 1, &paths(&["/a", "/b"]), None),
        1
    );
    assert_eq!(
        restored_selection(
            &paths(&["/a", "/gone", "/b", "/c"]),
            2,
            &paths(&["/a", "/b", "/c"]),
            None
        ),
        1
    );
    assert_eq!(restored_selection(&saved, 2, &saved, None), 2);
}

#[test]
fn restored_selection_falls_back_to_first_when_the_selected_path_failed() {
    let saved = paths(&["/a", "/b", "/c"]);
    assert_eq!(
        restored_selection(&saved, 0, &paths(&["/b", "/c"]), None),
        0
    );
    assert_eq!(
        restored_selection(&saved, 2, &paths(&["/a", "/b"]), None),
        0
    );
    assert_eq!(restored_selection(&[], 0, &[], None), 0);
    assert_eq!(restored_selection(&saved, 9, &paths(&["/a"]), None), 0);
    assert_eq!(
        restored_selection(&saved, -4, &paths(&["/b", "/a"]), None),
        1
    );
}

#[test]
fn restored_selection_keeps_the_launch_document() {
    let saved = paths(&["/a", "/b", "/c"]);
    assert_eq!(restored_selection(&saved, 0, &saved, Some("/c")), 2);
    assert_eq!(
        restored_selection(&saved, 1, &paths(&["/a", "/b"]), Some("/gone")),
        1
    );
    assert_eq!(
        restored_selection(&[], 0, &paths(&["/launch"]), Some("/launch")),
        0
    );
}

/// A store holding a selection outside the list (another build, a shrunk list) reads as the
/// first tab.
#[test]
fn a_stored_selection_outside_the_list_reads_as_first() {
    let tabs = OpenTabs::new(MemoryTabStore {
        paths: paths(&["/a", "/b"]),
        selected: 7,
        ..MemoryTabStore::default()
    });
    assert_eq!(tabs.selected_index(), 0);
    let negative = OpenTabs::new(MemoryTabStore {
        paths: paths(&["/a", "/b"]),
        selected: -1,
        ..MemoryTabStore::default()
    });
    assert_eq!(negative.selected_index(), 0);
}

#[test]
fn saved_selection_outside_the_list_falls_back_to_first() {
    let mut tabs = tabs();
    tabs.save(&paths(&["/one", "/two"]), 9);
    assert_eq!(tabs.selected_index(), 0);
    assert_eq!(
        tabs.store().selected,
        0,
        "the clamped value is what gets stored"
    );
}

#[test]
fn empty_has_nothing_to_reopen() {
    let mut closed = ClosedTabs::new();
    assert!(!closed.can_reopen(&set(&[])));
    assert_eq!(closed.pop(&set(&[])), None);
}

#[test]
fn pops_most_recent_first() {
    let mut closed = ClosedTabs::new();
    closed.push("/a");
    closed.push("/b");
    assert_eq!(closed.pop(&set(&[])).as_deref(), Some("/b"));
    assert_eq!(closed.pop(&set(&[])).as_deref(), Some("/a"));
    assert_eq!(closed.pop(&set(&[])), None);
}

#[test]
fn closing_again_moves_to_top() {
    let mut closed = ClosedTabs::new();
    closed.push("/a");
    closed.push("/b");
    closed.push("/a");
    assert_eq!(closed.paths(), paths(&["/b", "/a"]).as_slice());
}

#[test]
fn skips_and_drops_repositories_already_open() {
    let mut closed = ClosedTabs::new();
    closed.push("/a");
    closed.push("/b");
    closed.push("/c");
    assert!(closed.can_reopen(&set(&["/c"])));
    assert_eq!(closed.pop(&set(&["/c", "/b"])).as_deref(), Some("/a"));
    assert!(closed.paths().is_empty());
}

#[test]
fn nothing_to_reopen_when_every_entry_is_open() {
    let mut closed = ClosedTabs::new();
    closed.push("/a");
    assert!(!closed.can_reopen(&set(&["/a"])));
    assert_eq!(closed.pop(&set(&["/a"])), None);
}

#[test]
fn caps_at_capacity_dropping_oldest() {
    let mut closed = ClosedTabs::new();
    for index in 0..=ClosedTabs::CAPACITY {
        closed.push(&format!("/repo{index}"));
    }
    assert_eq!(closed.paths().len(), ClosedTabs::CAPACITY);
    assert!(!closed.paths().contains(&"/repo0".to_owned()));
    assert_eq!(
        closed.paths().last().map(String::as_str),
        Some(format!("/repo{}", ClosedTabs::CAPACITY).as_str())
    );
}
