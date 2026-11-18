//! Text that crosses the trust boundary: what the server says before it reaches an error or a
//! screen, and what rc-git encodes before it reaches a request body.

/// The longest server-provided text an error message carries.
pub(super) const MESSAGE_CHARS: usize = 300;

/// `text` made safe to show on one line: ANSI escape sequences are dropped, and every other
/// control character (line breaks and tabs included) and every bidirectional control
/// (U+200E, U+200F, U+202A–U+202E, U+2066–U+2069) is shown as a visible `\u{..}` escape
/// rather than interpreted. Longer than `max_chars` characters, it is cut there and ends in
/// `…`.
#[must_use]
pub fn single_line(text: &str, max_chars: usize) -> String {
    let mut shown = String::new();
    let mut count = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape_sequence(&mut chars);
            continue;
        }
        if count == max_chars {
            shown.push('…');
            break;
        }
        count += 1;
        if c.is_control() || is_bidi_control(c) {
            shown.push_str("\\u{");
            shown.push_str(&hex(u32::from(c)));
            shown.push('}');
        } else {
            shown.push(c);
        }
    }
    shown
}

/// `value` in lowercase hexadecimal, without leading zeros.
fn hex(value: u32) -> String {
    format!("{value:x}")
}

/// Drops the rest of an escape sequence whose `ESC` was just read: a CSI (`ESC [`) up to its
/// final byte, an OSC (`ESC ]`) up to its BEL or `ESC \`, or the one character after `ESC`.
fn skip_escape_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        Some('[') => {
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        }
        Some(_) | None => {}
    }
}

/// Whether `c` reorders the text around it when shown: the bidirectional marks, embeddings,
/// overrides and isolates.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// `text` with every occurrence of each non-empty secret replaced by `<redacted>`: the last
/// line of defense for a server or library message that echoes what it was sent.
pub(super) fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut scrubbed = text.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            scrubbed = scrubbed.replace(secret, "<redacted>");
        }
    }
    scrubbed
}

/// Server text for an error message: scrubbed of `secrets`, then made single-line.
pub(super) fn server_text(text: &str, secrets: &[&str]) -> String {
    single_line(&scrub(text, secrets), MESSAGE_CHARS)
}

/// `pairs` as an `application/x-www-form-urlencoded` body.
pub(super) fn form_body(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut body = String::new();
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        form_encode_into(&mut body, name);
        body.push('=');
        form_encode_into(&mut body, value);
    }
    body.into_bytes()
}

fn form_encode_into(out: &mut String, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => {
                const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(char::from(DIGITS[usize::from(other >> 4)]));
                out.push(char::from(DIGITS[usize::from(other & 0x0f)]));
            }
        }
    }
}

/// Whether `name` may be one path segment of an API URL or a GraphQL owner or repository
/// variable: `[A-Za-z0-9._-]+`, and not `.` or `..`.
pub(super) fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_shows_controls_and_bidi_as_escapes_and_drops_ansi() {
        assert_eq!(single_line("a\nb\tc", 100), "a\\u{a}b\\u{9}c");
        assert_eq!(
            single_line("evil\u{202e}txt.exe\u{2066}x\u{200f}", 100),
            "evil\\u{202e}txt.exe\\u{2066}x\\u{200f}"
        );
        assert_eq!(single_line("\u{1b}[31mred\u{1b}[0m!", 100), "red!");
        assert_eq!(single_line("\u{1b}]8;;https://x\u{7}link", 100), "link");
        assert_eq!(single_line("abcdef", 3), "abc…");
        assert_eq!(single_line("abc", 3), "abc");
    }

    #[test]
    fn scrub_removes_every_copy_of_every_secret() {
        assert_eq!(
            scrub("Bearer s3cr3t and s3cr3t, ghr_x", &["s3cr3t", "", "ghr_x"]),
            "Bearer <redacted> and <redacted>, <redacted>"
        );
    }

    #[test]
    fn form_body_encodes_reserved_bytes() {
        let body = form_body(&[("grant_type", "urn:x:y"), ("a b", "c&d=e+é")]);
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "grant_type=urn%3Ax%3Ay&a+b=c%26d%3De%2B%C3%A9"
        );
    }

    #[test]
    fn plain_names_refuse_dots_alone_slashes_and_encoding() {
        for good in ["rc-git", "a.b", "A_9", ".github", "x.."] {
            assert!(is_plain_name(good), "{good}");
        }
        for bad in ["", ".", "..", "a/b", "a%2fb", "a b", "é", "a\n"] {
            assert!(!is_plain_name(bad), "{bad:?}");
        }
    }
}
