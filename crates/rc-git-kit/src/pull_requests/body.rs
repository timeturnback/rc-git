//! Which links in a pull request's description may open. The description is
//! attacker-controlled markdown: it renders through the same parser as a repository's
//! markdown files, which never loads anything (an image is its alt text, linked to its source),
//! and a link opens only when the user clicks it, only for `http:` and `https:` with a plain
//! host and no userinfo, and with that host named before the click. Every other destination
//! (`mailto:`, `file:`, `javascript:`, an app's scheme, a relative path, a fragment) is plain
//! text.

/// A description link the user may open.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BodyLink {
    /// The URL as written.
    pub url: String,
    /// Its host, lowercase, with the port when it names one: what the tooltip names.
    pub host: String,
}

impl BodyLink {
    /// What hovering the link says before a click opens it.
    #[must_use]
    pub fn tooltip(&self) -> String {
        format!("Opens {} in your browser\n{}", self.host, self.url)
    }
}

/// The longest link kept.
const MAX_LINK_BYTES: usize = 2048;

/// A description as the document the markdown preview draws: lines start by the core's rule
/// (none for empty text, and a trailing newline starts no line).
#[must_use]
pub fn body_document(body: &str) -> crate::text_document::TextDocument {
    let bytes = body.as_bytes().to_vec();
    let mut starts = Vec::new();
    if !bytes.is_empty() {
        starts.push(0);
    }
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && index + 1 < bytes.len() {
            starts.push(index + 1);
        }
    }
    crate::text_document::TextDocument::new(bytes, &starts, true)
}

/// `destination` as a link the user may open, or `None` to leave it plain text.
#[must_use]
pub fn body_link(destination: &str) -> Option<BodyLink> {
    let url = destination.trim();
    if url.is_empty() || url.len() > MAX_LINK_BYTES || !url.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    let offset = if lower.starts_with("https://") {
        "https://".len()
    } else if lower.starts_with("http://") {
        "http://".len()
    } else {
        return None;
    };
    let rest = &url[offset..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].to_ascii_lowercase();
    let (name, port) = match authority.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (authority.as_str(), None),
    };
    let plain_name = !name.is_empty()
        && name.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    let plain_port = port.is_none_or(|port| {
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
    });
    (plain_name && plain_port).then(|| BodyLink {
        url: url.to_owned(),
        host: authority.clone(),
    })
}
