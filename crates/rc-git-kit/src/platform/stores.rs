//! The persistence halves of the Swift files that kept something in `UserDefaults`, each over
//! a [`SettingsStore`] and under the key in [`keys`]: open tabs, recents, the
//! show-hidden preference, saved commit identities, the sidebar pin, the change-list
//! groupings, and the window and split frames `AppKit` autosaved.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::change_tree::ChangeGrouping;
use crate::commit_identity::{CommitIdentity, SavedIdentities, SavedIdentity, standardized_path};
use crate::layout_policy::{HistoryColumn, SavedHistoryColumns};
use crate::recent_repositories;
use crate::tabs::TabStore;

use super::keys;
use super::settings::{SettingsError, SettingsStore, Value};

/// A settings store shared between the models that persist through it.
pub type SharedSettings = Arc<dyn SettingsStore>;

/// [`TabStore`] over the settings store (`OpenTabs.swift`). [`TabStore::write`] cannot report
/// a failure, so the last one is kept for the caller to log.
pub struct SettingsTabStore {
    settings: SharedSettings,
    last_error: Option<SettingsError>,
}

impl SettingsTabStore {
    /// The open tabs kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self {
            settings,
            last_error: None,
        }
    }

    /// The most recent write or flush failure, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<&SettingsError> {
        self.last_error.as_ref()
    }
}

impl std::fmt::Debug for SettingsTabStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsTabStore")
            .field("last_error", &self.last_error)
            .finish_non_exhaustive()
    }
}

impl TabStore for SettingsTabStore {
    fn paths(&self) -> Vec<String> {
        self.settings
            .string_array(keys::OPEN_TABS)
            .unwrap_or_default()
    }

    fn selected(&self) -> i64 {
        self.settings.int(keys::OPEN_TABS_SELECTED)
    }

    fn write(&mut self, paths: &[String], selected: usize) {
        let selected = i64::try_from(selected).unwrap_or(0);
        let wrote = self
            .settings
            .set_value(keys::OPEN_TABS, Some(Value::strings(paths)))
            .and_then(|()| {
                self.settings
                    .set_value(keys::OPEN_TABS_SELECTED, Some(Value::Int(selected)))
            });
        if let Err(error) = wrote {
            self.last_error = Some(error);
        }
    }

    fn flush(&mut self) -> bool {
        match self.settings.synchronize() {
            Ok(()) => true,
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
    }
}

/// The recent repositories, newest first (`RecentRepositories.swift`).
pub struct RecentRepositories {
    settings: SharedSettings,
}

impl RecentRepositories {
    /// The recents kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    /// The saved list; empty when nothing is saved or the stored value has another type.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        self.settings
            .string_array(keys::RECENT_REPOSITORIES)
            .unwrap_or_default()
    }

    /// Moves `path` to the front of the list.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::RECENT_REPOSITORIES`] when the list could not be saved.
    pub fn note(&self, path: &str) -> Result<(), SettingsError> {
        let updated = recent_repositories::inserting(path, &self.paths());
        self.settings
            .set_value(keys::RECENT_REPOSITORIES, Some(Value::strings(&updated)))
    }

    /// Forgets every recent.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::RECENT_REPOSITORIES`] when the key could not be removed.
    pub fn clear(&self) -> Result<(), SettingsError> {
        self.settings.set_value(keys::RECENT_REPOSITORIES, None)
    }
}

/// Whether hidden files are shown, remembered between launches (`HiddenFiles.swift`).
pub struct HiddenFilesPreference {
    settings: SharedSettings,
}

impl HiddenFilesPreference {
    /// The preference kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    /// Whether hidden files show; off until set.
    #[must_use]
    pub fn shows(&self) -> bool {
        self.settings.bool(keys::SHOW_HIDDEN_FILES)
    }

    /// Flips the preference and returns the new value.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::SHOW_HIDDEN_FILES`] when it could not be saved.
    pub fn toggle(&self) -> Result<bool, SettingsError> {
        let flipped = !self.shows();
        self.settings
            .set_value(keys::SHOW_HIDDEN_FILES, Some(Value::Bool(flipped)))?;
        Ok(flipped)
    }
}

/// Whether diff panes and the file viewer wrap long lines instead of scrolling sideways,
/// remembered between launches. Wrapping is the default: text reads without sideways scrolling.
pub struct WrapLinesPreference {
    settings: SharedSettings,
}

