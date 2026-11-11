//! Find in file and the viewer's selection.

use rc_git_kit::text_find::{
    MATCH_LIMIT, Selection, find_all, first_from, line_of, selected_text, step,
};

#[test]
fn matches_ignore_case_and_never_overlap() {
    assert_eq!(find_all("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
    assert_eq!(find_all("aaaa", "aa"), vec![0..2, 2..4]);
    assert_eq!(find_all("abc", ""), Vec::<std::ops::Range<usize>>::new());
    assert_eq!(
        find_all("abc", "abcd"),
        Vec::<std::ops::Range<usize>>::new()
    );
}

#[test]
fn matches_are_byte_ranges_of_the_original_text() {
    let text = "ÄBC äbc";
    let found = find_all(text, "äbc");
    assert_eq!(found, vec![0..4, 5..9]);
    assert_eq!(&text[found[1].clone()], "äbc");
}

#[test]
fn a_query_matching_everything_stops_at_the_limit() {
    let text = "a".repeat(MATCH_LIMIT + 5);
    assert_eq!(find_all(&text, "a").len(), MATCH_LIMIT);
}

#[test]
fn stepping_wraps_at_both_ends() {
    assert_eq!(step(0, None, true), None);
    assert_eq!(step(3, None, true), Some(0));
    assert_eq!(step(3, None, false), Some(2));
    assert_eq!(step(3, Some(2), true), Some(0));
    assert_eq!(step(3, Some(0), false), Some(2));
    assert_eq!(step(3, Some(1), true), Some(2));
}

#[test]
fn a_new_query_starts_at_the_first_match_after_the_caret() {
    let matches = vec![0..1, 10..11, 20..21];
    assert_eq!(first_from(&matches, 5), Some(1));
    assert_eq!(first_from(&matches, 10), Some(1));
    assert_eq!(first_from(&matches, 25), Some(0));
    assert_eq!(first_from(&[], 0), None);
}

#[test]
fn an_offset_belongs_to_the_line_it_falls_in() {
    let starts = [0, 4, 9];
    assert_eq!(line_of(&starts, 0), 0);
    assert_eq!(line_of(&starts, 3), 0);
    assert_eq!(line_of(&starts, 4), 1);
    assert_eq!(line_of(&starts, 100), 2);
    assert_eq!(line_of(&[], 3), 0);
}

#[test]
fn a_backwards_drag_selects_the_same_range() {
    let forward = Selection { anchor: 2, head: 7 };
    let backward = Selection { anchor: 7, head: 2 };
    assert_eq!(forward.range(), 2..7);
    assert_eq!(backward.range(), 2..7);
    assert!(Selection::at(4).is_empty());
}

#[test]
fn a_selection_splits_across_the_lines_it_covers() {
    let selection = Selection {
        anchor: 2,
        head: 11,
    };
    assert_eq!(selection.within(&(0..4)), Some(2..4));
    assert_eq!(selection.within(&(5..9)), Some(0..4));
    assert_eq!(selection.within(&(10..14)), Some(0..1));
    assert_eq!(selection.within(&(12..14)), None);
    assert_eq!(Selection::at(3).within(&(0..4)), None);
}

#[test]
fn a_selection_over_an_empty_line_still_marks_its_break() {
    let selection = Selection { anchor: 0, head: 6 };
    assert_eq!(selection.within(&(4..4)), Some(0..0));
}

#[test]
fn a_copy_widens_to_character_boundaries() {
    let text = "aé b";
    assert_eq!(selected_text(text, &Selection { anchor: 0, head: 2 }), "aé");
    assert_eq!(
        selected_text(text, &Selection { anchor: 2, head: 9 }),
        "é b"
    );
    assert_eq!(selected_text(text, &Selection::at(1)), "");
}
