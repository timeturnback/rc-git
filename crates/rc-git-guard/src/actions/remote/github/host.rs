//! Where a token may go. A [`GitHubHost`] is a host the user typed in Settings, parsed once
//! into a lowercase name and an optional port; every URL a call sends to is built from it, so
//! the API base is the account's and never a repository's. A repository's remote URL is
//! untrusted (the repository controls it): it selects an owner and a name only, and only when
//! its host is the account's host exactly ([`RepositoryTarget::on_host`]).

use std::fmt;

use super::text::{is_plain_name, single_line};
use super::{GitHubError, GitHubResult};

/// The public GitHub host.
const GITHUB_COM: &str = "github.com";

/// The REST and GraphQL API host of github.com.
const GITHUB_COM_API: &str = "api.github.com";

/// The port an `https://` URL uses when it names none.
const HTTPS_PORT: u16 = 443;

/// The port an `ssh://` URL uses when it names none.
const SSH_PORT: u16 = 22;

/// Whether plain `http://` may reach a loopback host. Only the guard's own unit tests and the
/// dev-only `dev-loopback-http` feature (enabled from `[dev-dependencies]` alone) allow it;
/// every shipped build speaks `https://` to every host, loopback included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlainHttp {
    /// Every URL is `https://`.
    Refused,
    /// A loopback host is reached over `http://`, so tests can run a local fake server.
    Loopback,
}

/// The one place the build decides [`PlainHttp`].
pub(super) const PLAIN_HTTP: PlainHttp = if cfg!(any(test, feature = "dev-loopback-http")) {
    PlainHttp::Loopback
} else {
    PlainHttp::Refused
};

/// A GitHub host the user signs into: `github.com` or a GitHub Enterprise Server host, as a
/// lowercase DNS name (or IPv4 address) and a port other than 443 when it has one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitHubHost {
    name: String,
    port: Option<u16>,
}

impl GitHubHost {
    /// `github.com`.
    #[must_use]
    pub fn github_com() -> Self {
        Self {
            name: GITHUB_COM.to_owned(),
            port: None,
        }
    }

    /// The host typed in `input`: `name` or `name:port`, surrounding whitespace trimmed, case
    /// folded to lowercase. Port 443 is the same host as no port. A scheme, path, userinfo,
    /// bracketed address, empty or dash-edged label, trailing dot, or `github.com` with a port
    /// is refused, as is `api.github.com` (the API host is derived from `github.com`).
    ///
    /// # Errors
    /// [`GitHubError::InvalidHost`] naming the input and what is wrong with it.
    pub fn parse(input: &str) -> GitHubResult<Self> {
        let text = input.trim();
        let invalid = |reason| GitHubError::InvalidHost {
            input: single_line(text, 80),
            reason,
        };
        if text.is_empty() {
            return Err(invalid("it is empty"));
        }
        if !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
        {
            return Err(invalid(
                "enter only the host name and an optional :port, with no scheme, path or user",
            ));
        }
        let lower = text.to_ascii_lowercase();
        let (name, port) = match lower.rsplit_once(':') {
            Some((name, port)) => (
                name,
                Some(parse_port(port).ok_or_else(|| invalid("the port is not 1 to 65535"))?),
            ),
            None => (lower.as_str(), None),
        };
        check_name(name).map_err(invalid)?;
        let port = port.filter(|port| *port != HTTPS_PORT);
        if name == GITHUB_COM_API {
            return Err(invalid(
                "enter github.com; rc-git derives the API host from it",
            ));
        }
        if name == GITHUB_COM && port.is_some() {
            return Err(invalid("github.com takes no port"));
        }
        Ok(Self {
            name: name.to_owned(),
            port,
        })
    }

    /// The host name, lowercase.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The port, when it is not 443.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Whether this is `github.com`, whose API lives on `api.github.com`.
    #[must_use]
    pub fn is_github_com(&self) -> bool {
        self.name == GITHUB_COM && self.port.is_none()
    }

    /// The only page rc-git opens for a device flow: `https://<host>/login/device`.
    #[must_use]
    pub fn verification_url(&self) -> String {
        format!("{}/login/device", self.origin(PLAIN_HTTP))
    }

    fn is_loopback(&self) -> bool {
        matches!(self.name.as_str(), "127.0.0.1" | "localhost")
    }

