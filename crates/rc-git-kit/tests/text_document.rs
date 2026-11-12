//! Port of `TextDocumentTests.swift` and `FileLimitsTests.swift`, with every UTF-16 offset
//! restated as a UTF-8 byte offset.

use rc_git_kit::text_document::{
    FileReveal, HARD_LIMIT, TextDocument, can_force_load, format_bytes, format_over_limit,
    line_number_label,
};

/// The core's line starts for valid text: 0, then after each LF that is not the last byte.
fn core_line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = if bytes.is_empty() { vec![] } else { vec![0] };
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && index + 1 < bytes.len() {
            starts.push(index + 1);
        }
    }
    starts
}

/// Builds a document the way a text blob read from the core arrives.
fn document(text: &str) -> TextDocument {
    let bytes = text.as_bytes().to_vec();
    let starts = core_line_starts(&bytes);
    TextDocument::new(bytes, &starts, true)
}

/// A reference walk over decoded characters, as the Swift side's UTF-16 walk did, in bytes.
fn walked(text: &str) -> (Vec<usize>, Vec<usize>) {
    if text.is_empty() {
        return (vec![], vec![]);
    }
    let mut starts = vec![0];
    let mut non_lf = vec![];
    let mut previous_cr: Option<usize> = None;
    for (offset, ch) in text.char_indices() {
        if let Some(cr) = previous_cr.take()
            && ch != '\n'
        {
            non_lf.push(cr);
        }
        match ch {
            '\n' => starts.push(offset + 1),
            '\r' => previous_cr = Some(offset),
            '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}' => non_lf.push(offset),
            _ => {}
        }
    }
    if let Some(cr) = previous_cr {
        non_lf.push(cr);
    }
    if starts.len() > 1 && starts.last() == Some(&text.len()) {
        starts.pop();
    }
    (starts, non_lf)
}

#[test]
fn decodes_utf8_and_counts_lines() {
    let document = TextDocument::new(b"a\nb\n".to_vec(), &[0, 2], true);
    assert_eq!(document.text, "a\nb\n");
    assert_eq!(document.line_count, 2);
    assert_eq!(document.byte_count, 4);
    assert!(!document.is_lossy);
}

#[test]
fn replaces_invalid_utf8_and_flags_it() {
    let document = TextDocument::new(vec![0x66, 0xff, 0x6f], &[0], false);
    assert_eq!(document.text, "f\u{fffd}o");
    assert!(document.is_lossy);
    assert_eq!(
        document.byte_count, 3,
        "the stored size, not the decoded one"
    );
}

#[test]
fn line_starts_follow_the_core_rule() {
    assert_eq!(document("").line_starts, Vec::<usize>::new());
    assert_eq!(document("a").line_starts, [0]);
    assert_eq!(document("a\n").line_starts, [0]);
    assert_eq!(document("a\nb").line_starts, [0, 2]);
    assert_eq!(document("a\nb\n").line_starts, [0, 2]);
    assert_eq!(document("\n\n").line_starts, [0, 1]);
}

#[test]
fn line_starts_count_bytes() {
    // "é" is 2 UTF-8 bytes; "😀" is 4.
    assert_eq!(document("é\n😀\nx").line_starts, [0, 3, 8]);
}

#[test]
fn non_lf_breaks_record_every_separator_a_layout_breaks_on() {
    let breaks = |text: &str| document(text).non_lf_breaks;
    assert_eq!(breaks("a\nb"), Vec::<usize>::new());
    assert_eq!(breaks("a\rb"), [1]);
    assert_eq!(breaks("a\r\nb"), Vec::<usize>::new());
    assert_eq!(breaks("a\rb\rc\r"), [1, 3, 5]);
    assert_eq!(breaks("a\u{0C}\nb"), [1]);
    assert_eq!(breaks("a\u{0B}b\u{85}c"), [1, 3]);
    assert_eq!(breaks("a\u{2028}b\u{2029}c"), [1, 5]);
    assert_eq!(breaks(""), Vec::<usize>::new());
}

#[test]
fn non_lf_breaks_leave_line_starts_on_lf() {
    let document = document("😀\r\u{2028}x\ny");
    assert_eq!(document.line_starts, [0, 10]);
    assert_eq!(document.non_lf_breaks, [4, 5]);
}

#[test]
fn byte_breaks_match_a_character_walk() {
    for text in [
        "plain ascii\nsecond line",
        "é\nñ ü",
        "日本\n語",
        "😀\n𝄞x",
        "",
        "a\n",
        "\n",
        "😀\r\u{2028}x\ny",
        "a\r\nb\rc\u{0B}d\u{0C}e\u{85}f\u{2029}g\r",
    ] {
        let document = document(text);
        let (starts, non_lf) = walked(text);
        assert_eq!(document.line_starts, starts, "{text:?}");
        assert_eq!(document.non_lf_breaks, non_lf, "{text:?}");
    }
}

#[test]
fn lossy_text_walks_the_decoded_text() {
    // U+FFFD is three bytes standing in for one invalid byte, so line 2 starts at 4 in the
    // decoded text, not at 2 as in the stored bytes.
    let document = TextDocument::new(vec![0x80, 0x0A, 0x61], &[0, 2], false);
    assert_eq!(document.text, "\u{FFFD}\na");
    assert_eq!(document.line_starts, [0, 4]);
    assert_eq!(document.line_count, 2);
}

