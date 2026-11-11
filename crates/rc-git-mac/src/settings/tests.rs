//! Real `CFPreferences`, each test in a domain of its own: the domain is named by an absolute
//! path inside a temporary folder, so it is stored at `<path>.plist` there rather than in
//! `~/Library/Preferences`, the same isolation `ScratchDefaults.swift` used. Every key a test
//! may write is removed and flushed before the folder goes.

use std::collections::BTreeMap;
use std::sync::Arc;

use rc_git_kit::commit_identity::CommitIdentity;
use rc_git_kit::platform::host_keys::{HostKeyStore, SettingsHostKeyStore};
use rc_git_kit::platform::keys;
use rc_git_kit::platform::stores::{
    ChangeGroupingStore, CommitIdentityStore, HiddenFilesPreference, RecentRepositories,
    SettingsTabStore, SidebarWidthPin, SubviewFrame, WindowFrame,
};
use rc_git_kit::tabs::OpenTabs;

use super::*;

struct Scratch {
    folder: tempfile::TempDir,
    path: String,
    settings: Arc<Preferences>,
}

impl Scratch {
    fn new() -> Self {
        let folder = tempfile::tempdir().unwrap();
        let path = folder
            .path()
            .join("defaults")
            .to_string_lossy()
            .into_owned();
        let settings = Arc::new(Preferences::domain(&path).unwrap());
        Self {
            folder,
            path,
            settings,
        }
    }

    fn shared(&self) -> Arc<dyn SettingsStore> {
        self.settings.clone()
    }

