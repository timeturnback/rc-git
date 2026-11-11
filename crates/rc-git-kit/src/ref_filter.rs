//! Port of `RefFilter.swift` and the parts of `Models.swift` the ref list and the History
//! search decide with: the ref list's natural order (`RefListing`), the filter's matching and
//! Return pick, and `CommitFilter`'s day bounds, listed time and revision jump.

use rc_git_core::{BrokenRef, CommitFilter, CommitSummary, RefEntry, RefKind, RefListing};

use crate::natural_sort;

/// One group of the ref list: a title and its refs in natural order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSection {
    /// What the list heads the group with.
    pub title: &'static str,
    /// The namespace every ref in the group comes from.
    pub kind: RefKind,
    /// The group's refs, in natural order of short name.
    pub refs: Vec<RefEntry>,
}

/// The order the ref list shows its sections in, which is also the tie-break order of
/// [`RefList::best_match`].
pub const SECTION_ORDER: [(&str, RefKind); 4] = [
    ("Branches", RefKind::LocalBranch),
    ("Remotes", RefKind::RemoteBranch),
    ("Tags", RefKind::Tag),
    ("Other", RefKind::Other),
];

/// The title of the ref list's last section: the references whose target could not be read.
/// Its rows are listed so a broken ref does not look like a missing one, and are never picked.
pub const UNREADABLE_TITLE: &str = "Unreadable";

/// Every reference in a repository plus the ref list's display order. The order is sorted
/// once, when the list is built or its refs change, so filtering never sorts.
#[derive(Debug, Clone)]
pub struct RefList {
    listing: RefListing,
    /// Every section, unfiltered: section order, natural order within each.
    sorted_sections: Vec<RefSection>,
    /// `listing.broken` in natural order of full name.
    sorted_broken: Vec<BrokenRef>,
}

/// Equal when the refs are: the sorted copies follow from them.
impl PartialEq for RefList {
    fn eq(&self, other: &Self) -> bool {
        self.listing == other.listing
    }
}

impl Eq for RefList {}

impl RefList {
    /// Sorts `listing` for display.
    #[must_use]
    pub fn new(listing: RefListing) -> Self {
        let sorted_sections = natural_sections(&listing.refs);
        let sorted_broken = natural_order(&listing.broken);
        Self {
            listing,
            sorted_sections,
            sorted_broken,
        }
    }

    /// The listing as the core read it.
    #[must_use]
    pub fn listing(&self) -> &RefListing {
        &self.listing
    }

    /// Replaces the resolved refs and sorts them again.
    pub fn set_refs(&mut self, refs: Vec<RefEntry>) {
        self.sorted_sections = natural_sections(&refs);
        self.listing.refs = refs;
    }

    /// Replaces the broken refs and sorts them again.
    pub fn set_broken(&mut self, broken: Vec<BrokenRef>) {
        self.sorted_broken = natural_order(&broken);
        self.listing.broken = broken;
    }

    /// The broken refs whose full name contains `filter`, ignoring case, sorted naturally. A
    /// blank filter keeps every one; an empty result means the section is left out.
    #[must_use]
    pub fn unreadable(&self, filter: &str) -> Vec<BrokenRef> {
        let Some(query) = query(filter) else {
            return self.sorted_broken.clone();
        };
        let mut matches = Vec::new();
        for broken in &self.sorted_broken {
            if broken.full_name.to_lowercase().contains(&query) {
                matches.push(broken.clone());
            }
        }
        matches
    }

    /// The refs whose short name contains `filter`, ignoring case, grouped in section order
    /// and sorted naturally (`v2` before `v10`) within each. Empty sections are dropped; a
    /// blank filter keeps every ref. Filters the sorted sections and never sorts.
    #[must_use]
    pub fn sections(&self, filter: &str) -> Vec<RefSection> {
        let Some(query) = query(filter) else {
            return self.sorted_sections.clone();
        };
        let mut sections = Vec::new();
        for section in &self.sorted_sections {
            let refs: Vec<RefEntry> = section
                .refs
                .iter()
                .filter(|entry| rank(entry, &query).is_some())
                .cloned()
                .collect();
            if !refs.is_empty() {
                sections.push(RefSection {
                    title: section.title,
                    kind: section.kind,
                    refs,
                });
            }
        }
        sections
    }

    /// The ref Return should pick for `filter`: an exact short name first, then a short-name
    /// prefix, then a last path component that starts with the filter (`main` finds
    /// `origin/main`), then any other substring. Ties go to the earlier section, then to the
    /// natural order. `None` when the filter is blank or nothing matches.
    #[must_use]
    pub fn best_match(&self, filter: &str) -> Option<RefEntry> {
        let query = query(filter)?;
        let mut best: Option<(&RefEntry, u8)> = None;
        for section in &self.sorted_sections {
            for entry in &section.refs {
                let Some(rank) = rank(entry, &query) else {
                    continue;
                };
                if best.is_none_or(|(_, best_rank)| rank < best_rank) {
                    best = Some((entry, rank));
                }
            }
        }
        best.map(|(entry, _)| entry.clone())
    }
}

/// The filter as matching uses it: trimmed and lowercased, or `None` when it is blank and so
/// filters nothing.
#[must_use]
pub fn query(filter: &str) -> Option<String> {
    let query = filter.trim().to_lowercase();
    if query.is_empty() { None } else { Some(query) }
}

