//! The main menu's key bindings: one chord per command, the standard chords on the standard
//! items, and the table matching the Swift one entry for entry. Port of
//! `MenuShortcutsTests.swift`.

use std::collections::{HashMap, HashSet};

use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::menu_shortcuts::{Chord, Modifiers, SHORTCUTS, search_title, shortcut_for};

/// `MenuShortcuts.swift` as it stood when ported: title, key, and modifiers with ⌘ ⇧ ⌥ ⌃ for
/// command, shift, option and control. An uppercase key carries Shift, as `AppKit` reads it.
const SWIFT_TABLE: [(&str, &str, &str); 44] = [
    ("Settings…", ",", "⌘"),
    ("Hide", "h", "⌘"),
    ("Hide Others", "h", "⌘⌥"),
    ("Quit", "q", "⌘"),
    ("Rebuild and Relaunch Now", "r", "⌘⌥"),
    ("New Tab", "t", "⌘"),
    ("Reopen Closed Tab", "t", "⌘⇧"),
    ("Open Repository…", "o", "⌘"),
    ("Open Recent Repository…", "r", "⌃"),
    ("Go to File…", "o", "⌘⇧"),
    ("Go to Branch…", "b", "⌘"),
    ("Close Tab", "w", "⌘"),
    ("Close Window", "w", "⌘⇧"),
    ("Undo", "z", "⌘"),
    ("Redo", "z", "⌘⇧"),
    ("Cut", "x", "⌘"),
    ("Copy", "c", "⌘"),
    ("Paste", "v", "⌘"),
    ("Paste and Match Style", "v", "⌘⌥⇧"),
    ("Select All", "a", "⌘"),
    ("Find…", "f", "⌘"),
    ("Find Next", "g", "⌘"),
    ("Find Previous", "G", "⌘"),
    ("Use Selection for Find", "e", "⌘"),
    ("Files", "1", "⌘"),
    ("History", "2", "⌘"),
    ("Changes", "3", "⌘"),
    ("Back", "[", "⌘"),
    ("Forward", "]", "⌘"),
    ("Show Hidden Files", ".", "⌘⇧"),
    ("Show Blame", "b", "⌘⌥"),
    ("History of This File", "y", "⌘⌥"),
    ("Search Commits", "F", "⌘⇧"),
    ("Toggle Sidebar", "s", "⌘⌃"),
    ("Enter Full Screen", "f", "⌘⌃"),
    ("Fetch", "f", "⌘⌥"),
    ("Pull", "u", "⌘⇧"),
    ("Push", "p", "⌘⇧"),
    ("Sync", "r", "⌘⇧"),
    ("Back to Default Branch", "m", "⌘⇧"),
    ("Cancel Sync", ".", "⌘"),
    ("Minimize", "m", "⌘"),
    ("Show Previous Tab", "[", "⌘⇧"),
    ("Show Next Tab", "]", "⌘⇧"),
];

/// A Swift row's chord: each symbol mapped to its modifier, the key read as `AppKit` does.
fn swift_chord(key: &str, symbols: &str) -> Option<Chord> {
    let mut names = Vec::new();
    for symbol in symbols.chars() {
        let name = match symbol {
            '⌘' => "cmd",
            '⌃' => "ctrl",
            '⌥' => "alt",
            '⇧' => "shift",
            _ => return None,
        };
        names.push(name);
    }
    names.push(key);
    Chord::parse(&names.join("-"))
}

fn chord(keystroke: &str) -> Chord {
    Chord::parse(keystroke).unwrap_or_else(|| Chord {
        key: format!("unparsable {keystroke}"),
        modifiers: Modifiers::default(),
    })
}

fn owners(target: &Chord) -> Vec<&'static str> {
    SHORTCUTS
        .iter()
        .filter(|row| &chord(row.keystroke) == target)
        .map(|row| row.title)
        .collect()
}

