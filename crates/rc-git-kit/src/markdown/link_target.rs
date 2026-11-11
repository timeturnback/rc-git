//! What a markdown link points at once it is read against the file showing it.

use super::ascii;
use super::text;

/// The schemes safe to hand to the system without asking.
const BROWSER_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// What a markdown link points at once it is read against the file showing it. The preview
/// resolves every link through [`MarkdownLinkTarget::resolve`], so clicking, copying, and the
/// context menu all agree on what a destination means.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MarkdownLinkTarget {
    /// A web or mail link: `http:`, `https:`, `mailto:`, or a scheme-relative `//host` link
    /// (given `https:`). Safe to hand to the system without asking. The URL is as written; the
    /// system opener validates and encodes it.
    Web(String),
    /// Any other scheme (`ssh:`, `smb:`, `vnc:`, an app's own scheme). The system hands it to
    /// whichever app claims the scheme, so the preview asks before opening it.
    OtherScheme(String),
    /// Another file in the repository, with the heading fragment the link asked for.
    RepositoryPath {
        /// The repository-relative path, flattened.
        path: String,
        /// The percent-decoded fragment, if a non-empty one was given.
        fragment: Option<String>,
    },
    /// A jump inside the file already on screen.
    Fragment(String),
}

impl MarkdownLinkTarget {
    /// Resolves `destination` as written in a file at `source_path`. Returns `None` for an
    /// empty destination, for a relative path that climbs above the repository root or lands
    /// on it, and for a `file:` URL. Only `http`, `https` and `mailto` resolve to `Web`; every
    /// other scheme resolves to `OtherScheme`.
    ///
    /// `source_path` is the repository-relative path of the file being shown; when it is `None`
    /// a relative destination is read as if the file sat at the repository root.
    #[must_use]
    pub fn resolve(destination: &str, source_path: Option<&str>) -> Option<Self> {
        let trimmed = destination.trim_matches(text::is_whitespace_or_newline);
        if trimmed.is_empty() {
            return None;
        }
        if let Some(fragment) = trimmed.strip_prefix('#') {
            return Some(Self::Fragment(decoded(fragment)));
        }
        if trimmed.starts_with("//") {
            return Some(Self::Web(format!("https:{trimmed}")));
        }
        if let Some(scheme) = scheme(trimmed) {
            if scheme == "file" {
                return None;
            }
            let url = trimmed.to_owned();
            if BROWSER_SCHEMES.contains(&scheme.as_str()) {
                return Some(Self::Web(url));
            }
            return Some(Self::OtherScheme(url));
        }
        repository_target(trimmed, source_path)
    }
}

fn repository_target(destination: &str, source_path: Option<&str>) -> Option<MarkdownLinkTarget> {
    let (before_fragment, fragment) = match destination.split_once('#') {
        Some((before, after)) => (before, (!after.is_empty()).then(|| decoded(after))),
        None => (destination, None),
    };
    let path_text = before_fragment.split('?').next().unwrap_or(before_fragment);
    let raw_path =
        text::removing_percent_encoding(path_text).unwrap_or_else(|| path_text.to_owned());
    if raw_path.is_empty() {
        return fragment.map(MarkdownLinkTarget::Fragment);
    }
    let base = if raw_path.starts_with('/') {
        Vec::new()
    } else {
        directory(source_path)
    };
    let path = flatten(base, &raw_path)?;
    if path.is_empty() {
        return None;
    }
    Some(MarkdownLinkTarget::RepositoryPath {
        path: path.join("/"),
        fragment,
    })
}

/// A fragment as a browser matches it: percent-decoded, so a copied GitHub heading link
/// (`#ti%E1%BA%BFng`) finds its heading. Text that does not decode (`#100%`) stays as written.
fn decoded(fragment: &str) -> String {
    text::removing_percent_encoding(fragment).unwrap_or_else(|| fragment.to_owned())
}

/// The lowercased scheme of an absolute URL, or `None` when the destination is a path. A scheme
/// follows `CommonMark`'s ASCII rule, so `x-apple.systempreferences:` is one and `é:` is not. A
/// bare `C:` style prefix is not a scheme here; a scheme needs at least two characters.
fn scheme(destination: &str) -> Option<String> {
    let colon = destination.find(':')?;
    let candidate = &destination[..colon];
    if candidate.len() < 2 || !ascii::is_scheme_shaped(candidate.as_bytes()) {
        return None;
    }
    if destination.find('/').is_some_and(|slash| slash < colon) {
        return None;
    }
    Some(candidate.to_ascii_lowercase())
}

/// The directory components of `path`, empty segments dropped.
fn directory(path: Option<&str>) -> Vec<&str> {
    let mut components = segments(path.unwrap_or(""));
    components.pop();
    components
}

fn segments(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for segment in path.split('/') {
        if !segment.is_empty() {
            out.push(segment);
        }
    }
    out
}

/// Appends `relative` to the `base` components and flattens `.`, `..` and empty segments.
/// `None` when the path climbs out of the repository; an empty result is the repository root.
fn flatten<'a>(base: Vec<&'a str>, relative: &'a str) -> Option<Vec<&'a str>> {
    let mut components = base;
    for component in segments(relative) {
        if component == "." {
            continue;
        }
        if component == ".." {
            components.pop()?;
        } else {
            components.push(component);
        }
    }
    Some(components)
}
