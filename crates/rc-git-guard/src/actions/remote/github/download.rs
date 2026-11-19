//! The one address rc-git sends a request to that is not the signed-in account's API host: where
//! GitHub redirects a job log's download ([`super::logs`]). GitHub names it in a `Location`
//! header, so it is the host's word, and a hostile or compromised API host could name any address
//! at all; it is also a secret, since it carries a short-lived signature that lets anyone holding
//! it read the log.
//!
//! [`LogDownloadUrl::parse`] is the only way to make one, and it refuses anything but a plain
//! `https://` URL to a public-looking DNS name: no userinfo, no IP literal (in any of the forms
//! a resolver reads as an address), no `localhost` or other name that only means something inside
//! a network, no control character, space, backslash or byte outside printable ASCII, and at most
//! [`MAX_DOWNLOAD_URL_BYTES`]. Plain `http://` reaches a loopback host only under the dev
//! allowance ([`PLAIN_HTTP`](super::host::PLAIN_HTTP)), as every other request does. A DNS name
//! can still resolve to an address inside the user's network; the request then needs that address
//! to complete a TLS handshake with a certificate valid for the name before a byte of it is sent.
//!
//! The URL never appears in a message: its `Debug` names the host only, every error the transfer
//! produces names [`LogDownloadUrl::shown`], and the URL's parts are scrubbed from curl's own
//! words ([`LogDownloadUrl::secrets`]).

use std::fmt;

use super::host::{PlainHttp, check_name, parse_port};
use super::text::single_line;

/// The longest download address rc-git follows, in bytes.
pub(super) const MAX_DOWNLOAD_URL_BYTES: usize = 4096;

/// The shortest part of the address scrubbed from messages on its own: shorter values (a `1`, a
/// `true`) would scrub ordinary words.
const MIN_SCRUBBED_BYTES: usize = 8;

/// Names that never denote a public host: the loopback name, and the suffixes reserved or
/// commonly used for names inside a network (RFC 6761, RFC 6762 appendix G, RFC 8375, ICANN's
/// `.internal`).
const INTERNAL_SUFFIXES: [&str; 9] = [
    "localhost",
    "local",
    "internal",
    "intranet",
    "private",
    "corp",
    "home",
    "lan",
    "home.arpa",
];

/// Why a download address was refused. The address is never kept: only its host, when one could
/// be read, made single-line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DownloadRefused {
    /// The host the address names, single-line; `None` when none could be read.
    pub host: Option<String>,
    /// What is wrong with it.
    pub reason: &'static str,
}

/// A job log's download address, validated. Built only by [`LogDownloadUrl::parse`]; its text
/// leaves only through [`LogDownloadUrl::as_str`], which only the transport reads.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct LogDownloadUrl {
    url: String,
    /// `name` or `name:port`, lowercase.
    host: String,
    /// Where the path starts in `url`.
    path_start: usize,
}

impl fmt::Debug for LogDownloadUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogDownloadUrl")
            .field("host", &self.host)
            .field("url", &"<redacted>")
            .field("path_start", &self.path_start)
            .finish()
    }
}

