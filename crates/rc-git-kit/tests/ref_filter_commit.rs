//! The History search's commit filter: day bounds, the listed time, and which searches jump
//! to a revision. Port of `CommitFilterDayBoundsTests.swift`, `CommitListedTimeTests.swift`
//! and `CommitSearchQueryTests.swift`.
#![expect(clippy::unwrap_used, reason = "the fixed commit id is valid hex")]

use rc_git_core::{CommitFilter, CommitSummary, Oid};
use rc_git_kit::ref_filter::{
    CommitSearchScope, day_bounds, filter_is_empty, listed_time, revision,
};

const HOUR: i64 = 3600;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The instant a wall-clock time reads in a zone `offset` seconds east of UTC.
fn instant(offset: i64, date: (i64, i64, i64), hour: i64, minute: i64, second: i64) -> i64 {
    let (year, month, day) = date;
    days_from_civil(year, month, day) * 86_400 + hour * HOUR + minute * 60 + second - offset
}

const HO_CHI_MINH: i64 = 7 * HOUR;

fn ho_chi_minh(_: i64) -> i32 {
    7 * 3600
}

/// `America/New_York` for 2026: EDT from 2026-03-08 07:00 UTC to 2026-11-01 06:00 UTC.
fn new_york(at: i64) -> i32 {
    let edt_start = days_from_civil(2026, 3, 8) * 86_400 + 7 * HOUR;
    let edt_end = days_from_civil(2026, 11, 1) * 86_400 + 6 * HOUR;
    if (edt_start..edt_end).contains(&at) {
        -4 * 3600
    } else {
        -5 * 3600
    }
}

/// `Australia/Sydney` around October 2026: AEST (+10) until 2026-10-03 16:00 UTC, when local
/// 02:00 on the 4th jumps to 03:00 AEDT (+11). East of UTC, so a local midnight read as UTC
/// lands after the jump.
fn sydney(at: i64) -> i32 {
    let aedt_start = days_from_civil(2026, 10, 3) * 86_400 + 16 * HOUR;
    if at >= aedt_start {
        11 * 3600
    } else {
        10 * 3600
    }
}

#[test]
fn a_zone_east_of_utc_finds_its_midnight_before_a_jump() {
    let day = instant(11 * HOUR, (2026, 10, 4), 12, 0, 0);
    let bounds = day_bounds(Some(day), Some(day), sydney);
    let (after, before) = (bounds.after.unwrap(), bounds.before.unwrap());
    assert_eq!(after, instant(10 * HOUR, (2026, 10, 4), 0, 0, 0));
    assert_eq!(before - after + 1, 23 * HOUR);
}

#[test]
fn since_maps_to_local_midnight() {
    let since = instant(HO_CHI_MINH, (2026, 10, 13), 15, 30, 0);
    let bounds = day_bounds(Some(since), None, ho_chi_minh);
    assert_eq!(
        bounds.after,
        Some(instant(HO_CHI_MINH, (2026, 10, 13), 0, 0, 0))
    );
    assert_eq!(bounds.before, None);
}

#[test]
fn until_maps_to_the_last_second_of_the_day() {
    let until = instant(HO_CHI_MINH, (2026, 10, 13), 9, 0, 0);
    let bounds = day_bounds(None, Some(until), ho_chi_minh);
    assert_eq!(
        bounds.before,
        Some(instant(HO_CHI_MINH, (2026, 10, 13), 23, 59, 59))
    );
    assert_eq!(bounds.after, None);
}

#[test]
fn a_later_commit_the_same_day_is_inside_until() {
    let picked = instant(HO_CHI_MINH, (2026, 10, 13), 9, 0, 0);
    let commit = instant(HO_CHI_MINH, (2026, 10, 13), 10, 0, 0);
    let bounds = day_bounds(Some(picked), Some(picked), ho_chi_minh);
    let (after, before) = (bounds.after.unwrap(), bounds.before.unwrap());
    assert!(after <= commit && commit <= before);
}

#[test]
fn a_local_day_differs_from_the_utc_day() {
    // 01:00 on the 14th in Ho Chi Minh City is still the 13th in UTC.
    let picked = instant(HO_CHI_MINH, (2026, 10, 14), 1, 0, 0);
    let bounds = day_bounds(Some(picked), None, ho_chi_minh);
    assert_eq!(
        bounds.after,
        Some(instant(HO_CHI_MINH, (2026, 10, 14), 0, 0, 0))
    );
}

#[test]
fn a_spring_forward_day_spans_twenty_three_hours() {
    let day = instant(-4 * HOUR, (2026, 3, 8), 12, 0, 0);
    let bounds = day_bounds(Some(day), Some(day), new_york);
    let (after, before) = (bounds.after.unwrap(), bounds.before.unwrap());
    assert_eq!(after, instant(-5 * HOUR, (2026, 3, 8), 0, 0, 0));
    assert_eq!(before - after + 1, 23 * HOUR);
}

