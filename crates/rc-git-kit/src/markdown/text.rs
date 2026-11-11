//! The string semantics the Swift parser borrowed from Foundation and `Character`, rebuilt on
//! `char` so the port reads text the way the app did.

use unicode_normalization::char::is_combining_mark;

/// Lowercases one scalar at a time, as Swift's `String.lowercased()` does. `str::to_lowercase`
/// would turn a word-final `Σ` into `ς`, which Swift never does, so a heading anchor would
/// change.
pub(super) fn lowercased(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        out.extend(ch.to_lowercase());
    }
    out
}

/// Foundation's `CharacterSet.whitespaces`: tab plus the space separators (category `Zs`).
pub(super) fn is_whitespace(ch: char) -> bool {
    matches_space_separator(ch) || ch == '\t'
}

/// Foundation's `CharacterSet.whitespacesAndNewlines`: `whitespaces` plus the line breaks.
pub(super) fn is_whitespace_or_newline(ch: char) -> bool {
    is_whitespace(ch) || ('\n'..='\r').contains(&ch) || "\u{85}\u{2028}\u{2029}".contains(ch)
}

/// Unicode general category `Zs`.
fn matches_space_separator(ch: char) -> bool {
    "\u{20}\u{a0}\u{1680}\u{202f}\u{205f}\u{3000}".contains(ch)
        || ('\u{2000}'..='\u{200a}').contains(&ch)
}

/// Whether `ch` joins the character before it into one user-perceived character: a combining
/// mark, a zero-width joiner, or an emoji skin-tone modifier. It approximates Swift's
/// grapheme-cluster `Character` closely enough for the questions asked of it here: is the
/// first scalar a letter, a digit, whitespace.
fn extends_cluster(ch: char) -> bool {
    is_combining_mark(ch) || ch == '\u{200d}' || ('\u{1f3fb}'..='\u{1f3ff}').contains(&ch)
}

/// Splits `text` into the clusters Swift iterates as `Character`s (see `extends_cluster`).
pub(super) fn clusters(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut joined = false;
    for (offset, ch) in text.char_indices() {
        let extends =
            extends_cluster(ch) || joined || (ch == '\n' && text[..offset].ends_with('\r'));
        if offset > start && !extends {
            out.push(&text[start..offset]);
            start = offset;
        }
        joined = ch == '\u{200d}';
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Whether some character of `text` is not whitespace, judged by each cluster's first scalar
/// as Swift's `Character.isWhitespace` does.
pub(super) fn has_visible_text(text: &str) -> bool {
    for cluster in clusters(text) {
        if cluster.chars().next().is_some_and(|ch| !ch.is_whitespace()) {
            return true;
        }
    }
    false
}

/// Foundation's `removingPercentEncoding`: every `%` must start two hex digits and the decoded
/// bytes must be UTF-8, or the whole string fails to decode.
pub(super) fn removing_percent_encoding(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while let Some(&byte) = bytes.get(cursor) {
        if byte != b'%' {
            out.push(byte);
            cursor += 1;
            continue;
        }
        let high = hex_value(*bytes.get(cursor + 1)?)?;
        let low = hex_value(*bytes.get(cursor + 2)?)?;
        out.push(high << 4 | low);
        cursor += 3;
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    let digit = char::from(byte).to_digit(16)?;
    u8::try_from(digit).ok()
}
