//! Host-key verification: the `known_hosts` files OpenSSH would consult are read, never
//! written. An unknown host is refused with its fingerprint so the user can approve it
//! (trust-on-first-use); the approval lives app-side and returns as
//! `RemoteAuth::accepted_hostkey`. A key that is `@revoked`, that contradicts every
//! `known_hosts` entry for the host, or that contradicts the fingerprint approved
//! earlier, is refused with no in-app override.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use russh::keys::HashAlg;
use russh::keys::PublicKeyOrCertificate;

use super::target::SshTarget;

/// Why `check_server_key` said no, kept for the caller to type the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HostKeyReport {
    /// The host has no `known_hosts` entry and no accepted fingerprint matched.
    Unknown {
        /// The offered key's algorithm, e.g. `ssh-ed25519`.
        key_type: String,
        /// The offered key's fingerprint, `SHA256:...`.
        fingerprint: String,
    },
    /// The offered key contradicts the host's `known_hosts` entry, or — when it has none —
    /// the fingerprint the user approved earlier.
    Mismatch {
        /// The offered key's algorithm.
        key_type: String,
        /// The offered key's fingerprint.
        fingerprint: String,
        /// The approved fingerprint it contradicts; `None` when `known_hosts` refused it.
        approved: Option<String>,
    },
    /// A `known_hosts` file has an `@revoked` line for the host naming the offered key.
    Revoked {
        /// The offered key's algorithm.
        key_type: String,
        /// The offered key's fingerprint.
        fingerprint: String,
    },
}

/// Shared slot the handler writes its refusal into; the sync caller reads it after
/// `connect` fails with `russh::Error::UnknownKey`.
pub(super) type ReportSlot = Arc<Mutex<Option<HostKeyReport>>>;

/// The russh client handler: host-key policy only, everything else default.
pub(super) struct HostKeyHandler {
    /// The `known_hosts` lookup key, as OpenSSH chooses it: ssh-config `HostKeyAlias`
    /// when set, else the name actually dialed (ssh-config `HostName`), so an alias URL
    /// still finds the real host's key.
    host_name: String,
    /// The dialed port, or 22 under a `HostKeyAlias`: OpenSSH looks the alias up bare,
    /// never as `[alias]:port`.
    port: u16,
    known_hosts: Vec<PathBuf>,
    accepted: Option<String>,
    report: ReportSlot,
}

impl HostKeyHandler {
    pub(super) fn new(target: &SshTarget, accepted: Option<&str>, report: ReportSlot) -> Self {
        let (host_name, port) = match &target.host_key_alias {
            Some(alias) => (alias.clone(), 22),
            None => (target.host_name.clone(), target.port),
        };
        Self {
            host_name,
            port,
            known_hosts: target.known_hosts.clone(),
            accepted: accepted.map(str::to_owned),
            report,
        }
    }

    fn record(&self, report: HostKeyReport) {
        *self.report.lock().unwrap_or_else(PoisonError::into_inner) = Some(report);
    }
}

impl russh::client::Handler for HostKeyHandler {
    type Error = russh::Error;

    // The trait's contract is async; this policy check is synchronous, so it returns a
    // ready future instead of carrying an unused `async`.
    fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        std::future::ready(self.check(server_key))
    }
}

impl HostKeyHandler {
    fn check(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, russh::Error> {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => {
                russh::keys::PublicKey::new(cert.public_key().clone(), "")
            }
        };
        let key_type = key.algorithm().to_string();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        // known_hosts first: a revoked key or a contradicting entry is fatal whatever the
        // app approved, and an approval only stands in for a missing entry.
        if self.is_revoked(&key)? {
            self.record(HostKeyReport::Revoked {
                key_type,
                fingerprint,
            });
            return Ok(false);
        }
        match self.recorded_verdict(&key)? {
            Verdict::Known => return Ok(true),
            Verdict::Changed => {
                self.record(HostKeyReport::Mismatch {
                    key_type,
                    fingerprint,
                    approved: None,
                });
                return Ok(false);
            }
            Verdict::Absent => {}
        }
        match self.accepted.clone() {
            Some(approved) if approved == fingerprint => Ok(true),
            Some(approved) => {
                self.record(HostKeyReport::Mismatch {
                    key_type,
                    fingerprint,
                    approved: Some(approved),
                });
                Ok(false)
            }
            None => {
                self.record(HostKeyReport::Unknown {
                    key_type,
                    fingerprint,
                });
                Ok(false)
            }
        }
    }
}

