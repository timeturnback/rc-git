//! Port of `MenuShortcuts.swift`: every key binding in the main menu, in one table, so a test
//! can prove no two commands share a chord. A menu fires the first match, and the later
//! command would never fire from the keyboard.
//!
//! Keystrokes use GPUI's syntax: modifiers (`cmd`, `ctrl`, `alt`, `shift`) then the key,
//! joined by `-`, all lowercase, as in `cmd-shift-f`. [`Chord::parse`] reads that syntax into
//! a value two bindings compare by.

use crate::back_forward::ContentMode;

/// One menu command's id, title and key binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MenuShortcut {
    /// Stable command id, the Swift table's property name in snake case.
    pub id: &'static str,
    /// The menu item's title. An item whose title carries the app name keeps a placeholder.
    pub title: &'static str,
    /// The binding in GPUI keystroke syntax.
    pub keystroke: &'static str,
}

const fn shortcut(id: &'static str, title: &'static str, keystroke: &'static str) -> MenuShortcut {
    MenuShortcut {
        id,
        title,
        keystroke,
    }
}

/// The title the search command takes while Files is showing.
pub const FIND_IN_REPOSITORY_TITLE: &str = "Find in Repository\u{2026}";

/// The search command's title outside Files.
pub const SEARCH_COMMITS_TITLE: &str = "Search Commits";

/// Every key binding in the main menu, in menu order.
pub const SHORTCUTS: [MenuShortcut; 44] = [
    shortcut("settings", "Settings\u{2026}", "cmd-,"),
    shortcut("hide_app", "Hide", "cmd-h"),
    shortcut("hide_others", "Hide Others", "cmd-alt-h"),
    shortcut("quit", "Quit", "cmd-q"),
    shortcut("rebuild_now", "Rebuild and Relaunch Now", "cmd-alt-r"),
    shortcut("new_tab", "New Tab", "cmd-t"),
    shortcut("reopen_closed_tab", "Reopen Closed Tab", "cmd-shift-t"),
    shortcut("open_repository", "Open Repository\u{2026}", "cmd-o"),
    shortcut(
        "open_recent_repository",
        "Open Recent Repository\u{2026}",
        "ctrl-r",
    ),
    shortcut("go_to_file", "Go to File\u{2026}", "cmd-shift-o"),
    shortcut("go_to_branch", "Go to Branch\u{2026}", "cmd-b"),
    shortcut("close_tab", "Close Tab", "cmd-w"),
    shortcut("close_window", "Close Window", "cmd-shift-w"),
    shortcut("undo", "Undo", "cmd-z"),
    shortcut("redo", "Redo", "cmd-shift-z"),
    shortcut("cut", "Cut", "cmd-x"),
    shortcut("copy", "Copy", "cmd-c"),
    shortcut("paste", "Paste", "cmd-v"),
    shortcut(
        "paste_and_match_style",
        "Paste and Match Style",
        "cmd-alt-shift-v",
    ),
    shortcut("select_all", "Select All", "cmd-a"),
    shortcut("find", "Find\u{2026}", "cmd-f"),
    shortcut("find_next", "Find Next", "cmd-g"),
    shortcut("find_previous", "Find Previous", "cmd-shift-g"),
    shortcut("use_selection_for_find", "Use Selection for Find", "cmd-e"),
    shortcut("files", "Files", "cmd-1"),
    shortcut("history", "History", "cmd-2"),
    shortcut("changes", "Changes", "cmd-3"),
    shortcut("back", "Back", "cmd-["),
    shortcut("forward", "Forward", "cmd-]"),
    shortcut("show_hidden_files", "Show Hidden Files", "cmd-shift-."),
    shortcut("show_blame", "Show Blame", "cmd-alt-b"),
    shortcut("file_history", "History of This File", "cmd-alt-y"),
    shortcut("search_commits", SEARCH_COMMITS_TITLE, "cmd-shift-f"),
    shortcut("toggle_sidebar", "Toggle Sidebar", "cmd-ctrl-s"),
    shortcut("enter_full_screen", "Enter Full Screen", "cmd-ctrl-f"),
    shortcut("fetch", "Fetch", "cmd-alt-f"),
    shortcut("pull", "Pull", "cmd-shift-u"),
    shortcut("push", "Push", "cmd-shift-p"),
    shortcut("sync", "Sync", "cmd-shift-r"),
    shortcut("back_to_default", "Back to Default Branch", "cmd-shift-m"),
    shortcut("cancel_sync", "Cancel Sync", "cmd-."),
    shortcut("minimize", "Minimize", "cmd-m"),
    shortcut("previous_tab", "Show Previous Tab", "cmd-shift-["),
    shortcut("next_tab", "Show Next Tab", "cmd-shift-]"),
];

/// The binding for command `id`, `None` for an id the table lacks.
#[must_use]
pub fn shortcut_for(id: &str) -> Option<&'static MenuShortcut> {
    SHORTCUTS.iter().find(|row| row.id == id)
}

/// The search command's title while `mode` is showing: Files finds text in every file, the
/// other modes search commits. The binding is the same in every mode.
#[must_use]
pub fn search_title(mode: ContentMode) -> &'static str {
    match mode {
        ContentMode::Files => FIND_IN_REPOSITORY_TITLE,
        ContentMode::History | ContentMode::Changes => SEARCH_COMMITS_TITLE,
    }
}

/// A set of modifier keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    /// Command.
    pub const CMD: Self = Self(1);
    /// Control.
    pub const CTRL: Self = Self(1 << 1);
    /// Option.
    pub const ALT: Self = Self(1 << 2);
    /// Shift.
    pub const SHIFT: Self = Self(1 << 3);

    /// Both sets together.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every key in `other` is in this set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The keystroke a binding fires on. An uppercase letter is its lowercase letter with Shift,
/// so `cmd-G` and `cmd-shift-g` are the same chord.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    /// The key, lowercased.
    pub key: String,
    /// The modifiers held with it.
    pub modifiers: Modifiers,
}

impl Chord {
    /// Reads a GPUI keystroke. `None` when a modifier is unknown or the key is missing.
    #[must_use]
    pub fn parse(keystroke: &str) -> Option<Self> {
        let (prefix, key) = match keystroke.rsplit_once('-') {
            Some((prefix, "")) => (prefix.strip_suffix('-').unwrap_or(prefix), "-"),
            Some(split) => split,
            None => ("", keystroke),
        };
        if key.is_empty() {
            return None;
        }
        let mut modifiers = Modifiers::default();
        for name in prefix.split('-').filter(|name| !name.is_empty()) {
            modifiers = modifiers.union(modifier(name)?);
        }
        let lower = key.to_lowercase();
        if lower != key {
            modifiers = modifiers.union(Modifiers::SHIFT);
        }
        Some(Self {
            key: lower,
            modifiers,
        })
    }
}

fn modifier(name: &str) -> Option<Modifiers> {
    match name {
        "cmd" => Some(Modifiers::CMD),
        "ctrl" => Some(Modifiers::CTRL),
        "alt" => Some(Modifiers::ALT),
        "shift" => Some(Modifiers::SHIFT),
        _ => None,
    }
}
