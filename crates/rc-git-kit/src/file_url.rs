//! The path a `file://` URL names, for the system's open callback: Finder, the Dock and
//! `open -a` hand the app a folder as a URL, and every arrival then goes through one opener
//! that takes paths.

/// The path `url` names: a `file://` URL with an empty or `localhost` host, percent-decoded.
/// A plain path is returned as it is. `None` for any other scheme or host, a path that is not
/// absolute, or an escape that does not decode to UTF-8.
#[must_use]
pub fn path(url: &str) -> Option<String> {
    if url.starts_with('/') {
        return Some(url.to_owned());
    }
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let mut raw = rest.bytes();
    while let Some(byte) = raw.next() {
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let high = hex(raw.next()?)?;
        let low = hex(raw.next()?)?;
        bytes.push(high << 4 | low);
    }
    let mut decoded = String::from_utf8(bytes).ok()?;
    if decoded.len() > 1 && decoded.ends_with('/') {
        decoded.pop();
    }
    Some(decoded)
}

fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}