#[test]
fn a_fall_back_day_spans_twenty_five_hours() {
    let day = instant(-5 * HOUR, (2026, 11, 1), 12, 0, 0);
    let bounds = day_bounds(Some(day), Some(day), new_york);
    let (after, before) = (bounds.after.unwrap(), bounds.before.unwrap());
    assert_eq!(after, instant(-4 * HOUR, (2026, 11, 1), 0, 0, 0));
    assert_eq!(before - after + 1, 25 * HOUR);
}

#[test]
fn no_dates_give_no_bounds() {
    let bounds = day_bounds(None, None, |_| 0);
    assert_eq!((bounds.after, bounds.before), (None, None));
}

fn rebased() -> CommitSummary {
    CommitSummary {
        oid: Oid::from_hex("1a2b3c4d5e6f708192a3b4c5d6e7f80910a1b2c3").unwrap(),
        parents: Vec::new(),
        author_name: "A".to_owned(),
        author_email: "a@example.com".to_owned(),
        author_time: 1_704_412_800,
        author_offset: 0,
        committer_time: 1_704_585_600,
        title: "rebased".to_owned(),
    }
}

#[test]
fn no_filter_shows_author_time() {
    assert_eq!(listed_time(&rebased(), None), 1_704_412_800);
}

#[test]
fn a_filter_without_dates_shows_author_time() {
    let filter = CommitFilter {
        message: Some("fix".to_owned()),
        author: Some("a".to_owned()),
        path: Some("src".to_owned()),
        ..CommitFilter::default()
    };
    assert_eq!(listed_time(&rebased(), Some(&filter)), 1_704_412_800);
}

#[test]
fn a_since_filter_shows_committer_time() {
    let filter = CommitFilter {
        after: Some(1_704_499_200),
        ..CommitFilter::default()
    };
    assert_eq!(listed_time(&rebased(), Some(&filter)), 1_704_585_600);
}

#[test]
fn an_until_filter_shows_committer_time() {
    let filter = CommitFilter {
        message: Some("fix".to_owned()),
        before: Some(1_704_672_000),
        ..CommitFilter::default()
    };
    assert_eq!(listed_time(&rebased(), Some(&filter)), 1_704_585_600);
}

#[test]
fn only_a_filter_with_no_field_set_is_empty() {
    assert!(filter_is_empty(&CommitFilter::default()));
    let path_only = CommitFilter {
        path: Some(String::new()),
        ..CommitFilter::default()
    };
    assert!(!filter_is_empty(&path_only));
}

const MESSAGE: CommitSearchScope = CommitSearchScope::Message;

#[test]
fn four_to_forty_ascii_hex_digits_are_a_revision() {
    assert_eq!(revision("abc1", MESSAGE), Some("abc1"));
    assert_eq!(revision("2024", MESSAGE), Some("2024"));
    assert_eq!(revision("DEADbeef", MESSAGE), Some("DEADbeef"));
    let full = "f".repeat(40);
    assert_eq!(revision(&full, MESSAGE), Some(full.as_str()));
}

#[test]
fn lengths_outside_the_range_are_search_text() {
    assert_eq!(revision("abc", MESSAGE), None);
    assert_eq!(revision(&"f".repeat(41), MESSAGE), None);
    assert_eq!(revision("", MESSAGE), None);
}

#[test]
fn surrounding_whitespace_is_trimmed() {
    assert_eq!(revision("  cafe \t", MESSAGE), Some("cafe"));
    assert_eq!(revision(" abc ", MESSAGE), None);
    assert_eq!(
        revision("cafe\n", MESSAGE),
        None,
        "a line break is not trimmed"
    );
}

#[test]
fn non_ascii_hex_is_search_text() {
    assert_eq!(revision("\u{FF11}\u{FF12}\u{FF13}\u{FF14}", MESSAGE), None);
    assert_eq!(revision("cafe\u{301}", MESSAGE), None);
    assert_eq!(revision("feedg", MESSAGE), None);
    assert_eq!(revision("ab cd", MESSAGE), None);
}

#[test]
fn the_author_scope_never_jumps() {
    assert_eq!(revision("abc1", CommitSearchScope::Author), None);
    assert_eq!(revision("2024", CommitSearchScope::Author), None);
}

/// A zone west of UTC whose DST starts at local midnight, as Sao Paulo's did: -3 until
/// 2018-11-04 03:00 UTC, when local 00:00 jumps to 01:00 at -2. That day has no midnight.
fn sao_paulo_2018(at: i64) -> i32 {
    let jump = days_from_civil(2018, 11, 4) * 86_400 + 3 * HOUR;
    if at >= jump { -2 * 3600 } else { -3 * 3600 }
}

#[test]
fn a_day_without_a_midnight_starts_where_the_jump_lands() {
    let day = instant(-2 * HOUR, (2018, 11, 4), 12, 0, 0);
    let bounds = day_bounds(Some(day), None, sao_paulo_2018);
    assert_eq!(
        bounds.after,
        Some(instant(-2 * HOUR, (2018, 11, 4), 1, 0, 0))
    );
}