#[test]
fn match_ranges_map_core_byte_columns() {
    let document = document("é x\nab😀cd\n");
    assert_eq!(document.match_range(0, 3, 1), Some(3..4));
    assert_eq!(
        document.match_range(1, 2, 4),
        Some(7..11),
        "an emoji is four bytes"
    );
    assert_eq!(document.match_range(1, 0, 0), Some(5..5));
    assert_eq!(document.match_range(2, 0, 1), None, "past the last line");
    assert_eq!(
        document.match_range(1, 3, 1),
        Some(5..5),
        "a split character is empty"
    );
    assert_eq!(
        document.match_range(1, 4, 99),
        Some(5..5),
        "past the end is empty"
    );
    assert_eq!(
        document.match_range(1, usize::MAX, 1),
        Some(5..5),
        "overflow is empty"
    );
}

#[test]
fn lossy_match_ranges_stay_empty_at_the_line_start() {
    let document = TextDocument::new(b"\xffab\ncd".to_vec(), &[0, 4], false);
    assert_eq!(document.match_range(0, 1, 2), Some(0..0));
    assert_eq!(document.match_range(1, 0, 2), Some(6..6));
}

#[test]
fn only_a_match_needs_the_source_view() {
    assert!(!FileReveal::Fragment("intro".to_owned()).needs_source());
    let reveal = FileReveal::Match {
        line: 3,
        byte_column: 1,
        byte_length: 2,
    };
    assert!(reveal.needs_source());
}

#[test]
fn byte_format_uses_short_units() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(999), "999 B");
    assert_eq!(format_bytes(1_000), "1.0 KB");
    assert_eq!(format_bytes(12_345), "12 KB");
    assert_eq!(format_bytes(1_234_567), "1.2 MB");
    assert_eq!(format_bytes(5_000_000_000), "5.0 GB");
}

#[test]
fn byte_format_rounds_across_unit_boundaries() {
    assert_eq!(format_bytes(9_960), "10 KB");
    assert_eq!(format_bytes(9_999), "10 KB");
    assert_eq!(format_bytes(999_499), "999 KB");
    assert_eq!(format_bytes(999_500), "1.0 MB");
    assert_eq!(format_bytes(999_999), "1.0 MB");
    assert_eq!(format_bytes(999_999_999), "1.0 GB");
    assert_eq!(format_bytes(999_999_999_999), "1000 GB");
}

#[test]
fn over_limit_falls_back_to_exact_bytes_when_short_strings_match() {
    let limit: u64 = 8 * 1024 * 1024;
    let (size, limit_text) = format_over_limit(limit + 1, limit);
    assert_eq!(size, "8,388,609 bytes");
    assert_eq!(limit_text, "8,388,608-byte");
    let (size, limit_text) = format_over_limit(2 * limit, limit);
    assert_eq!(size, "17 MB");
    assert_eq!(limit_text, "8.4 MB");
    let (size, _) = format_over_limit(999, 999);
    assert_eq!(size, "999 bytes", "no comma under a thousand");
    let (size, _) = format_over_limit(1_000_000, 1_000_000);
    assert_eq!(size, "1,000,000 bytes");
}

#[test]
fn labels_only_exact_line_starts() {
    // "a\nb\nc": LF lines start at 0, 2, 4.
    assert_eq!(line_number_label(0, &[0, 2, 4]), Some(1));
    assert_eq!(line_number_label(2, &[0, 2, 4]), Some(2));
    assert_eq!(line_number_label(4, &[0, 2, 4]), Some(3));
    assert_eq!(line_number_label(1, &[0, 2, 4]), None);
    assert_eq!(line_number_label(5, &[0, 2, 4]), None);
    assert_eq!(line_number_label(0, &[]), None);
}

#[test]
fn labels_follow_the_documents_line_starts() {
    let labels = |text: &str, offsets: &[usize]| -> Vec<Option<usize>> {
        let starts = document(text).line_starts;
        offsets
            .iter()
            .map(|offset| line_number_label(*offset, &starts))
            .collect()
    };
    // CRLF: the second row starts after the LF, at 3.
    assert_eq!(labels("a\r\nb", &[0, 3]), [Some(1), Some(2)]);
    // CR only: a layout would start rows at 2 and 4; neither is a line start.
    assert_eq!(labels("a\rb\rc", &[0, 2, 4]), [Some(1), None, None]);
    // U+2028 mid-line: the row after it (byte 4) is a continuation.
    assert_eq!(labels("a\u{2028}b", &[0, 4]), [Some(1), None]);
    // FF before LF: the empty row at 2 is a continuation, the LF line starts at 3.
    assert_eq!(labels("a\u{0C}\nb", &[0, 2, 3]), [Some(1), None, Some(2)]);
}

#[test]
fn load_anyway_stops_at_the_hard_limit() {
    assert!(can_force_load(HARD_LIMIT - 1));
    assert!(can_force_load(HARD_LIMIT));
    assert!(!can_force_load(HARD_LIMIT + 1));
    assert!(!can_force_load(u64::MAX));
}

#[test]
fn the_widest_line_counts_characters_not_bytes_or_line_breaks() {
    assert_eq!(document("").widest_line(), None);
    assert_eq!(document("one").widest_line(), Some(0));
    assert_eq!(document("ab\nabcd\nabc").widest_line(), Some(1));
    assert_eq!(
        document("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\nabcdef\nab").widest_line(),
        Some(1),
        "six one-byte characters outrun five that take ten bytes"
    );
    assert_eq!(
        document("abcd\r\nabcde").widest_line(),
        Some(1),
        "a CRLF is a line break, not width"
    );
    assert_eq!(
        document("abc\nxyz\nab").widest_line(),
        Some(0),
        "the first of equals"
    );
}
