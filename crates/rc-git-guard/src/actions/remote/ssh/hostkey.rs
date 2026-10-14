//! Host-key verification: the `known_hosts` files OpenSSH would consult are read, never
//! written. An unknown host is refused with its fingerprint so the user can approve it
//! (trust-on-first-use); the approval lives app-side and returns as
//! `RemoteAuth::accepted_hostkey`. A key that is `@revoked`, that contradicts every
//! `known_hosts` entry for the host, or that contradicts the fingerprint approved
//! earlier, is refused with no in-app override. A host trusted only through
//! `@cert-authority` is refused by name: host certificates are not verified here.

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
        match self.recorded_verdict(&key)? {
            Verdict::Revoked => {
                self.record(HostKeyReport::Revoked {
                    key_type,
                    fingerprint,
                });
                return Ok(false);
            }
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
    /// Some file has a `@revoked` line for the host naming exactly this key.
    Revoked,
    /// Some file records exactly this key for the host.
    Known,
    /// No file records it, but one records another key of the same algorithm.
    Changed,
    /// Nothing recorded for the host in the offered key's algorithm.
    Absent,
}

/// What a `known_hosts` line's leading marker makes of its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// No marker: the host's own key.
    Plain,
    /// `@revoked`: the key is refused for the hosts it covers.
    Revoked,
    /// `@cert-authority`: the key signs host certificates, which this check does not verify.
    CertAuthority,
}

/// One `known_hosts` line whose host field covers the looked-up host.
struct Entry {
    marker: Marker,
    key: russh::keys::PublicKey,
    path: PathBuf,
    line: usize,
}

impl HostKeyHandler {
    /// Matches OpenSSH across every file: a `@revoked` line for the key refuses it, any
    /// exact entry wins even beside a stale line of the same algorithm, and a host trusted
    /// only through `@cert-authority` is refused by name rather than offered for approval,
    /// since host certificates are not verified here.
    fn recorded_verdict(&self, key: &russh::keys::PublicKey) -> Result<Verdict, russh::Error> {
        let entries = self.matching_entries()?;
        let same_key = |entry: &&Entry| entry.key.key_data() == key.key_data();
        let plain = || entries.iter().filter(|entry| entry.marker == Marker::Plain);
        if entries
            .iter()
            .filter(|entry| entry.marker == Marker::Revoked)
            .any(|entry| same_key(&entry))
        {
            return Ok(Verdict::Revoked);
        }
        if plain().any(|entry| same_key(&entry)) {
            return Ok(Verdict::Known);
        }
        if plain().any(|entry| entry.key.algorithm() == key.algorithm()) {
            return Ok(Verdict::Changed);
        }
        match entries
            .iter()
            .find(|entry| entry.marker == Marker::CertAuthority)
        {
            Some(authority) => Err(cert_authority_refused(authority, &self.lookup_host())),
            None => Ok(Verdict::Absent),
        }
    }

    /// The host as `known_hosts` records it: bare on port 22, else `[host]:port`.
    fn lookup_host(&self) -> String {
        if self.port == 22 {
            self.host_name.clone()
        } else {
            format!("[{}]:{}", self.host_name, self.port)
        }
    }

    /// Every line in every file that covers the host, read the way OpenSSH reads them: a
    /// line that does not parse (unknown marker, missing field, bad key, not UTF-8) is
    /// skipped, never fatal to the rest of its file. A file that does not exist is skipped;
    /// one that exists but cannot be read is an error, since its revocations cannot be
    /// ruled out.
    fn matching_entries(&self) -> Result<Vec<Entry>, russh::Error> {
        let host = self.lookup_host();
        let mut entries = Vec::new();
        for path in &self.known_hosts {
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(unreadable(path, &source)),
            };
            for (index, bytes) in std::io::BufReader::new(file).split(b'\n').enumerate() {
                let bytes = bytes.map_err(|source| unreadable(path, &source))?;
                // OpenSSH skips a line it cannot parse; a non-UTF-8 line is one of those.
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    continue;
                };
                if let Some((marker, key)) = parse_line(text, &host) {
                    entries.push(Entry {
                        marker,
                        key,
                        path: path.clone(),
                        line: index + 1,
                    });
                }
            }
        }
        Ok(entries)
    }
}

/// One `known_hosts` line, `[@marker] <hosts> <type> <base64> [comment]`, when its host
/// field covers `host`. `None` for a comment, a blank or malformed line, an unknown marker,
/// a key that does not parse, or a line for other hosts.
fn parse_line(line: &str, host: &str) -> Option<(Marker, russh::keys::PublicKey)> {
    let mut fields = line.split_whitespace();
    let mut hosts = fields.next()?;
    let marker = match hosts {
        "@revoked" => Marker::Revoked,
        "@cert-authority" => Marker::CertAuthority,
        _ if hosts.starts_with('@') || hosts.starts_with('#') => return None,
        _ => Marker::Plain,
    };
    if marker != Marker::Plain {
        hosts = fields.next()?;
    }
    let (Some(_), Some(base64)) = (fields.next(), fields.next()) else {
        return None;
    };
    if !host_field_matches(hosts, host) {
        return None;
    }
    let key = russh::keys::parse_public_key_base64(base64).ok()?;
    Some((marker, key))
}

/// A host field is either one hashed `|1|<salt>|<hash>` entry or a pattern list.
fn host_field_matches(field: &str, host: &str) -> bool {
    match field.strip_prefix("|1|") {
        Some(hashed) => hashed_host_matches(hashed, host),
        None => hosts_match(field, host),
    }
}