    /// `defaults` reading this domain: the program the Swift app's storage answers to.
    #[expect(
        clippy::disallowed_methods,
        reason = "the test reads the domain back with Apple's own defaults tool"
    )]
    fn defaults(&self, args: &[&str]) -> String {
        let output = std::process::Command::new("/usr/bin/defaults")
            .args(args.iter().map(|arg| arg.replace("{domain}", &self.path)))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let mut every = keys::ALL.map(str::to_owned).to_vec();
        every.push(keys::window_frame_key(keys::MAIN_WINDOW_FRAME));
        every.push(keys::split_view_key(keys::MAIN_SPLIT));
        every.extend([
            "scratch".to_owned(),
            "rc-git.sidebar-width-pinned".to_owned(),
        ]);
        for key in every {
            let _ = self.settings.set_value(&key, None);
        }
        let _ = self.settings.synchronize();
        let _ = &self.folder;
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn every_value_type_reads_back_from_the_domain() {
    let scratch = Scratch::new();
    let mut map = BTreeMap::new();
    map.insert("a".to_owned(), Value::String("x".to_owned()));
    for value in [
        Value::String("rc-git".to_owned()),
        Value::Bool(true),
        Value::Int(42),
        Value::Double(0.25),
        Value::Data(vec![1, 2, 3]),
        Value::strings(&strings(&["/one", "/two"])),
        Value::Dict(map),
    ] {
        scratch
            .settings
            .set_value("scratch", Some(value.clone()))
            .unwrap();
        assert_eq!(scratch.settings.value("scratch"), Some(value));
    }
    scratch.settings.set_value("scratch", None).unwrap();
    assert_eq!(scratch.settings.value("scratch"), None);
    scratch.settings.synchronize().unwrap();
}

#[test]
fn open_tabs_persist_and_defaults_reads_them_as_the_swift_app_wrote_them() {
    let scratch = Scratch::new();
    let mut tabs = OpenTabs::new(SettingsTabStore::new(scratch.shared()));
    assert!(tabs.paths().is_empty());
    tabs.save(&strings(&["/one", "/two", "/three"]), 2);
    let reopened = OpenTabs::new(SettingsTabStore::new(scratch.shared()));
    assert_eq!(reopened.paths(), strings(&["/one", "/two", "/three"]));
    assert_eq!(reopened.selected_index(), 2);
    let outcome = tabs.hand_off(&strings(&["/two", "/one"]), 1);
    assert_eq!(outcome, rc_git_kit::tabs::HandOff::Flushed { count: 2 });

    let listed = scratch.defaults(&["read", "{domain}", "rc-git.openTabs"]);
    let listed: Vec<&str> = listed.lines().map(str::trim).collect();
    assert_eq!(listed, ["(", "\"/two\",", "\"/one\",", "\"/three\"", ")"]);
    let kind = scratch.defaults(&["read-type", "{domain}", "rc-git.openTabs.selected"]);
    assert_eq!(kind.trim(), "Type is integer");
}

#[test]
fn values_the_defaults_tool_writes_read_back() {
    let scratch = Scratch::new();
    scratch.defaults(&[
        "write",
        "{domain}",
        "rc-git.recentRepositories",
        "-array",
        "/b",
        "/a",
    ]);
    scratch.defaults(&[
        "write",
        "{domain}",
        "rc-git.showHiddenFiles",
        "-bool",
        "YES",
    ]);
    scratch.defaults(&["write", "{domain}", "rc-git.openTabs.selected", "-int", "1"]);
    scratch.defaults(&[
        "write",
        "{domain}",
        "rcgit.acceptedHostKeys",
        "-dict",
        "one.test",
        "SHA256:aaa",
    ]);
    let recents = RecentRepositories::new(scratch.shared());
    assert_eq!(recents.paths(), strings(&["/b", "/a"]));
    assert!(HiddenFilesPreference::new(scratch.shared()).shows());
    assert_eq!(scratch.settings.int(keys::OPEN_TABS_SELECTED), 1);
    let hosts = SettingsHostKeyStore::new(scratch.shared());
    assert_eq!(hosts.fingerprint("one.test").as_deref(), Some("SHA256:aaa"));
}

/// Another build may have written these keys with another type; that reads as nothing, and
/// the next write replaces it. The values are stored through `CFPreferences` itself: `defaults
/// write` on a path domain writes the plist behind `cfprefsd`, so a later write from this
/// process would not land on top of it.
#[test]
fn wrongly_typed_stored_values_read_as_empty() {
    let scratch = Scratch::new();
    let set = |key: &str, value: Value| scratch.settings.set_value(key, Some(value)).unwrap();
    set(keys::OPEN_TABS, Value::String("oops".to_owned()));
    set(keys::OPEN_TABS_SELECTED, Value::String("x".to_owned()));
    set(
        keys::RECENT_REPOSITORIES,
        Value::Array(vec![Value::String("/a".to_owned()), Value::Int(3)]),
    );
    let tabs = OpenTabs::new(SettingsTabStore::new(scratch.shared()));
    assert!(tabs.paths().is_empty());
    assert_eq!(tabs.selected_index(), 0);
    let recents = RecentRepositories::new(scratch.shared());
    assert!(recents.paths().is_empty());
    recents.note("/b").unwrap();
    assert_eq!(recents.paths(), strings(&["/b"]));
}

#[test]
fn the_hidden_files_toggle_persists() {
    let scratch = Scratch::new();
    let preference = HiddenFilesPreference::new(scratch.shared());
    assert!(!preference.shows());
    assert!(preference.toggle().unwrap());
    assert!(HiddenFilesPreference::new(scratch.shared()).shows());
    assert!(!preference.toggle().unwrap());
}

#[test]
fn host_key_approvals_keep_their_string_entries_one_by_one() {
    let scratch = Scratch::new();
    let mut entries = BTreeMap::new();
    entries.insert(
        "good.test".to_owned(),
        Value::String("SHA256:aaa".to_owned()),
    );
    entries.insert("bad.test".to_owned(), Value::Int(42));
    scratch
        .settings
        .set_value(keys::ACCEPTED_HOST_KEYS, Some(Value::Dict(entries)))
        .unwrap();
    let store = SettingsHostKeyStore::new(scratch.shared());
    assert_eq!(
        store.fingerprint("good.test").as_deref(),
        Some("SHA256:aaa")
    );
    assert_eq!(store.fingerprint("bad.test"), None);
    store.store("SHA256:bbb", "new.test").unwrap();
    store.forget("good.test").unwrap();
    let reopened = SettingsHostKeyStore::new(scratch.shared());
    assert_eq!(reopened.fingerprint("good.test"), None);
    assert_eq!(
        reopened.fingerprint("new.test").as_deref(),
        Some("SHA256:bbb")
    );
    scratch.settings.synchronize().unwrap();
    let listed = scratch.defaults(&["read", "{domain}", "rcgit.acceptedHostKeys"]);
    assert!(listed.contains("\"new.test\" = \"SHA256:bbb\""), "{listed}");
}

/// The map as the Swift app saved it: keyed by `standardizingPath`, which drops `/private`.
#[test]
fn an_identity_the_swift_app_saved_without_private_still_matches() {
    let scratch = Scratch::new();
    let mut fields = BTreeMap::new();
    fields.insert("name".to_owned(), Value::String("Work Me".to_owned()));
    fields.insert(
        "email".to_owned(),
        Value::String("me@work.example".to_owned()),
    );
    let mut entries = BTreeMap::new();
    entries.insert("/tmp/repo".to_owned(), Value::Dict(fields));
    scratch
        .settings
        .set_value(keys::COMMIT_IDENTITIES, Some(Value::Dict(entries)))
        .unwrap();
    let store = CommitIdentityStore::new(scratch.shared());
    let work = CommitIdentity::parse("Work Me", "me@work.example").unwrap();
    assert_eq!(store.identity("/private/tmp/repo"), Some(work.clone()));
    store.save(&work, "/private/tmp/repo").unwrap();
    scratch.settings.synchronize().unwrap();
    let listed = scratch.defaults(&["read", "{domain}", "rc-git.commitIdentities"]);
    assert!(listed.contains("\"/private/tmp/repo\""), "{listed}");
    assert!(!listed.contains("\"/tmp/repo\""), "{listed}");
    store.clear("/private/tmp/repo").unwrap();
    assert_eq!(store.identity("/private/tmp/repo"), None);
    assert_eq!(store.identity("/tmp/repo"), None);
}

#[test]
fn the_pin_and_groupings_persist() {
    let scratch = Scratch::new();
    assert!(!SidebarWidthPin::new(scratch.shared()).is_pinned());
    SidebarWidthPin::new(scratch.shared()).pin().unwrap();
    assert!(SidebarWidthPin::new(scratch.shared()).is_pinned());
    let worktree = ChangeGroupingStore::worktree(scratch.shared());
    worktree
        .set_grouping(rc_git_kit::change_tree::ChangeGrouping::Folders)
        .unwrap();
    let read = scratch.defaults(&["read", "{domain}", "rc-git.changesGrouping"]);
    assert_eq!(read.trim(), "folders");
}

#[test]
fn appkit_frames_read_back_in_their_own_format() {
    let scratch = Scratch::new();
    scratch.defaults(&[
        "write",
        "{domain}",
        "NSWindow Frame rc-git.window",
        "-string",
        "-1080 905 1080 893 -1080 -92 1080 1890 ",
    ]);
    scratch.defaults(&[
        "write",
        "{domain}",
        "NSSplitView Subview Frames rc-git.main.v2",
        "-array",
        "0.000000, 0.000000, 280.000000, 843.000000, NO, NO",
        "281.000000, 0.000000, 610.000000, 843.000000, NO, NO",
    ]);
    let frame = WindowFrame::load(scratch.settings.as_ref(), keys::MAIN_WINDOW_FRAME).unwrap();
    assert_eq!(frame.format(), "-1080 905 1080 893 -1080 -92 1080 1890 ");
    let splits = SubviewFrame::load_split(scratch.settings.as_ref(), keys::MAIN_SPLIT).unwrap();
    assert_eq!(splits.len(), 2);
    assert_eq!(
        splits[1].format(),
        "281.000000, 0.000000, 610.000000, 843.000000, NO, NO"
    );
}

#[test]
fn the_current_application_domain_opens() {
    let settings = Preferences::current_application();
    assert_eq!(
        format!("{settings:?}"),
        "Preferences { domain: \"current application\" }"
    );
}
