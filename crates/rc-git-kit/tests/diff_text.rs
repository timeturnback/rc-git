//! Status letters, diff placeholders and hunk ownership. Port of `StatusLetterTests.swift`,
//! `BlobDiffPlaceholderTests.swift` and `DiffHunkRowTests.swift`.

use rc_git_core::bstr::BString;
use rc_git_core::{
    BlobDiff, ChangeKind, DiffHunk, DiffLine, DiffLineKind, LineEnd, WorktreeStatus,
};
use rc_git_kit::diff_text::{
    change_letter, owning_hunk, placeholder, too_large_placeholder, worktree_letter,
};

#[test]
fn worktree_status_letters_match_git_status() {
    assert_eq!(worktree_letter(WorktreeStatus::Modified), Some("M"));
    assert_eq!(worktree_letter(WorktreeStatus::Untracked), Some("U"));
    assert_eq!(worktree_letter(WorktreeStatus::Ignored), Some("I"));
    assert_eq!(worktree_letter(WorktreeStatus::Deleted), Some("D"));
}

#[test]
fn an_unchanged_path_has_no_letter() {
    assert_eq!(worktree_letter(WorktreeStatus::Unchanged), None);
}

#[test]
fn change_kind_letters_match_git_status() {
    assert_eq!(change_letter(ChangeKind::Added), "A");
    assert_eq!(change_letter(ChangeKind::Deleted), "D");
    assert_eq!(change_letter(ChangeKind::Modified), "M");
    assert_eq!(change_letter(ChangeKind::Renamed), "R");
}

fn line(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        text: BString::from(text),
        eol: LineEnd::Lf,
    }
}

fn hunk(starts: (u32, u32), counts: (u32, u32), lines: Vec<DiffLine>) -> DiffHunk {
    DiffHunk {
        old_start: starts.0,
        old_lines: counts.0,
        new_start: starts.1,
        new_lines: counts.1,
        lines,
    }
}

#[test]
fn identical_text_says_no_line_changes() {
    let diff = BlobDiff::Text { hunks: Vec::new() };
    assert_eq!(
        placeholder(&diff, "a/b.txt").as_deref(),
        Some("a/b.txt: no line changes")
    );
}

#[test]
fn text_with_hunks_has_no_placeholder() {
    let diff = BlobDiff::Text {
        hunks: vec![hunk((1, 1), (0, 1), vec![line(DiffLineKind::Added, "x")])],
    };
    assert_eq!(placeholder(&diff, "a.txt"), None);
}

#[test]
fn binary_names_both_sizes() {
    let diff = BlobDiff::Binary {
        old_size: 999,
        new_size: 1_234_567,
    };
    assert_eq!(
        placeholder(&diff, "logo.png").as_deref(),
        Some("logo.png is binary (999 B \u{2192} 1.2 MB)")
    );
}

#[test]
fn too_large_names_size_and_limit() {
    let diff = BlobDiff::TooLarge {
        size: 12_345_678,
        limit: 5_000_000,
    };
    assert_eq!(
        placeholder(&diff, "dump.sql").as_deref(),
        Some("dump.sql is 12 MB, over the 5.0 MB limit")
    );
}

#[test]
fn file_too_large_uses_the_diff_wording() {
    assert_eq!(
        too_large_placeholder("big.bin", 2_000, 1_000),
        "big.bin is 2.0 KB, over the 1.0 KB limit"
    );
}

#[test]
fn sizes_that_read_the_same_fall_back_to_exact_bytes() {
    assert_eq!(
        too_large_placeholder("big.bin", 1_000_001, 1_000_000),
        "big.bin is 1,000,001 bytes, over the 1,000,000-byte limit"
    );
}

#[test]
fn a_size_that_rounds_up_carries_to_the_next_unit() {
    let diff = BlobDiff::Binary {
        old_size: 999_999,
        new_size: 9_960,
    };
    assert_eq!(
        placeholder(&diff, "x").as_deref(),
        Some("x is binary (1.0 MB \u{2192} 10 KB)")
    );
}

fn pane() -> Vec<DiffHunk> {
    vec![
        hunk(
            (1, 1),
            (1, 2),
            vec![
                line(DiffLineKind::Context, "a"),
                line(DiffLineKind::Added, "b"),
            ],
        ),
        hunk((9, 10), (1, 0), vec![line(DiffLineKind::Removed, "z")]),
    ]
}

#[test]
fn a_header_row_owns_its_hunk() {
    let hunks = pane();
    assert_eq!(owning_hunk(0, &hunks), Some(&hunks[0]));
    assert_eq!(owning_hunk(3, &hunks), Some(&hunks[1]));
}

#[test]
fn a_line_row_belongs_to_the_hunk_above_it() {
    let hunks = pane();
    assert_eq!(owning_hunk(1, &hunks), Some(&hunks[0]));
    assert_eq!(owning_hunk(2, &hunks), Some(&hunks[0]));
    assert_eq!(owning_hunk(4, &hunks), Some(&hunks[1]));
}

#[test]
fn a_row_outside_the_pane_has_no_hunk() {
    let hunks = pane();
    assert_eq!(owning_hunk(5, &hunks), None);
    assert_eq!(owning_hunk(0, &[]), None);
}