impl WrapLinesPreference {
    /// The preference kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    /// Whether long lines wrap; on until set.
    #[must_use]
    pub fn wraps(&self) -> bool {
        self.settings.value(keys::WRAP_DIFF_LINES).is_none()
            || self.settings.bool(keys::WRAP_DIFF_LINES)
    }

    /// Flips the preference and returns the new value.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::WRAP_DIFF_LINES`] when it could not be saved.
    pub fn toggle(&self) -> Result<bool, SettingsError> {
        let flipped = !self.wraps();
        self.settings
            .set_value(keys::WRAP_DIFF_LINES, Some(Value::Bool(flipped)))?;
        Ok(flipped)
    }
}

/// The History commit list's column widths a drag set, remembered between launches. A column
/// never dragged keeps no width, so its default can change under it.
pub struct HistoryColumnsStore {
    settings: SharedSettings,
}

impl HistoryColumnsStore {
    /// The widths kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    fn key(column: HistoryColumn) -> &'static str {
        match column {
            HistoryColumn::Author => keys::HISTORY_AUTHOR_COLUMN,
            HistoryColumn::Date => keys::HISTORY_DATE_COLUMN,
        }
    }

    fn width(&self, column: HistoryColumn) -> Option<f64> {
        let key = Self::key(column);
        self.settings.value(key)?;
        let width = self.settings.double(key);
        (width.is_finite() && width > 0.0).then_some(width)
    }

    /// The widths saved so far; a missing or unreadable one is `None`.
    #[must_use]
    pub fn load(&self) -> SavedHistoryColumns {
        SavedHistoryColumns {
            author: self.width(HistoryColumn::Author),
            date: self.width(HistoryColumn::Date),
        }
    }

    /// Saves `column`'s dragged width.
    ///
    /// # Errors
    /// [`SettingsError`] naming the column's key when it could not be saved.
    pub fn save(&self, column: HistoryColumn, width: f64) -> Result<(), SettingsError> {
        self.settings
            .set_value(Self::key(column), Some(Value::Double(width)))
    }
}

/// The identity saved per repository (`CommitIdentity.swift`, `CommitIdentityStore`).
///
/// The Swift app keyed entries by `NSString.standardizingPath`, which drops a leading
/// `/private` when the rest still names an existing path; [`standardized_path`] keeps it. So
/// a read that misses under `/private/...` tries the spelling without it, and a save or clear
/// also removes that older spelling, so it cannot come back after a clear.
pub struct CommitIdentityStore {
    settings: SharedSettings,
}

impl CommitIdentityStore {
    /// The identities kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    /// Every stored entry. Read entry by entry: a value that is not a map is dropped alone.
    #[must_use]
    pub fn load(&self) -> SavedIdentities {
        let mut saved = SavedIdentities::default();
        let Some(Value::Dict(dict)) = self.settings.value(keys::COMMIT_IDENTITIES) else {
            return saved;
        };
        for (path, entry) in dict {
            let Some(fields) = entry.as_string_map() else {
                continue;
            };
            let entry = SavedIdentity {
                name: fields.get("name").cloned(),
                email: fields.get("email").cloned(),
            };
            saved.entries.insert(path, entry);
        }
        saved
    }

    /// The identity saved for `repo_path`, under its own spelling or the one without
    /// `/private` the Swift app saved.
    #[must_use]
    pub fn identity(&self, repo_path: &str) -> Option<CommitIdentity> {
        let saved = self.load();
        if let Some(identity) = saved.identity(repo_path) {
            return Some(identity);
        }
        saved.identity(&legacy_spelling(repo_path)?)
    }

    /// Remembers `identity` for `repo_path`.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::COMMIT_IDENTITIES`] when the map could not be saved.
    pub fn save(&self, identity: &CommitIdentity, repo_path: &str) -> Result<(), SettingsError> {
        let mut saved = self.load();
        if let Some(legacy) = legacy_spelling(repo_path) {
            saved.clear(&legacy);
        }
        saved.save(identity, repo_path);
        self.write(&saved)
    }

    /// Forgets the identity saved for `repo_path`, under either spelling.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::COMMIT_IDENTITIES`] when the map could not be saved.
    pub fn clear(&self, repo_path: &str) -> Result<(), SettingsError> {
        let mut saved = self.load();
        if let Some(legacy) = legacy_spelling(repo_path) {
            saved.clear(&legacy);
        }
        saved.clear(repo_path);
        self.write(&saved)
    }