#[test]
fn the_table_matches_the_swift_table_entry_for_entry() {
    assert_eq!(SHORTCUTS.len(), SWIFT_TABLE.len());
    for (row, (title, key, symbols)) in SHORTCUTS.iter().zip(SWIFT_TABLE) {
        assert_eq!(row.title, title, "{}", row.id);
        assert_eq!(
            Some(chord(row.keystroke)),
            swift_chord(key, symbols),
            "{}",
            row.id
        );
    }
}

#[test]
fn every_keystroke_parses_and_is_lowercase() {
    for row in SHORTCUTS {
        assert!(Chord::parse(row.keystroke).is_some(), "{}", row.keystroke);
        assert_eq!(row.keystroke, row.keystroke.to_lowercase(), "{}", row.id);
    }
}

#[test]
fn every_id_is_unique_and_found() {
    let mut ids = HashSet::new();
    for row in SHORTCUTS {
        assert!(ids.insert(row.id), "{} twice", row.id);
        assert_eq!(shortcut_for(row.id), Some(&row));
    }
    assert_eq!(shortcut_for("missing"), None);
}

#[test]
fn no_two_items_share_a_chord() {
    let mut seen: HashMap<Chord, &str> = HashMap::new();
    for row in SHORTCUTS {
        if let Some(other) = seen.insert(chord(row.keystroke), row.title) {
            panic!("{} and {other} share {}", row.title, row.keystroke);
        }
    }
}

#[test]
fn standard_chords_belong_to_the_standard_items() {
    let standard = [
        ("Hide", "cmd-h"),
        ("Hide Others", "cmd-alt-h"),
        ("Quit", "cmd-q"),
        ("Minimize", "cmd-m"),
        ("Enter Full Screen", "cmd-ctrl-f"),
    ];
    for (title, keystroke) in standard {
        assert_eq!(owners(&chord(keystroke)), [title], "{keystroke}");
    }
}

#[test]
fn file_history_leaves_hide_others_alone() {
    let file_history = shortcut_for("file_history").map(|row| chord(row.keystroke));
    let hide_others = shortcut_for("hide_others").map(|row| chord(row.keystroke));
    assert_eq!(file_history, Chord::parse("cmd-alt-y"));
    assert_ne!(file_history, hide_others);
}

#[test]
fn search_title_follows_the_mode() {
    assert_eq!(
        search_title(ContentMode::Files),
        "Find in Repository\u{2026}"
    );
    assert_eq!(search_title(ContentMode::History), "Search Commits");
    assert_eq!(search_title(ContentMode::Changes), "Search Commits");
}

#[test]
fn uppercase_letter_is_the_lowercase_letter_with_shift() {
    assert_eq!(Chord::parse("cmd-G"), Chord::parse("cmd-shift-g"));
    assert_eq!(Chord::parse("cmd-shift-F"), Chord::parse("cmd-shift-f"));
    assert_ne!(Chord::parse("cmd-g"), Chord::parse("cmd-G"));
    assert_eq!(
        Chord::parse("cmd-[").map(|chord| chord.modifiers),
        Some(Modifiers::CMD)
    );
}

#[test]
fn modifier_order_does_not_matter() {
    assert_eq!(Chord::parse("shift-cmd-f"), Chord::parse("cmd-shift-f"));
    let both = Modifiers::CMD.union(Modifiers::SHIFT);
    assert!(both.contains(Modifiers::SHIFT));
    assert!(!both.contains(Modifiers::ALT));
}

#[test]
fn a_dash_key_and_malformed_keystrokes() {
    let dash = Chord::parse("cmd--");
    assert_eq!(dash.as_ref().map(|chord| chord.key.as_str()), Some("-"));
    assert_eq!(dash.map(|chord| chord.modifiers), Some(Modifiers::CMD));
    assert_eq!(Chord::parse("hyper-x"), None, "an unknown modifier");
    assert_eq!(Chord::parse(""), None, "no key");
}
