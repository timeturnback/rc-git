//! Port of `RecentRepositories.swift` (the list rules; persistence lives in the settings
//! store): the repositories opened most recently, newest first.

use unicode_normalization::UnicodeNormalization;

/// The key the settings store keeps the list under.
pub const KEY: &str = "rc-git.recentRepositories";

/// How many recents are kept.
pub const CAPACITY: usize = 10;

/// `existing` with `path` moved to the front, duplicates dropped, and anything past
/// [`CAPACITY`] cut off.
#[must_use]
pub fn inserting(path: &str, existing: &[String]) -> Vec<String> {
    let mut updated = Vec::with_capacity(CAPACITY);
    updated.push(path.to_owned());
    for candidate in existing {
        if updated.len() == CAPACITY {
            break;
        }
        if candidate != path {
            updated.push(candidate.clone());
        }
    }
    updated
}

/// How a recent is written in the Open Recent menu and the picker: `home` (the user's home
/// directory) as `~`. The picker matches against this form, so a query typed as shown finds
/// it.
#[must_use]
pub fn display_path(path: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_owned();
    }
    if path == home {
        return "~".to_owned();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        Some(_) | None => path.to_owned(),
    }
}

/// The recents `query` matches, best first, for the Open Recent Repository picker: every path
/// in recency order when the query is blank, else the matches of `score` against
/// [`display_path`] by score, highest first, with recency breaking ties.
///
/// `score(candidate, query)` is the path search's fuzzy scorer (`None` for no match). Both
/// arguments arrive in Unicode NFC, so a decomposed query matches a precomposed path; the
/// query is trimmed of surrounding spaces and tabs.
#[must_use]
pub fn matching(
    paths: &[String],
    query: &str,
    home: &str,
    score: impl Fn(&str, &str) -> Option<i64>,
) -> Vec<String> {
    let query: String = query.trim_matches(is_space).nfc().collect();
    if query.is_empty() {
        return paths.to_vec();
    }
    let mut scored: Vec<(i64, usize, &String)> = Vec::new();
    for (position, path) in paths.iter().enumerate() {
        let key: String = display_path(path, home).nfc().collect();
        if let Some(points) = score(&key, &query) {
            scored.push((points, position, path));
        }
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    let mut matches = Vec::with_capacity(scored.len());
    for (_, _, path) in scored {
        matches.push(path.clone());
    }
    matches
}

/// Foundation's `.whitespaces`: tab and the space separators, but no line breaks.
fn is_space(ch: char) -> bool {
    let line_break =
        ('\n'..='\r').contains(&ch) || ch == '\u{85}' || ch == '\u{2028}' || ch == '\u{2029}';
    ch.is_whitespace() && !line_break
}

/// What the picker says in place of an empty list: that there are no recents at all, or that
/// none matches what was typed.
#[must_use]
pub fn empty_note(has_recents: bool) -> &'static str {
    if has_recents {
        "No recent repository matches"
    } else {
        "No recent repositories"
    }
}