impl LogDownloadUrl {
    /// `location` as a download address under `plain`, or why not.
    ///
    /// # Errors
    /// [`DownloadRefused`] naming the host when one could be read.
    pub(super) fn parse(location: &str, plain: PlainHttp) -> Result<Self, DownloadRefused> {
        let refuse = |host: Option<&str>, reason| DownloadRefused {
            host: host.map(|host| single_line(host, 100)),
            reason,
        };
        if location.len() > MAX_DOWNLOAD_URL_BYTES {
            return Err(refuse(None, "it is longer than 4096 bytes"));
        }
        if !location.bytes().all(|b| b.is_ascii_graphic() && b != b'\\') {
            return Err(refuse(
                None,
                "it holds a space, a control character, a backslash or a byte outside printable \
                 ASCII",
            ));
        }
        let (scheme, rest) = if let Some(rest) = location.strip_prefix("https://") {
            ("https", rest)
        } else if let Some(rest) = location.strip_prefix("http://") {
            ("http", rest)
        } else {
            return Err(refuse(None, "it is not an https:// address"));
        };
        let end = rest.find('/').unwrap_or(rest.len());
        let authority = &rest[..end];
        if authority.contains(['@', '?', '#', '[', ']']) {
            return Err(refuse(
                None,
                "it names a user, a password or a bracketed address",
            ));
        }
        if end == rest.len() {
            return Err(refuse(Some(authority), "it names no path"));
        }
        if rest.contains('#') {
            return Err(refuse(Some(authority), "it carries a fragment"));
        }
        let host = authority.to_ascii_lowercase();
        let (name, port) = match host.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host.as_str(), None),
        };
        if port.is_some_and(|port| parse_port(port).is_none()) {
            return Err(refuse(Some(authority), "its port is not 1 to 65535"));
        }
        let loopback = matches!(name, "127.0.0.1" | "localhost");
        let allowed = match (scheme, plain) {
            (_, PlainHttp::Loopback) if loopback => Ok(()),
            ("https", _) => public_name(name),
            (_, PlainHttp::Loopback | PlainHttp::Refused) => Err("it is not an https:// address"),
        };
        allowed.map_err(|reason| refuse(Some(authority), reason))?;
        Ok(Self {
            path_start: scheme.len() + "://".len() + end,
            url: location.to_owned(),
            host,
        })
    }

    /// The address, for the transport alone.
    pub(super) fn as_str(&self) -> &str {
        &self.url
    }

    /// `name` or `name:port`, lowercase.
    pub(super) fn host(&self) -> &str {
        &self.host
    }

    /// What a message names instead of the address: `https://<host>/…`.
    pub(super) fn shown(&self) -> String {
        let scheme = if self.url.starts_with("http://") {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}/…", self.host)
    }

    /// The parts of the address a message must never carry: the whole address, its path and
    /// query, the query alone, and each query value of [`MIN_SCRUBBED_BYTES`] or more.
    pub(super) fn secrets(&self) -> Vec<&str> {
        let path = &self.url[self.path_start..];
        let mut secrets = vec![self.url.as_str(), path];
        if let Some((_, query)) = path.split_once('?') {
            secrets.push(query);
            for pair in query.split('&') {
                let value = pair.split_once('=').map_or(pair, |(_, value)| value);
                if value.len() >= MIN_SCRUBBED_BYTES {
                    secrets.push(value);
                }
            }
        }
        secrets
    }
}

