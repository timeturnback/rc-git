//! URLs the server sends inside an answer: a pull request's page, a check's details page. They
//! are untrusted text. One is kept only when it is a plain `https://` URL (plain `http://` to a
//! loopback host under the dev allowance) with a DNS host, no userinfo and only URL characters,
//! and it is typed by whether it is on the account's own host. rc-git never fetches one; the
//! front end opens it on a click, with its host shown.

use std::fmt;

use super::host::{GitHubHost, PLAIN_HTTP, PlainHttp, check_name, parse_port};

/// The longest server URL rc-git keeps.
const MAX_LINK_BYTES: usize = 2048;

/// A URL from the server, never fetched by rc-git.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum ServerLink {
    /// A page on the account's own host.
    OnHost(String),
    /// A page on another host, such as a CI provider's: shown with that host.
    Elsewhere {
        /// The URL.
        url: String,
        /// Its host, with the port when it names one.
        host: String,
    },
}

impl ServerLink {
    /// The URL.
    #[must_use]
    pub fn url(&self) -> &str {
        match self {
            Self::OnHost(url) | Self::Elsewhere { url, .. } => url,
        }
    }
}

impl fmt::Debug for ServerLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OnHost(url) => write!(f, "OnHost({url})"),
            Self::Elsewhere { url, .. } => write!(f, "Elsewhere({url})"),
        }
    }
}

/// `text` as a [`ServerLink`] against `account`, or `None` when it is not a URL rc-git keeps.
pub(super) fn server_link(text: &str, account: &GitHubHost) -> Option<ServerLink> {
    if text.len() > MAX_LINK_BYTES {
        return None;
    }
    let (rest, plain) = match text.strip_prefix("https://") {
        Some(rest) => (rest, false),
        None => (text.strip_prefix("http://")?, true),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let (name, port) = link_host(authority)?;
    if plain && !(PLAIN_HTTP == PlainHttp::Loopback && is_loopback_name(&name)) {
        return None;
    }
    if !path.bytes().all(is_url_byte) {
        return None;
    }
    let default_port = if plain { 80 } else { 443 };
    let port = port.filter(|port| *port != default_port);
    if name == account.name() && port == account.port() {
        return Some(ServerLink::OnHost(text.to_owned()));
    }
    let host = match port {
        Some(port) => format!("{name}:{port}"),
        None => name,
    };
    Some(ServerLink::Elsewhere {
        url: text.to_owned(),
        host,
    })
}

fn is_loopback_name(name: &str) -> bool {
    matches!(name, "127.0.0.1" | "localhost")
}

/// The lowercase name and the port of a URL's authority; `None` with userinfo, brackets, or a
/// name DNS does not allow.
fn link_host(authority: &str) -> Option<(String, Option<u16>)> {
    if authority.is_empty()
        || !authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
    {
        return None;
    }
    let lower = authority.to_ascii_lowercase();
    let (name, port) = match lower.split_once(':') {
        Some((name, port)) => (name.to_owned(), Some(parse_port(port)?)),
        None => (lower, None),
    };
    check_name(&name).ok()?;
    Some((name, port))
}

/// Whether `byte` may stand in a URL's path, query or fragment as rc-git keeps it: printable
/// ASCII other than space and the characters RFC 3986 never allows unescaped.
fn is_url_byte(byte: u8) -> bool {
    byte.is_ascii_graphic()
        && !matches!(
            byte,
            b'<' | b'>' | b'"' | b'{' | b'}' | b'|' | b'\\' | b'^' | b'`'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_on_the_account_host_is_typed_on_host() {
        let com = GitHubHost::github_com();
        for url in [
            "https://github.com/o/r/pull/1",
            "https://GitHub.com/o/r/actions/runs/1/job/2?pr=1#step:3:4",
            "https://github.com:443/o/r",
        ] {
            assert_eq!(
                server_link(url, &com),
                Some(ServerLink::OnHost(url.to_owned())),
                "{url}"
            );
        }
    }

    #[test]
    fn a_link_elsewhere_names_its_real_host() {
        let com = GitHubHost::github_com();
        for (url, host) in [
            ("https://ci.example.com/build/7", "ci.example.com"),
            (
                "https://github.com.evil.example/x",
                "github.com.evil.example",
            ),
            ("https://evil.example/github.com/x", "evil.example"),
            ("https://github.com:8443/x", "github.com:8443"),
            ("https://api.github.com/repos/o/r", "api.github.com"),
        ] {
            assert_eq!(
                server_link(url, &com),
                Some(ServerLink::Elsewhere {
                    url: url.to_owned(),
                    host: host.to_owned()
                }),
                "{url}"
            );
        }
    }

    #[test]
    fn a_link_rc_git_cannot_show_honestly_is_dropped() {
        let com = GitHubHost::github_com();
        let long = format!("https://github.com/{}", "a".repeat(MAX_LINK_BYTES));
        for url in [
            "http://github.com/o/r",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://github.com@evil.example/x",
            "https://user:pw@github.com/x",
            "https://[::1]/x",
            "https://github.com/a b",
            "https://github.com/a\u{202e}b",
            "https://github.com/a\nb",
            "https:///x",
            "https://gïthub.com/x",
            "//github.com/x",
            "",
            long.as_str(),
        ] {
            assert_eq!(server_link(url, &com), None, "{url:?}");
        }
    }
}
