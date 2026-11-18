//! Text that crosses the trust boundary: what the server says before it reaches an error or a
//! screen, and what rc-git encodes before it reaches a request body.

/// The longest server-provided text an error message carries.
pub(super) const MESSAGE_CHARS: usize = 300;

/// `text` made safe to show on one line: ANSI escape sequences are dropped, and every other
/// control character (line breaks and tabs included), every bidirectional control (Unicode's
/// whole `Bidi_Control` set: U+061C, U+200E, U+200F, U+202A–U+202E, U+2066–U+2069), the line
/// and paragraph separators (U+2028, U+2029) and the zero-width characters (U+200B–U+200D,
/// U+2060, U+FEFF) are shown as a visible `\u{..}` escape rather than interpreted. Longer
/// than `max_chars` characters, it is cut there and ends in `…`.
#[must_use]
pub fn single_line(text: &str, max_chars: usize) -> String {
    neutralize(text, max_chars, Breaks::Escaped)
}

/// `text` made safe to show as several lines, as a pull request's description: line breaks
/// are kept (`\r\n` and a lone `\r` become `\n`) and so are tabs, ANSI escape sequences are
/// dropped, and every other character [`single_line`] escapes is shown as the same visible
/// `\u{..}` escape. Longer than `max_chars` characters, it is cut there and ends in `…`.
#[must_use]
pub fn multi_line(text: &str, max_chars: usize) -> String {
    neutralize(text, max_chars, Breaks::Kept)
}

/// What [`neutralize`] does with a line break or a tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Breaks {
    Escaped,
    Kept,
}

fn neutralize(text: &str, max_chars: usize, breaks: Breaks) -> String {
    let mut shown = String::new();
    let mut count = 0;
    let mut chars = text.chars().peekable();
    while let Some(mut c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape_sequence(&mut chars);
            continue;
        }
        if count == max_chars {
            shown.push('…');
            break;
        }
        count += 1;
        if breaks == Breaks::Kept && c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            c = '\n';
        }
        let kept = breaks == Breaks::Kept && matches!(c, '\n' | '\t');
        if !kept && (c.is_control() || is_hidden_format(c)) {
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

/// Whether `c` changes how the text around it shows without showing itself: every
/// `Bidi_Control` code point (the Arabic letter mark, the bidirectional marks, embeddings,
/// overrides and isolates), the line and paragraph separators, and the zero-width space,
/// joiners, word joiner and byte-order mark.
fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
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

/// The longest user or organization name GitHub allows.
pub(super) const MAX_OWNER_CHARS: usize = 39;

/// The longest repository name GitHub allows.
pub(super) const MAX_REPOSITORY_CHARS: usize = 100;

/// Whether `owner` may name a repository's owner: a plain name of at most
/// [`MAX_OWNER_CHARS`] characters.
pub(super) fn is_owner_name(owner: &str) -> bool {
    owner.len() <= MAX_OWNER_CHARS && is_plain_name(owner)
}

/// Whether `name` may name a repository: a plain name of at most [`MAX_REPOSITORY_CHARS`]
/// characters.
pub(super) fn is_repository_name(name: &str) -> bool {
    name.len() <= MAX_REPOSITORY_CHARS && is_plain_name(name)
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
    fn single_line_shows_every_hidden_format_character_as_an_escape() {
        let hidden = [
            '\u{061c}', '\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{2028}',
            '\u{2029}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2060}',
            '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{feff}', '\u{85}', '\u{7f}',
        ];
        for c in hidden {
            let shown = single_line(&format!("a{c}b"), 100);
            assert_eq!(
                shown,
                format!("a\\u{{{:x}}}b", u32::from(c)),
                "{:x}",
                u32::from(c)
            );
        }
        for kept in ["é", "日本", "a b", "ﷺ"] {
            assert_eq!(single_line(kept, 100), kept, "{kept:?}");
        }
    }

    #[test]
    fn multi_line_keeps_breaks_and_tabs_and_escapes_the_rest() {
        assert_eq!(multi_line("a\r\nb\rc\n\td", 100), "a\nb\nc\n\td");
        assert_eq!(
            multi_line("x\u{202e}y\u{200b}z\u{2028}w\u{0}v\u{85}", 100),
            "x\\u{202e}y\\u{200b}z\\u{2028}w\\u{0}v\\u{85}"
        );
        assert_eq!(multi_line("\u{1b}[2J\u{1b}]0;title\u{7}ok", 100), "ok");
        assert_eq!(multi_line("ab\ncd", 3), "ab\n…");
    }

    #[test]
    fn owner_and_repository_names_are_capped_at_githubs_lengths() {
        assert!(is_owner_name(&"o".repeat(MAX_OWNER_CHARS)));
        assert!(!is_owner_name(&"o".repeat(MAX_OWNER_CHARS + 1)));
        assert!(is_repository_name(&"r".repeat(MAX_REPOSITORY_CHARS)));
        assert!(!is_repository_name(&"r".repeat(MAX_REPOSITORY_CHARS + 1)));
        assert!(!is_owner_name("..") && !is_repository_name("a/b"));
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