    /// `scheme://host[:port]` of the web host, under the given plain-http allowance.
    pub(super) fn origin(&self, plain: PlainHttp) -> String {
        let scheme = match plain {
            PlainHttp::Loopback if self.is_loopback() => "http",
            PlainHttp::Loopback | PlainHttp::Refused => "https",
        };
        format!("{scheme}://{self}")
    }

    /// A page of the web host, e.g. `/login/device/code`.
    pub(super) fn web_url(&self, path: &'static str) -> ApiUrl {
        ApiUrl(format!("{}{path}", self.origin(PLAIN_HTTP)))
    }

    /// The REST API base: `https://api.github.com` for github.com, `https://<host>/api/v3` for
    /// an Enterprise host.
    fn rest_base(&self) -> String {
        if self.is_github_com() {
            return format!("https://{GITHUB_COM_API}");
        }
        format!("{}/api/v3", self.origin(PLAIN_HTTP))
    }

    /// A REST endpoint: the API base followed by `segments`, each a plain name, and the
    /// already-encoded `query` (empty for none).
    ///
    /// # Errors
    /// [`GitHubError::InvalidValue`] when a segment is not a plain name.
    pub(super) fn rest_url(&self, segments: &[&str], query: &str) -> GitHubResult<ApiUrl> {
        let mut url = self.rest_base();
        for segment in segments {
            if !is_plain_name(segment) {
                return Err(GitHubError::InvalidValue {
                    field: "API path segment",
                    value: single_line(segment, 80),
                    reason: "a path segment is letters, digits, '.', '_' or '-', and not . or ..",
                });
            }
            url.push('/');
            url.push_str(segment);
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }
        Ok(ApiUrl(url))
    }

    /// The GraphQL endpoint: `https://api.github.com/graphql` for github.com,
    /// `https://<host>/api/graphql` for an Enterprise host.
    pub(super) fn graphql_url(&self) -> ApiUrl {
        if self.is_github_com() {
            return ApiUrl(format!("https://{GITHUB_COM_API}/graphql"));
        }
        ApiUrl(format!("{}/api/graphql", self.origin(PLAIN_HTTP)))
    }
}

impl fmt::Display for GitHubHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.port {
            Some(port) => write!(f, "{}:{port}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

fn parse_port(text: &str) -> Option<u16> {
    if text.is_empty() || text.len() > 5 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|port| *port != 0)
}

/// Checks a lowercase host name: dot-separated labels of 1 to 63 letters, digits and inner
/// dashes, 253 characters at most.
fn check_name(name: &str) -> Result<(), &'static str> {
    if name.len() > 253 {
        return Err("the host name is longer than 253 characters");
    }
    for label in name.split('.') {
        if label.is_empty() {
            return Err("the host name has an empty label (a leading, trailing or doubled dot)");
        }
        if label.len() > 63 {
            return Err("a label of the host name is longer than 63 characters");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("a label of the host name starts or ends with '-'");
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("the host name holds a character DNS names do not");
        }
    }
    Ok(())
}

/// A URL a call sends to, built only by [`GitHubHost`] from its own host, so no other text can
/// become a request URL. Holds no secret: tokens travel in headers, device codes and refresh
/// tokens in request bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApiUrl(String);

impl ApiUrl {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The host a repository's remote URL names, for looking up the account to use. The URL is
/// untrusted: this only parses it, and a token goes to the result only through an account
/// record for exactly that host.
///
/// # Errors
/// [`GitHubError::ForeignRemote`] when the URL is not an `https://` or ssh remote naming one
/// `owner/name` repository on a plain host, as for [`RepositoryTarget::on_host`].
pub fn remote_host(remote_url: &str) -> GitHubResult<GitHubHost> {
    Ok(parse_remote(remote_url)?.host)
}

/// One repository on one host, with owner and name validated as plain names: what a pull
/// request call is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryTarget {
    host: GitHubHost,
    owner: String,
    name: String,
}