/// Whether the Working Tree row stays listed under `filter`: always for a blank filter,
/// otherwise only when "working tree" contains it, ignoring case, like every ref row.
#[must_use]
pub fn working_tree_matches(filter: &str) -> bool {
    query(filter).is_none_or(|query| "working tree".contains(&query))
}

/// `refs` grouped into the ref list's sections, each sorted naturally; a kind with no refs has
/// no section. The one place the ref list sorts.
fn natural_sections(refs: &[RefEntry]) -> Vec<RefSection> {
    let mut sections = Vec::new();
    for (title, kind) in SECTION_ORDER {
        let mut members: Vec<RefEntry> = refs
            .iter()
            .filter(|entry| entry.kind == kind)
            .cloned()
            .collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by(|a, b| natural_sort::compare(&a.short_name, &b.short_name));
        sections.push(RefSection {
            title,
            kind,
            refs: members,
        });
    }
    sections
}

/// `broken` sorted naturally by full name.
fn natural_order(broken: &[BrokenRef]) -> Vec<BrokenRef> {
    let mut sorted = broken.to_vec();
    sorted.sort_by(|a, b| natural_sort::compare(&a.full_name, &b.full_name));
    sorted
}

/// How well `entry` matches a non-empty lowercased query, lower is better; `None` when it does
/// not match at all.
fn rank(entry: &RefEntry, query: &str) -> Option<u8> {
    let name = entry.short_name.to_lowercase();
    let last_component = name
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(&name);
    if name == query {
        Some(0)
    } else if name.starts_with(query) {
        Some(1)
    } else if last_component.starts_with(query) {
        Some(2)
    } else if name.contains(query) {
        Some(3)
    } else {
        None
    }
}

/// The time the History Date column shows under `filter`: committer time while the filter
/// bounds dates, since that is the time it compares, and author time otherwise. Showing author
/// time there would place a rebased commit outside the range it matched.
#[must_use]
pub fn listed_time(commit: &CommitSummary, filter: Option<&CommitFilter>) -> i64 {
    match filter {
        Some(filter) if filter.after.is_some() || filter.before.is_some() => commit.committer_time,
        Some(_) | None => commit.author_time,
    }
}

/// True when no field of `filter` is set, which would match every commit.
#[must_use]
pub fn filter_is_empty(filter: &CommitFilter) -> bool {
    *filter == CommitFilter::default()
}

/// The inclusive instants a Since/Until day pick filters by, seconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayBounds {
    /// The first second of the `since` day.
    pub after: Option<i64>,
    /// The last second of the `until` day.
    pub before: Option<i64>,
}

const DAY: i64 = 86_400;

/// The `after` and `before` instants for a Since/Until day pick: the first second of the
/// `since` day and the last second of the `until` day, whatever time of day the picked instants
/// carry. The day end is the next day's start less one second, so a day a DST change makes 23
/// or 25 hours long is covered whole.
///
/// Swift reads days in the user's local zone through `Calendar`; this crate takes no time-zone
/// dependency, so the caller passes the zone as `offset_at`: the UTC offset in seconds east
/// that is in effect at a given instant. A fixed zone is `|_| offset`. A local midnight that a
/// DST jump skips resolves to the instant the jump lands on, as `startOfDay` does.
#[must_use]
pub fn day_bounds(
    since: Option<i64>,
    until: Option<i64>,
    offset_at: impl Fn(i64) -> i32,
) -> DayBounds {
    let after = since.map(|instant| day_start(local_day(instant, &offset_at), &offset_at));
    let before = until.map(|instant| day_start(local_day(instant, &offset_at) + 1, &offset_at) - 1);
    DayBounds { after, before }
}

/// Days since the epoch of the local calendar day `instant` falls on.
fn local_day(instant: i64, offset_at: &impl Fn(i64) -> i32) -> i64 {
    (instant + i64::from(offset_at(instant))).div_euclid(DAY)
}

/// The first instant of local day `day`: its midnight, or for a midnight a DST jump skips,
/// the instant the jump lands on.
fn day_start(day: i64, offset_at: &impl Fn(i64) -> i32) -> i64 {
    let midnight = day * DAY;
    let guess = midnight - i64::from(offset_at(midnight));
    let instant = midnight - i64::from(offset_at(guess));
    if local_day(instant, offset_at) == day {
        instant
    } else {
        guess.max(instant)
    }
}

/// Which commit field the History search box matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitSearchScope {
    /// The commit message.
    Message,
    /// The author name or email.
    Author,
}

/// How many hex digits a submitted query may hold and still count as an abbreviated hash.
pub const REVISION_LENGTHS: std::ops::RangeInclusive<usize> = 4..=40;

/// The revision to try for a submitted History search, or `None` when it is only search text:
/// 4 to 40 ASCII hex digits once trimmed of spaces and tabs, and never in the author scope.
/// Only ASCII counts, so fullwidth digits stay search text.
#[must_use]
pub fn revision(query: &str, scope: CommitSearchScope) -> Option<&str> {
    match scope {
        CommitSearchScope::Author => None,
        CommitSearchScope::Message => {
            let trimmed = query.trim_matches(is_horizontal_space);
            let is_hex = trimmed.bytes().all(|byte| byte.is_ascii_hexdigit());
            (REVISION_LENGTHS.contains(&trimmed.len()) && is_hex).then_some(trimmed)
        }
    }
}

/// Foundation's `CharacterSet.whitespaces`: tab and the space separators, no line breaks.
fn is_horizontal_space(c: char) -> bool {
    let line_break = matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    );
    c.is_whitespace() && !line_break
}