    fn write(&self, saved: &SavedIdentities) -> Result<(), SettingsError> {
        let mut dict = BTreeMap::new();
        for (path, entry) in &saved.entries {
            let mut fields = BTreeMap::new();
            if let Some(name) = &entry.name {
                fields.insert("name".to_owned(), Value::String(name.clone()));
            }
            if let Some(email) = &entry.email {
                fields.insert("email".to_owned(), Value::String(email.clone()));
            }
            dict.insert(path.clone(), Value::Dict(fields));
        }
        self.settings
            .set_value(keys::COMMIT_IDENTITIES, Some(Value::Dict(dict)))
    }
}

/// The key the Swift app saved for `repo_path` when it differs from ours: the standardized
/// path with its leading `/private` dropped. `None` when the path has no such prefix.
fn legacy_spelling(repo_path: &str) -> Option<String> {
    let standard = standardized_path(repo_path);
    let rest = standard.strip_prefix("/private")?;
    rest.starts_with('/').then(|| rest.to_owned())
}

/// Whether the user has dragged the sidebar divider (`LayoutPolicy.swift`, `SidebarWidthPin`).
/// Once pinned, the automatic fit stands back for good.
pub struct SidebarWidthPin {
    settings: SharedSettings,
}

impl SidebarWidthPin {
    /// The pin kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    /// Whether the divider was dragged.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.settings.bool(keys::SIDEBAR_WIDTH_PINNED)
    }

    /// Remembers that the divider was dragged.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::SIDEBAR_WIDTH_PINNED`] when it could not be saved.
    pub fn pin(&self) -> Result<(), SettingsError> {
        self.settings
            .set_value(keys::SIDEBAR_WIDTH_PINNED, Some(Value::Bool(true)))
    }
}

/// One change list's grouping, remembered between launches (`ChangeTree.swift`,
/// `ChangeGroupingStore`). Each list keeps its own key and default.
pub struct ChangeGroupingStore {
    settings: SharedSettings,
    key: &'static str,
    fallback: ChangeGrouping,
}

impl ChangeGroupingStore {
    /// The working tree's Changes list: flat until the user picks folders.
    #[must_use]
    pub fn worktree(settings: SharedSettings) -> Self {
        Self {
            settings,
            key: keys::CHANGES_GROUPING,
            fallback: ChangeGrouping::Flat,
        }
    }

    /// A History commit's changed files: folders until the user picks flat.
    #[must_use]
    pub fn commit(settings: SharedSettings) -> Self {
        Self {
            settings,
            key: keys::COMMIT_CHANGES_GROUPING,
            fallback: ChangeGrouping::Folders,
        }
    }

    /// The stored grouping, or this list's default when none or an unknown one is stored.
    #[must_use]
    pub fn grouping(&self) -> ChangeGrouping {
        self.settings
            .string(self.key)
            .and_then(|stored| ChangeGrouping::parse(&stored))
            .unwrap_or(self.fallback)
    }

    /// Remembers `grouping` for this list.
    ///
    /// # Errors
    /// [`SettingsError`] naming this list's key when it could not be saved.
    pub fn set_grouping(&self, grouping: ChangeGrouping) -> Result<(), SettingsError> {
        self.settings
            .set_value(self.key, Some(Value::String(grouping.as_str().to_owned())))
    }
}

/// A rectangle in `AppKit`'s bottom-left screen coordinates, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Bottom edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// A window frame as `AppKit` autosaves it: `"x y w h sx sy sw sh "`, the window frame then the
/// visible frame of the screen it was on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFrame {
    /// The window's frame.
    pub frame: Rect,
    /// The visible frame of the screen the window was on.
    pub screen: Rect,
}

