//! The changelog Help ▸ Changelog shows: the shipped file keeps its shape, and the shape check
//! names each way a file can break it.

use rc_git_kit::changelog::{SOURCE, document, problems};

const GOOD: &str = "# Changelog

What changed, newest first.

## 2026-11-17

### Added

- A thing you can do, wrapped
  onto a second line.

### Fixed

- A thing that broke.

## 2026-11-16

### Changed

- A thing that works differently.
";

/// Whether one problem starts with `line N:` and mentions `needle`.
fn reports(found: &[String], line: usize, needle: &str) -> bool {
    let prefix = format!("line {line}: ");
    found
        .iter()
        .any(|problem| problem.starts_with(&prefix) && problem.contains(needle))
}

#[test]
fn the_shipped_changelog_keeps_its_shape() {
    let found = problems(SOURCE);
    assert!(found.is_empty(), "CHANGELOG.md: {found:#?}");
}

#[test]
fn the_document_is_the_shipped_file() {
    let document = document();
    assert_eq!(document.text, SOURCE);
    assert_eq!(document.byte_count, SOURCE.len());
    assert_eq!(document.line_count, SOURCE.lines().count());
    assert!(!document.is_lossy);
}

#[test]
fn a_well_formed_file_has_no_problem() {
    assert_eq!(problems(GOOD), Vec::<String>::new());
}

#[test]
fn a_missing_title_is_reported_on_line_one() {
    let found = problems(&GOOD.replacen("# Changelog", "# Notes", 1));
    assert!(reports(&found, 1, "# Changelog"), "{found:?}");
    let found = problems("");
    assert!(reports(&found, 1, "# Changelog"), "{found:?}");
}

#[test]
fn a_day_must_be_a_date_older_than_the_one_above() {
    let found = problems(&GOOD.replace("2026-11-16", "2026-11-17"));
    assert!(
        reports(&found, 16, "not older than 2026-11-17"),
        "{found:?}"
    );
    let found = problems(&GOOD.replace("2026-11-16", "2026-11-18"));
    assert!(reports(&found, 16, "not older"), "{found:?}");
    let found = problems(&GOOD.replace("2026-11-16", "Nov 16"));
    assert!(reports(&found, 16, "not a YYYY-MM-DD"), "{found:?}");
    let found = problems(&GOOD.replace("2026-11-16", "2026-13-16"));
    assert!(reports(&found, 16, "not a YYYY-MM-DD"), "{found:?}");
    let found = problems(&GOOD.replace("2026-11-16", "2026-11-00"));
    assert!(reports(&found, 16, "not a YYYY-MM-DD"), "{found:?}");
}

#[test]
fn sections_are_known_once_each_and_in_order() {
    let found = problems(&GOOD.replace("### Fixed", "### Removed"));
    assert!(
        reports(&found, 12, "not one of Added, Changed, Fixed"),
        "{found:?}"
    );
    let found = problems(&GOOD.replace("### Fixed", "### Added"));
    assert!(
        reports(&found, 12, "repeats or comes out of the order"),
        "{found:?}"
    );
    let swapped = GOOD
        .replace("### Added", "### Tmp")
        .replace("### Fixed", "### Added")
        .replace("### Tmp", "### Fixed");
    let found = problems(&swapped);
    assert!(reports(&found, 12, "out of the order"), "{found:?}");
}

#[test]
fn a_day_without_a_section_and_a_section_without_an_entry_are_reported() {
    let text = "# Changelog\n\n## 2026-11-17\n\n## 2026-11-16\n\n### Fixed\n\nprose only\n";
    let found = problems(text);
    assert!(reports(&found, 3, "2026-11-17 has no section"), "{found:?}");
    assert!(reports(&found, 7, "no `- ` entry"), "{found:?}");
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn the_last_day_is_checked_at_the_end_of_the_file() {
    let found = problems("# Changelog\n\n## 2026-11-17\n");
    assert!(reports(&found, 3, "has no section"), "{found:?}");
}

#[test]
fn entries_and_headings_outside_the_structure_are_reported() {
    let found = problems("# Changelog\n\n- early entry\n\n### Added\n\n- entry\n");
    assert!(reports(&found, 3, "outside a section"), "{found:?}");
    assert!(reports(&found, 5, "before any day"), "{found:?}");
    let found = problems(&GOOD.replace("### Fixed", "#### Fixed"));
    assert!(reports(&found, 12, "only `## ` days"), "{found:?}");
}
