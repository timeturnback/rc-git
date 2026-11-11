//! Byte classes for the ASCII delimiters markdown uses.

/// An ASCII digit.
pub(super) fn is_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

/// An ASCII letter in either case.
pub(super) fn is_letter(byte: u8) -> bool {
    byte.is_ascii_alphabetic()
}

/// A space or a tab: the whitespace markdown trims inside a line.
pub(super) fn is_space(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// `CommonMark`'s scheme characters: an ASCII letter, then ASCII letters, digits, `+`, `.` or
/// `-`. Callers bound the length, since autolinks and link destinations differ there.
pub(super) fn is_scheme_shaped(scheme: &[u8]) -> bool {
    let Some((&first, rest)) = scheme.split_first() else {
        return false;
    };
    if !is_letter(first) {
        return false;
    }
    for &byte in rest {
        if !(is_letter(byte) || is_digit(byte) || byte == b'+' || byte == b'.' || byte == b'-') {
            return false;
        }
    }
    true
}

/// ASCII punctuation: what a backslash may escape.
pub(super) fn is_escapable(byte: u8) -> bool {
    byte.is_ascii_punctuation()
}