impl WindowFrame {
    /// Parses `AppKit`'s string; `None` unless it holds eight numbers.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut numbers = Vec::with_capacity(8);
        for word in text.split_whitespace() {
            numbers.push(word.parse::<f64>().ok()?);
        }
        let [x, y, width, height, sx, sy, sw, sh] = numbers.as_slice() else {
            return None;
        };
        Some(Self {
            frame: Rect {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            },
            screen: Rect {
                x: *sx,
                y: *sy,
                width: *sw,
                height: *sh,
            },
        })
    }

    /// `AppKit`'s string, whole points with a trailing space, as `saveFrame(usingName:)` writes.
    #[must_use]
    pub fn format(&self) -> String {
        let mut text = String::new();
        for number in [
            self.frame.x,
            self.frame.y,
            self.frame.width,
            self.frame.height,
            self.screen.x,
            self.screen.y,
            self.screen.width,
            self.screen.height,
        ] {
            let _ = write!(text, "{} ", number.round());
        }
        text
    }

    /// The frame saved under the `AppKit` autosave `name`.
    #[must_use]
    pub fn load(settings: &dyn SettingsStore, name: &str) -> Option<Self> {
        Self::parse(&settings.string(&keys::window_frame_key(name))?)
    }

    /// Saves this frame under the `AppKit` autosave `name`.
    ///
    /// # Errors
    /// [`SettingsError`] naming the `NSWindow Frame` key when it could not be saved.
    pub fn save(&self, settings: &dyn SettingsStore, name: &str) -> Result<(), SettingsError> {
        settings.set_value(
            &keys::window_frame_key(name),
            Some(Value::String(self.format())),
        )
    }
}

/// One split subview's saved frame: `"x, y, w, h, hidden, flag"` as `AppKit` autosaves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubviewFrame {
    /// The subview's frame inside the split.
    pub frame: Rect,
    /// Whether the subview was collapsed (`YES` in the fifth field).
    pub hidden: bool,
    /// `AppKit`'s sixth field, kept as read so a write gives it back unchanged.
    pub flag: bool,
}

impl SubviewFrame {
    /// Parses one entry; `None` unless it holds four numbers and two `YES`/`NO` words.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let fields: Vec<&str> = text.split(',').map(str::trim).collect();
        let [x, y, width, height, hidden, flag] = fields.as_slice() else {
            return None;
        };
        Some(Self {
            frame: Rect {
                x: x.parse().ok()?,
                y: y.parse().ok()?,
                width: width.parse().ok()?,
                height: height.parse().ok()?,
            },
            hidden: parse_yes_no(hidden)?,
            flag: parse_yes_no(flag)?,
        })
    }

    /// `AppKit`'s string: six decimals per number, then `YES`/`NO` twice.
    #[must_use]
    pub fn format(&self) -> String {
        let word = |flag: bool| if flag { "YES" } else { "NO" };
        format!(
            "{:.6}, {:.6}, {:.6}, {:.6}, {}, {}",
            self.frame.x,
            self.frame.y,
            self.frame.width,
            self.frame.height,
            word(self.hidden),
            word(self.flag)
        )
    }

    /// The subview frames saved under the split autosave `name`; `None` when nothing is saved
    /// or any entry does not parse.
    #[must_use]
    pub fn load_split(settings: &dyn SettingsStore, name: &str) -> Option<Vec<Self>> {
        let entries = settings.string_array(&keys::split_view_key(name))?;
        let mut frames = Vec::with_capacity(entries.len());
        for entry in entries {
            frames.push(Self::parse(&entry)?);
        }
        Some(frames)
    }

    /// Saves `frames` under the split autosave `name`.
    ///
    /// # Errors
    /// [`SettingsError`] naming the `NSSplitView Subview Frames` key when it could not be
    /// saved.
    pub fn save_split(
        settings: &dyn SettingsStore,
        name: &str,
        frames: &[Self],
    ) -> Result<(), SettingsError> {
        let mut entries = Vec::with_capacity(frames.len());
        for frame in frames {
            entries.push(frame.format());
        }
        settings.set_value(&keys::split_view_key(name), Some(Value::strings(&entries)))
    }
}