/// `<salt>|<hash>` (both base64) matches when `hash` is HMAC-SHA1 of `host` keyed by `salt`,
/// as `ssh-keygen -H` writes it. A field that does not decode matches nothing.
fn hashed_host_matches(hashed: &str, host: &str) -> bool {
    use hmac::{KeyInit, Mac};
    use russh::keys::ssh_encoding::base64::{Base64, Encoding};

    let Some((salt, hash)) = hashed.split_once('|') else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (Base64::decode_vec(salt), Base64::decode_vec(hash)) else {
        return false;
    };
    let Ok(mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(&salt) else {
        return false;
    };
    mac.chain_update(host.as_bytes())
        .verify_slice(&hash)
        .is_ok()
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

/// The refusal for a host whose only `known_hosts` trust is a `@cert-authority` line,
/// naming the line and the way out.
fn cert_authority_refused(authority: &Entry, host: &str) -> russh::Error {
    russh::Error::IO(std::io::Error::other(format!(
        "known_hosts file '{}' line {} trusts host '{host}' through @cert-authority, which \
         rc-git does not verify; add the host's own key to known_hosts, or use git directly",
        authority.path.display(),
        authority.line
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

    /// `ssh-keygen -H` output for `example.com` and `[example.com]:2222`.
    const HASHED_EXAMPLE: &str = "|1|huNll4RARPBWz3kndxxsHNky/uE=|vcmrXZlSRiGG9f/g4z+6bYpY49k=";
    const HASHED_EXAMPLE_2222: &str =
        "|1|ONiHezoYvwBY0kOgDzQ1DiaIMgg=|V9VmIP0ljQFTtnE9i4okawH/qac=";

    /// A plain entry is a pattern list: a glob covers the host, a negation vetoes it.
    #[test]
    fn a_wildcard_entry_is_known_and_a_negated_one_is_not() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "glob",
            line("", "*.example.org,exam?le.COM", &key(1)).as_bytes(),
        );
        dir.write("negated", line("", "*,!example.com", &key(1)).as_bytes());
        let (mut glob, report) = handler(vec![dir.path().join("glob")], None);
        assert!(check(&mut glob, &key(1)));
        assert_eq!(reported(&report), None);
        let (mut negated, report) = handler(vec![dir.path().join("negated")], None);
        assert!(!check(&mut negated, &key(1)));
        assert!(matches!(
            reported(&report),
            Some(HostKeyReport::Unknown { .. })
        ));
    }

    /// A hashed `@revoked` line refuses the key a plain entry records.
    #[test]
    fn a_hashed_revoked_line_refuses_the_key() {
        let offered = key(1);
        let dir = rc_git_testkit::Fixture::empty_dir();
        let contents = [
            line("", "example.com", &offered),
            line("@revoked ", HASHED_EXAMPLE, &offered),
        ]
        .concat();
        dir.write("known", contents.as_bytes());
        let (mut handler, report) = handler(vec![dir.path().join("known")], None);
        assert!(!check(&mut handler, &offered));
        assert!(matches!(
            reported(&report),
            Some(HostKeyReport::Revoked { .. })
        ));
    }

    /// One bad line costs only itself: garbage, a bad key, an unknown marker and a
    /// non-UTF-8 line all sit before the real entry, which still counts.
    #[test]
    fn a_garbage_line_does_not_hide_the_entry_after_it() {
        let offered = key(1);
        let mut contents =
            b"garbage\nexample.com ssh-ed25519 !!!\n@bogus example.com x y\n\xff\xfe\n# c\n\n"
                .to_vec();
        contents.extend_from_slice(line("", "example.com", &offered).as_bytes());
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("known", &contents);
        let (mut handler, report) = handler(vec![dir.path().join("known")], None);
        assert!(check(&mut handler, &offered));
        assert_eq!(reported(&report), None);
    }

    /// A host trusted only through `@cert-authority` is refused by name, approval or not;
    /// beside a plain entry for the offered key, the plain entry decides.
    #[test]
    fn a_cert_authority_line_is_refused_unless_a_plain_entry_knows_the_key() {
        let offered = key(1);
        let fingerprint = offered.fingerprint(HashAlg::Sha256).to_string();
        let dir = rc_git_testkit::Fixture::empty_dir();
        let authority = line("@cert-authority ", "*.com", &key(2));
        dir.write("ca", format!("# trust\n{authority}").as_bytes());
        dir.write("plain", line("", "example.com", &offered).as_bytes());
        let ca = dir.path().join("ca");
        let (mut only_ca, report) = handler(vec![ca.clone()], Some(&fingerprint));
        let error = only_ca
            .check(&PublicKeyOrCertificate::PublicKey {
                key: offered.clone(),
                hash_alg: None,
            })
            .expect_err("a cert-authority host is refused")
            .to_string();
        assert!(error.contains("@cert-authority"), "{error}");
        assert!(
            error.contains(&format!("'{}' line 2", ca.display())),
            "{error}"
        );
        assert_eq!(reported(&report), None);
        let (mut with_plain, _) = handler(vec![ca, dir.path().join("plain")], None);
        assert!(check(&mut with_plain, &offered));
    }

    #[test]
    fn hashed_hosts_match_ssh_keygen_output() {
        assert!(host_field_matches(HASHED_EXAMPLE, "example.com"));
        assert!(!host_field_matches(HASHED_EXAMPLE, "example.org"));
        assert!(host_field_matches(
            HASHED_EXAMPLE_2222,
            "[example.com]:2222"
        ));
        assert!(!host_field_matches(HASHED_EXAMPLE_2222, "example.com"));
        assert!(!host_field_matches("|1|not base64|x", "example.com"));
        assert!(!host_field_matches("|1|", "example.com"));
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