/// What the `known_hosts` files say about the offered key.
enum Verdict {
    /// Some file records exactly this key for the host.
    Known,
    /// No file records it, but one records another key of the same algorithm.
    Changed,
    /// Nothing recorded for the host in the offered key's algorithm.
    Absent,
}

impl HostKeyHandler {
    /// Matches OpenSSH across every file: any exact entry wins, even beside a stale line
    /// of the same algorithm (russh's own check fails on the first stale line). Hashed
    /// `|1|` entries are matched by russh, which also skips `@` marker lines.
    fn recorded_verdict(&self, key: &russh::keys::PublicKey) -> Result<Verdict, russh::Error> {
        let mut same_algorithm = false;
        for path in &self.known_hosts {
            let recorded =
                russh::keys::known_hosts::known_host_keys_path(&self.host_name, self.port, path)
                    .map_err(|source| unreadable(path, &source))?;
            for (_, recorded) in recorded {
                if recorded.key_data() == key.key_data() {
                    return Ok(Verdict::Known);
                }
                same_algorithm |= recorded.algorithm() == key.algorithm();
            }
        }
        Ok(if same_algorithm {
            Verdict::Changed
        } else {
            Verdict::Absent
        })
    }

    /// Whether any file has a `@revoked` line for this host (plain patterns only:
    /// wildcards, `?`, negation, case-insensitive) naming exactly this key. A file that
    /// does not exist is skipped; one that exists but cannot be read is an error, since
    /// its revocations cannot be ruled out.
    fn is_revoked(&self, key: &russh::keys::PublicKey) -> Result<bool, russh::Error> {
        let host = if self.port == 22 {
            self.host_name.clone()
        } else {
            format!("[{}]:{}", self.host_name, self.port)
        };
        for path in &self.known_hosts {
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(unreadable(path, &source)),
            };
            for line in std::io::BufReader::new(file).lines() {
                let line = line.map_err(|source| unreadable(path, &source))?;
                if revokes(&line, &host, key) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Whether one `known_hosts` line is `@revoked <patterns> <type> <base64>` covering `host`
/// and naming `key`. A revoked key that does not parse cannot equal the offered one.
fn revokes(line: &str, host: &str, key: &russh::keys::PublicKey) -> bool {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("@revoked") {
        return false;
    }
    let (Some(patterns), Some(_), Some(base64)) = (fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    hosts_match(patterns, host)
        && russh::keys::parse_public_key_base64(base64)
            .is_ok_and(|revoked| revoked.key_data() == key.key_data())
}

/// OpenSSH's host-pattern list: comma-separated globs, any `!pattern` match vetoes the
/// line, otherwise one positive match is needed. Case-insensitive.
fn hosts_match(patterns: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let mut matched = false;
    for pattern in patterns.split(',') {
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix('!') {
            Some(negated) if glob(negated.as_bytes(), host.as_bytes()) => return false,
            Some(_) => {}
            None => matched |= glob(pattern.as_bytes(), host.as_bytes()),
        }
    }
    matched
}

/// `*` matches any run of bytes, `?` exactly one, everything else itself.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| glob(rest, &text[skip..])),
        Some((&first, rest)) => text
            .split_first()
            .is_some_and(|(&byte, tail)| (first == b'?' || first == byte) && glob(rest, tail)),
    }
}

/// A `known_hosts` file that exists but could not be read or parsed, naming the file.
fn unreadable(path: &Path, source: &dyn std::fmt::Display) -> russh::Error {
    russh::Error::IO(std::io::Error::other(format!(
        "cannot read known_hosts file '{}': {source}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> russh::keys::PublicKey {
        let pair = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]);
        russh::keys::PrivateKey::from(pair).public_key().clone()
    }

    fn line(marker: &str, hosts: &str, key: &russh::keys::PublicKey) -> String {
        format!(
            "{marker}{hosts} {}\n",
            key.to_openssh().expect("encode key")
        )
    }

    fn handler(files: Vec<PathBuf>, accepted: Option<&str>) -> (HostKeyHandler, ReportSlot) {
        let report: ReportSlot = Arc::new(Mutex::new(None));
        let handler = HostKeyHandler {
            host_name: "example.com".to_owned(),
            port: 22,
            known_hosts: files,
            accepted: accepted.map(str::to_owned),
            report: Arc::clone(&report),
        };
        (handler, report)
    }

    fn check(handler: &mut HostKeyHandler, key: &russh::keys::PublicKey) -> bool {
        handler
            .check(&PublicKeyOrCertificate::PublicKey {
                key: key.clone(),
                hash_alg: None,
            })
            .expect("check")
    }

    fn reported(report: &ReportSlot) -> Option<HostKeyReport> {
        report.lock().expect("report").clone()
    }

    /// The first file has a stale same-algorithm line, the second the real key: OpenSSH
    /// accepts, and so must this check.
    #[test]
    fn a_key_only_in_the_second_file_is_known() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("first", line("", "example.com", &key(2)).as_bytes());
        dir.write("second", line("", "example.com", &key(1)).as_bytes());
        let files = vec![
            dir.path().join("missing"),
            dir.path().join("first"),
            dir.path().join("second"),
        ];
        let (mut handler, report) = handler(files, None);
        assert!(check(&mut handler, &key(1)));
        assert_eq!(reported(&report), None);
    }

    /// A same-algorithm entry in some file and the offered key in none is a mismatch.
    #[test]
    fn a_different_key_in_any_file_is_a_mismatch() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("second", line("", "example.com", &key(2)).as_bytes());
        let files = vec![dir.path().join("first"), dir.path().join("second")];
        let (mut handler, report) = handler(files, None);
        assert!(!check(&mut handler, &key(1)));
        assert!(matches!(
            reported(&report),
            Some(HostKeyReport::Mismatch { approved: None, .. })
        ));
    }