fn parse_yes_no(word: &str) -> Option<bool> {
    match word {
        "YES" => Some(true),
        "NO" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::settings::MemorySettings;
    use crate::tabs::OpenTabs;

    fn scratch() -> Arc<MemorySettings> {
        Arc::new(MemorySettings::new())
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn open_tabs_persist_through_settings() {
        let settings = scratch();
        let mut tabs = OpenTabs::new(SettingsTabStore::new(settings.clone()));
        assert!(tabs.paths().is_empty());
        assert_eq!(tabs.selected_index(), 0);
        tabs.save(&strings(&["/one", "/two", "/three"]), 2);
        let reopened = OpenTabs::new(SettingsTabStore::new(settings.clone()));
        assert_eq!(reopened.paths(), strings(&["/one", "/two", "/three"]));
        assert_eq!(reopened.selected_index(), 2);
        tabs.save(&[], 2);
        assert!(tabs.paths().is_empty());
        assert_eq!(tabs.selected_index(), 0);
    }

    #[test]
    fn a_hand_off_records_then_flushes() {
        let settings = scratch();
        let mut tabs = OpenTabs::new(SettingsTabStore::new(settings.clone()));
        tabs.save(&strings(&["/one", "/two", "/three"]), 2);
        let outcome = tabs.hand_off(&strings(&["/two", "/one"]), 1);
        assert_eq!(outcome, crate::tabs::HandOff::Flushed { count: 2 });
        assert_eq!(tabs.paths(), strings(&["/two", "/one", "/three"]));
        assert_eq!(tabs.selected_index(), 1);
        assert_eq!(settings.synchronize_count(), 1);
    }

    #[test]
    fn a_failed_flush_is_reported_and_kept() {
        let settings = scratch();
        let mut tabs = OpenTabs::new(SettingsTabStore::new(settings.clone()));
        settings.fail_writes(true);
        let outcome = tabs.hand_off(&strings(&["/a"]), 0);
        assert_eq!(outcome, crate::tabs::HandOff::NotFlushed { count: 1 });
        let error = tabs.store().last_error().unwrap();
        assert_eq!(error.op, "synchronize");
    }

    #[test]
    fn record_keeps_closed_tabs_and_drops_only_the_one_closed_alone() {
        let settings = scratch();
        let mut tabs = OpenTabs::new(SettingsTabStore::new(settings));
        tabs.record(&strings(&["/one", "/two"]), 1, None);
        tabs.record(&strings(&["/one"]), 0, None);
        assert_eq!(tabs.paths(), strings(&["/one", "/two"]));
        tabs.record(&[], 0, None);
        assert_eq!(tabs.paths(), strings(&["/one", "/two"]));
        tabs.record(&strings(&["/three"]), 0, None);
        assert_eq!(tabs.paths(), strings(&["/three", "/one", "/two"]));
        tabs.record(&strings(&["/three"]), 0, Some("/one"));
        assert_eq!(tabs.paths(), strings(&["/three", "/two"]));
    }

    #[test]
    fn wrongly_typed_stored_tabs_read_as_empty() {
        let settings = scratch();
        let tabs = OpenTabs::new(SettingsTabStore::new(settings.clone()));
        settings
            .set_value(keys::OPEN_TABS, Some(Value::String("oops".into())))
            .unwrap();
        settings
            .set_value(keys::OPEN_TABS_SELECTED, Some(Value::String("x".into())))
            .unwrap();
        assert!(tabs.paths().is_empty());
        assert_eq!(tabs.selected_index(), 0);
        settings
            .set_value(
                keys::OPEN_TABS,
                Some(Value::Array(vec![
                    Value::String("/a".into()),
                    Value::Int(3),
                ])),
            )
            .unwrap();
        assert!(tabs.paths().is_empty());
        settings
            .set_value(
                keys::OPEN_TABS,
                Some(Value::strings(&strings(&["/a", "/b"]))),
            )
            .unwrap();
        assert_eq!(tabs.selected_index(), 0);
    }

    #[test]
    fn a_saved_selection_outside_the_list_falls_back_to_first() {
        let mut tabs = OpenTabs::new(SettingsTabStore::new(scratch()));
        tabs.save(&strings(&["/one", "/two"]), 9);
        assert_eq!(tabs.selected_index(), 0);
    }

    #[test]
    fn recents_persist_and_a_wrong_type_reads_as_empty() {
        let settings = scratch();
        let recents = RecentRepositories::new(settings.clone());
        assert!(recents.paths().is_empty());
        recents.note("/one").unwrap();
        recents.note("/two").unwrap();
        assert_eq!(recents.paths(), strings(&["/two", "/one"]));
        recents.clear().unwrap();
        assert!(recents.paths().is_empty());

        settings
            .set_value(
                keys::RECENT_REPOSITORIES,
                Some(Value::String("oops".into())),
            )
            .unwrap();
        assert!(recents.paths().is_empty());
        settings
            .set_value(
                keys::RECENT_REPOSITORIES,
                Some(Value::Array(vec![
                    Value::String("/a".into()),
                    Value::Int(3),
                ])),
            )
            .unwrap();
        assert!(recents.paths().is_empty());
        recents.note("/b").unwrap();
        assert_eq!(recents.paths(), strings(&["/b"]));
    }

    #[test]
    fn a_failed_recent_write_names_the_key() {
        let settings = scratch();
        settings.fail_writes(true);
        let error = RecentRepositories::new(settings).note("/a").unwrap_err();
        assert_eq!(error.key, keys::RECENT_REPOSITORIES);
    }

    #[test]
    fn hidden_files_start_off_and_each_toggle_flips_and_remembers() {
        let settings = scratch();
        let preference = HiddenFilesPreference::new(settings.clone());
        assert!(!preference.shows());
        assert!(preference.toggle().unwrap());
        assert!(HiddenFilesPreference::new(settings.clone()).shows());
        assert!(!preference.toggle().unwrap());
        assert!(!HiddenFilesPreference::new(settings).shows());
    }

    #[test]
    fn wrap_lines_starts_on_and_each_toggle_flips_and_remembers() {
        let settings = scratch();
        let preference = WrapLinesPreference::new(settings.clone());
        assert!(preference.wraps());
        assert!(!preference.toggle().unwrap());
        assert!(!WrapLinesPreference::new(settings.clone()).wraps());
        assert!(preference.toggle().unwrap());
        assert!(WrapLinesPreference::new(settings).wraps());
    }

    #[test]
    fn a_failed_wrap_lines_write_names_the_key() {
        let settings = scratch();
        settings.fail_writes(true);
        let error = WrapLinesPreference::new(settings).toggle().unwrap_err();
        assert_eq!(error.key, keys::WRAP_DIFF_LINES);
    }

    #[test]
    fn history_columns_keep_only_the_widths_a_drag_saved() {
        let settings = scratch();
        let store = HistoryColumnsStore::new(settings.clone());
        assert_eq!(store.load(), SavedHistoryColumns::default());
        store.save(HistoryColumn::Date, 150.0).unwrap();
        assert_eq!(
            HistoryColumnsStore::new(settings.clone()).load(),
            SavedHistoryColumns {
                author: None,
                date: Some(150.0)
            }
        );
        store.save(HistoryColumn::Author, 80.0).unwrap();
        assert_eq!(
            HistoryColumnsStore::new(settings).load(),
            SavedHistoryColumns {
                author: Some(80.0),
                date: Some(150.0)
            }
        );
    }

    #[test]
    fn an_unreadable_history_column_width_reads_as_never_dragged() {
        let settings = scratch();
        for (key, value) in [
            (keys::HISTORY_AUTHOR_COLUMN, Value::String("wide".into())),
            (keys::HISTORY_DATE_COLUMN, Value::Double(f64::NAN)),
        ] {
            settings.set_value(key, Some(value)).unwrap();
        }
        assert_eq!(
            HistoryColumnsStore::new(settings).load(),
            SavedHistoryColumns::default()
        );
    }

    #[test]
    fn a_failed_history_column_write_names_the_key() {
        let settings = scratch();
        settings.fail_writes(true);
        let error = HistoryColumnsStore::new(settings)
            .save(HistoryColumn::Author, 90.0)
            .unwrap_err();
        assert_eq!(error.key, keys::HISTORY_AUTHOR_COLUMN);
    }

    fn work() -> CommitIdentity {
        CommitIdentity::parse("Work Me", "me@work.example").unwrap()
    }

    #[test]
    fn an_identity_is_saved_for_one_repository_only() {
        let store = CommitIdentityStore::new(scratch());
        store.save(&work(), "/repos/work").unwrap();
        assert_eq!(store.identity("/repos/work"), Some(work()));
        assert_eq!(store.identity("/repos/work/"), Some(work()));
        assert_eq!(store.identity("/repos/other"), None);
        store.clear("/repos/work").unwrap();
        assert_eq!(store.identity("/repos/work"), None);
    }

    fn stored_identities(entries: &[(&str, &[(&str, &str)])]) -> Value {
        let mut dict = BTreeMap::new();
        for (path, fields) in entries {
            let mut map = BTreeMap::new();
            for (key, value) in *fields {
                map.insert((*key).to_owned(), Value::String((*value).to_owned()));
            }
            dict.insert((*path).to_owned(), Value::Dict(map));
        }
        Value::Dict(dict)
    }

    #[test]
    fn a_stored_identity_that_no_longer_parses_reads_as_absent() {
        let settings = scratch();
        let value = stored_identities(&[
            ("/bad", &[("name", "Long <a@b.c>"), ("email", "a@b.c")]),
            ("/half", &[("name", "Only Name")]),
        ]);
        settings
            .set_value(keys::COMMIT_IDENTITIES, Some(value))
            .unwrap();
        let store = CommitIdentityStore::new(settings.clone());
        assert_eq!(store.identity("/bad"), None);
        assert_eq!(store.identity("/half"), None);

        settings
            .set_value(keys::COMMIT_IDENTITIES, Some(Value::String("x".into())))
            .unwrap();
        assert_eq!(store.identity("/bad"), None);
        store.save(&work(), "/fresh").unwrap();
        assert_eq!(store.identity("/fresh"), Some(work()));
    }

    #[test]
    fn one_malformed_entry_hides_no_other() {
        let settings = scratch();
        let Value::Dict(mut dict) = stored_identities(&[(
            "/good",
            &[("name", "Work Me"), ("email", "me@work.example")],
        )]) else {
            unreachable!("stored_identities builds a dict");
        };
        dict.insert("/weird".to_owned(), Value::Int(3));
        settings
            .set_value(keys::COMMIT_IDENTITIES, Some(Value::Dict(dict)))
            .unwrap();
        assert_eq!(
            CommitIdentityStore::new(settings).identity("/good"),
            Some(work())
        );
    }

    #[test]
    fn a_key_the_swift_app_saved_without_private_still_matches() {
        let settings = scratch();
        let value = stored_identities(&[(
            "/tmp/repo",
            &[("name", "Work Me"), ("email", "me@work.example")],
        )]);
        settings
            .set_value(keys::COMMIT_IDENTITIES, Some(value))
            .unwrap();
        let store = CommitIdentityStore::new(settings);
        assert_eq!(store.identity("/private/tmp/repo"), Some(work()));
        assert_eq!(store.identity("/private/tmp/repo/"), Some(work()));
        assert_eq!(store.identity("/tmp/repo"), Some(work()));
        assert_eq!(store.identity("/privatex/tmp/repo"), None);
    }

    #[test]
    fn saving_or_clearing_retires_the_spelling_without_private() {
        let settings = scratch();
        let value = stored_identities(&[(
            "/tmp/repo",
            &[("name", "Old Me"), ("email", "old@example.com")],
        )]);
        settings
            .set_value(keys::COMMIT_IDENTITIES, Some(value))
            .unwrap();
        let store = CommitIdentityStore::new(settings);
        store.save(&work(), "/private/tmp/repo").unwrap();
        let keys: Vec<String> = store.load().entries.keys().cloned().collect();
        assert_eq!(keys, vec!["/private/tmp/repo".to_owned()]);
        assert_eq!(store.identity("/private/tmp/repo"), Some(work()));
        store.clear("/private/tmp/repo").unwrap();
        assert_eq!(store.identity("/private/tmp/repo"), None);
        assert_eq!(store.identity("/tmp/repo"), None);
    }

    #[test]
    fn clearing_forgets_an_identity_saved_only_without_private() {
        let settings = scratch();
        let value = stored_identities(&[(
            "/tmp/repo",
            &[("name", "Old Me"), ("email", "old@example.com")],
        )]);
        settings
            .set_value(keys::COMMIT_IDENTITIES, Some(value))
            .unwrap();
        let store = CommitIdentityStore::new(settings);
        assert!(store.identity("/private/tmp/repo").is_some());
        store.clear("/private/tmp/repo").unwrap();
        assert_eq!(store.identity("/private/tmp/repo"), None);
        assert!(store.load().entries.is_empty());
    }

    #[test]
    fn the_legacy_spelling_needs_a_whole_private_component() {
        assert_eq!(legacy_spelling("/private/var/x"), Some("/var/x".to_owned()));
        assert_eq!(legacy_spelling("/private"), None);
        assert_eq!(legacy_spelling("/privately/x"), None);
        assert_eq!(legacy_spelling("/var/x"), None);
    }

    #[test]
    fn the_sidebar_is_unpinned_until_pinned_then_remembered() {
        let settings = scratch();
        assert!(!SidebarWidthPin::new(settings.clone()).is_pinned());
        SidebarWidthPin::new(settings.clone()).pin().unwrap();
        assert!(SidebarWidthPin::new(settings).is_pinned());
    }

    #[test]
    fn the_first_pin_key_no_longer_pins() {
        let settings = scratch();
        settings
            .set_value("rc-git.sidebar-width-pinned", Some(Value::Bool(true)))
            .unwrap();
        assert!(!SidebarWidthPin::new(settings).is_pinned());
    }

    #[test]
    fn grouping_persists_and_each_list_keeps_its_own_default() {
        let settings = scratch();
        let worktree = ChangeGroupingStore::worktree(settings.clone());
        assert_eq!(worktree.grouping(), ChangeGrouping::Flat);
        let commit = ChangeGroupingStore::commit(settings.clone());
        assert_eq!(commit.grouping(), ChangeGrouping::Folders);
        commit.set_grouping(ChangeGrouping::Flat).unwrap();
        assert_eq!(
            ChangeGroupingStore::worktree(settings.clone()).grouping(),
            ChangeGrouping::Flat
        );
        worktree.set_grouping(ChangeGrouping::Folders).unwrap();
        assert_eq!(
            ChangeGroupingStore::commit(settings.clone()).grouping(),
            ChangeGrouping::Flat
        );
        assert_eq!(
            ChangeGroupingStore::worktree(settings).grouping(),
            ChangeGrouping::Folders
        );
    }

    #[test]
    fn an_unknown_stored_grouping_falls_back_to_the_list_default() {
        let settings = scratch();
        settings
            .set_value(
                keys::COMMIT_CHANGES_GROUPING,
                Some(Value::String("sideways".into())),
            )
            .unwrap();
        assert_eq!(
            ChangeGroupingStore::commit(settings).grouping(),
            ChangeGrouping::Folders
        );
    }

    #[test]
    fn a_window_frame_round_trips_appkits_string() {
        let text = "-1080 905 1080 893 -1080 -92 1080 1890 ";
        let frame = WindowFrame::parse(text).unwrap();
        assert_eq!(frame.frame.x.to_bits(), (-1080.0_f64).to_bits());
        assert_eq!(frame.frame.height.to_bits(), 893.0_f64.to_bits());
        assert_eq!(frame.screen.height.to_bits(), 1890.0_f64.to_bits());
        assert_eq!(frame.format(), text);
        assert_eq!(WindowFrame::parse("1 2 3"), None);
        assert_eq!(WindowFrame::parse("1 2 3 4 5 6 7 x"), None);
    }

    #[test]
    fn split_frames_round_trip_appkits_strings() {
        let settings = scratch();
        let saved = strings(&[
            "0.000000, 0.000000, 280.000000, 843.000000, NO, NO",
            "281.000000, 0.000000, 610.000000, 843.000000, YES, NO",
        ]);
        let key = keys::split_view_key(keys::MAIN_SPLIT);
        settings
            .set_value(&key, Some(Value::strings(&saved)))
            .unwrap();
        let frames = SubviewFrame::load_split(settings.as_ref(), keys::MAIN_SPLIT).unwrap();
        assert_eq!(frames[0].frame.width.to_bits(), 280.0_f64.to_bits());
        assert!(frames[1].hidden);
        SubviewFrame::save_split(settings.as_ref(), keys::MAIN_SPLIT, &frames).unwrap();
        assert_eq!(settings.string_array(&key), Some(saved));
        assert_eq!(SubviewFrame::parse("1, 2, 3, 4, MAYBE, NO"), None);
    }

    #[test]
    fn a_window_frame_saves_under_appkits_key() {
        let settings = scratch();
        let frame = WindowFrame::parse("10 20 800 600 0 0 1440 900").unwrap();
        frame
            .save(settings.as_ref(), keys::MAIN_WINDOW_FRAME)
            .unwrap();
        assert_eq!(
            settings.string("NSWindow Frame rc-git.window").as_deref(),
            Some("10 20 800 600 0 0 1440 900 ")
        );
        assert_eq!(
            WindowFrame::load(settings.as_ref(), keys::MAIN_WINDOW_FRAME),
            Some(frame)
        );
    }
}