/// Whether `name` (lowercase) may be a download host outside the dev allowance: a DNS name of at
/// least two labels whose last label starts with a letter (so no form of an IP address reads as
/// one), and not a name that only means something inside a network.
fn public_name(name: &str) -> Result<(), &'static str> {
    check_name(name)?;
    let Some((_, last)) = name.rsplit_once('.') else {
        return Err("it names a single-label host, which only resolves inside a network");
    };
    if !last.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err("it names an IP address, not a DNS name");
    }
    let internal = INTERNAL_SUFFIXES.iter().any(|suffix| {
        name == *suffix
            || name
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.'))
    });
    if internal {
        return Err("it names a host that only resolves inside a network");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNED: &str = "https://results.blob.example.net/logs/job-1.txt?sv=2023&sig=Zm9vYmFyYmF6";

    #[test]
    fn a_plain_https_address_on_a_public_name_is_followed() {
        for plain in [PlainHttp::Refused, PlainHttp::Loopback] {
            let url = LogDownloadUrl::parse(SIGNED, plain).unwrap();
            assert_eq!(url.as_str(), SIGNED);
            assert_eq!(url.host(), "results.blob.example.net");
            assert_eq!(url.shown(), "https://results.blob.example.net/…");
        }
        let url = LogDownloadUrl::parse("https://Logs.Example.COM:8443/x", PlainHttp::Refused);
        assert_eq!(url.unwrap().host(), "logs.example.com:8443");
    }

    /// Every shape a hostile API host could name, refused whatever the dev allowance says.
    #[test]
    fn every_refused_shape_is_refused_with_and_without_the_dev_allowance() {
        let long = format!(
            "https://logs.example.com/{}",
            "a".repeat(MAX_DOWNLOAD_URL_BYTES)
        );
        for (location, why) in [
            ("http://logs.example.com/x", "not an https://"),
            ("ftp://logs.example.com/x", "not an https://"),
            ("file:///etc/passwd", "not an https://"),
            ("//logs.example.com/x", "not an https://"),
            ("HTTPS://logs.example.com/x", "not an https://"),
            ("https://user:pw@logs.example.com/x", "a user, a password"),
            (
                "https://logs.example.com@evil.example/x",
                "a user, a password",
            ),
            ("https://[::1]/x", "bracketed address"),
            ("https://10.0.0.1/x", "an IP address"),
            ("https://192.168.1.1:8080/x", "an IP address"),
            ("https://169.254.169.254/latest", "an IP address"),
            ("https://2130706433/x", "single-label"),
            ("https://0x7f.1/x", "an IP address"),
            ("https://127.1/x", "an IP address"),
            ("https://intranet-host/x", "single-label"),
            (
                "https://localhost.localdomain.localhost/x",
                "inside a network",
            ),
            ("https://printer.local/x", "inside a network"),
            ("https://db.corp/x", "inside a network"),
            ("https://router.home.arpa/x", "inside a network"),
            ("https://logs.example.com/a b", "a space, a control"),
            (
                "https://logs.example.com/a\r\nX-Evil: 1",
                "a space, a control",
            ),
            ("https://logs.example.com/a\tb", "a space, a control"),
            ("https://logs.example.com/a\u{0}b", "a space, a control"),
            ("https://logs.example.com\\@evil.example/x", "a backslash"),
            ("https://logs.exämple.com/x", "outside printable ASCII"),
            ("https://logs.example.com", "no path"),
            (
                "https://logs.example.com?x=1",
                "a user, a password or a bracketed",
            ),
            ("https://logs.example.com/x#frag", "a fragment"),
            ("https://logs.example.com:0/x", "port"),
            ("https://logs.example.com:65536/x", "port"),
            ("https://-bad.example.com/x", "label"),
            ("https://logs..example.com/x", "empty label"),
            (long.as_str(), "longer than 4096"),
        ] {
            for plain in [PlainHttp::Refused, PlainHttp::Loopback] {
                let refused = LogDownloadUrl::parse(location, plain).unwrap_err();
                assert!(refused.reason.contains(why), "{location:?}: {refused:?}");
            }
        }
    }

    /// What only the dev allowance admits, so that a shipped build never does.
    #[test]
    fn loopback_is_reached_only_under_the_dev_allowance() {
        for location in [
            "http://127.0.0.1:4010/x",
            "http://localhost:4010/x",
            "https://127.0.0.1/x",
            "https://localhost/x",
            "http://localhost/x",
        ] {
            assert!(
                LogDownloadUrl::parse(location, PlainHttp::Loopback).is_ok(),
                "{location}"
            );
            assert!(
                LogDownloadUrl::parse(location, PlainHttp::Refused).is_err(),
                "{location}"
            );
        }
        for location in [
            "http://127.0.0.2/x",
            "http://10.0.0.1/x",
            "http://example.com/x",
        ] {
            assert!(
                LogDownloadUrl::parse(location, PlainHttp::Loopback).is_err(),
                "{location}"
            );
        }
    }

    #[test]
    fn a_refusal_and_the_debug_name_the_host_only() {
        let url = LogDownloadUrl::parse(SIGNED, PlainHttp::Refused).unwrap();
        let printed = format!("{url:?} {url:#?}");
        assert!(printed.contains("results.blob.example.net"), "{printed}");
        for secret in ["Zm9vYmFyYmF6", "sig=", "/logs/job-1.txt"] {
            assert!(!printed.contains(secret), "{printed}");
        }
        let refused =
            LogDownloadUrl::parse("https://10.0.0.1/x?sig=Zm9vYmFyYmF6", PlainHttp::Refused)
                .unwrap_err();
        assert_eq!(refused.host.as_deref(), Some("10.0.0.1"));
        assert!(!format!("{refused:?}").contains("Zm9vYmFyYmF6"));
    }

    #[test]
    fn the_secrets_cover_the_address_its_path_and_its_long_query_values() {
        let url = LogDownloadUrl::parse(SIGNED, PlainHttp::Refused).unwrap();
        assert_eq!(
            url.secrets(),
            [
                SIGNED,
                "/logs/job-1.txt?sv=2023&sig=Zm9vYmFyYmF6",
                "sv=2023&sig=Zm9vYmFyYmF6",
                "Zm9vYmFyYmF6",
            ]
        );
    }
}