    /// No entry for the host anywhere is an unknown host.
    #[test]
    fn no_entry_in_any_file_is_unknown() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("first", line("", "other.example.com", &key(1)).as_bytes());
        let (mut handler, report) = handler(vec![dir.path().join("first")], None);
        assert!(!check(&mut handler, &key(1)));
        assert!(matches!(
            reported(&report),
            Some(HostKeyReport::Unknown { .. })
        ));
    }

    /// A `@revoked` line refuses the key before any accept path: a plain entry for the
    /// same key and an app-side approval of its fingerprint both lose.
    #[test]
    fn a_revoked_key_is_refused_even_when_recorded_and_approved() {
        let offered = key(1);
        let fingerprint = offered.fingerprint(HashAlg::Sha256).to_string();
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("known", line("", "example.com", &offered).as_bytes());
        dir.write(
            "revoked",
            line("@revoked ", "*.COM,!other.com", &offered).as_bytes(),
        );
        let files = vec![dir.path().join("known"), dir.path().join("revoked")];
        let (mut handler, report) = handler(files, Some(&fingerprint));
        assert!(!check(&mut handler, &offered));
        assert_eq!(
            reported(&report),
            Some(HostKeyReport::Revoked {
                key_type: offered.algorithm().to_string(),
                fingerprint,
            })
        );
    }

    /// A revocation of another key, or one whose patterns exclude the host, leaves the
    /// recorded key accepted.
    #[test]
    fn a_revocation_for_another_key_or_host_does_not_refuse() {
        let offered = key(1);
        let dir = rc_git_testkit::Fixture::empty_dir();
        let contents = [
            line("", "example.com", &offered),
            line("@revoked ", "*", &key(2)),
            line("@revoked ", "*.com,!example.com", &offered),
            line("@revoked ", "exam?le.org", &offered),
        ]
        .concat();
        dir.write("known", contents.as_bytes());
        let (mut handler, report) = handler(vec![dir.path().join("known")], None);
        assert!(check(&mut handler, &offered));
        assert_eq!(reported(&report), None);
    }

    #[test]
    fn host_patterns_follow_openssh_rules() {
        assert!(hosts_match("*", "[example.com]:2222"));
        assert!(hosts_match("EXAMPLE.com", "example.com"));
        assert!(hosts_match("a.org,exa?ple.*", "example.com"));
        assert!(!hosts_match("*.com,!example.com", "example.com"));
        assert!(!hosts_match("!other.com", "example.com"));
        assert!(!hosts_match("example.co", "example.com"));
    }
}