impl RepositoryTarget {
    /// The repository `remote_url` names, when it lives on `host` exactly: the same lowercase
    /// name and, for `https://`, the same port (443 and none are one). The comparison is whole
    /// and exact, so `github.com.evil.example` and `evil.example/github.com` name other hosts.
    /// `https://` URLs carry no userinfo; ssh URLs carry no user but `git` and no port but 22;
    /// `git://`, `file://` and plain `http://` (outside the dev loopback allowance) are refused.
    /// The path must be `owner/name` or `owner/name.git`.
    ///
    /// # Errors
    /// [`GitHubError::ForeignRemote`] naming the remote (userinfo removed) and the reason.
    pub fn on_host(host: &GitHubHost, remote_url: &str) -> GitHubResult<Self> {
        let parsed = parse_remote(remote_url)?;
        if parsed.host != *host {
            return Err(GitHubError::ForeignRemote {
                remote: parsed.shown,
                reason: format!("is on '{}', not on '{host}'", parsed.host),
            });
        }
        Ok(Self {
            host: parsed.host,
            owner: parsed.owner,
            name: parsed.name,
        })
    }

    /// The host.
    #[must_use]
    pub fn host(&self) -> &GitHubHost {
        &self.host
    }

    /// The owning user or organization.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository's name, without `.git`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A remote URL taken apart.
struct ParsedRemote {
    host: GitHubHost,
    owner: String,
    name: String,
    /// The URL without userinfo, single-line, for messages.
    shown: String,
}

fn parse_remote(remote_url: &str) -> GitHubResult<ParsedRemote> {
    let url = gix::url::parse(gix::bstr::BStr::new(remote_url)).map_err(|_| {
        GitHubError::ForeignRemote {
            remote: single_line(&strip_userinfo_text(remote_url), 200),
            reason: "is not a URL".to_owned(),
        }
    })?;
    let shown = single_line(&super::super::redacted(&url), 200);
    let refuse = |reason: &str| GitHubError::ForeignRemote {
        remote: shown.clone(),
        reason: reason.to_owned(),
    };
    let port = match url.scheme {
        gix::url::Scheme::Https => {
            if url.user().is_some() || url.password().is_some() {
                return Err(refuse("carries a user name or password in its URL"));
            }
            url.port.filter(|port| *port != HTTPS_PORT)
        }
        gix::url::Scheme::Ssh => {
            if url.user().is_some_and(|user| user != "git") || url.password().is_some() {
                return Err(refuse("names an ssh user other than 'git'"));
            }
            if url.port.is_some_and(|port| port != SSH_PORT) {
                return Err(refuse("uses an ssh port other than 22"));
            }
            None
        }
        gix::url::Scheme::Http if PLAIN_HTTP == PlainHttp::Loopback => {
            if url.user().is_some() || url.password().is_some() {
                return Err(refuse("carries a user name or password in its URL"));
            }
            url.port
        }
        ref other => {
            return Err(refuse(&format!(
                "uses '{}'; only https:// and ssh remotes name a GitHub repository",
                other.as_str()
            )));
        }
    };
    let host_text = url.host().unwrap_or_default();
    let mut host = GitHubHost::parse(host_text).map_err(|_| refuse("names no plain host"))?;
    if host.port.is_some() {
        return Err(refuse("names a host with a port inside its host part"));
    }
    if url.scheme == gix::url::Scheme::Http && !host.is_loopback() {
        return Err(refuse(
            "uses 'http'; only https:// and ssh remotes name a GitHub repository",
        ));
    }
    host.port = port;
    let (owner, name) = owner_and_name(&url.path.to_string())
        .ok_or_else(|| refuse("does not name one owner/repository path"))?;
    Ok(ParsedRemote {
        host,
        owner,
        name,
        shown,
    })
}

/// `owner` and `name` from a remote path `owner/name` or `owner/name.git`, with or without the
/// leading `/`; `None` for any other shape or a part that is not a plain name.
fn owner_and_name(path: &str) -> Option<(String, String)> {
    let path = path.strip_prefix('/').unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    (is_plain_name(owner) && is_plain_name(name)).then(|| (owner.to_owned(), name.to_owned()))
}

/// `text` with anything between `://` and the last `@` before the next `/` removed: the
/// unparsable remote's userinfo, so even a refusal of garbage never echoes a password.
fn strip_userinfo_text(text: &str) -> String {
    let Some((scheme, rest)) = text.split_once("://") else {
        return text.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => text.to_owned(),
    }
}
