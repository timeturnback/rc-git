//! Host-key verification: `known_hosts` is read, never written. An unknown host is
//! refused with its fingerprint so the user can approve it (trust-on-first-use); the
//! approval lives app-side and returns as `RemoteAuth::accepted_hostkey`. A key that
//! contradicts `known_hosts`, or the fingerprint approved earlier, is refused with
//! no in-app override.

use std::path::PathBuf;
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
}

/// Shared slot the handler writes its refusal into; the sync caller reads it after
/// `connect` fails with `russh::Error::UnknownKey`.
pub(super) type ReportSlot = Arc<Mutex<Option<HostKeyReport>>>;

/// The russh client handler: host-key policy only, everything else default.
pub(super) struct HostKeyHandler {
    /// The name actually dialed (ssh-config `HostName`) — the `known_hosts` lookup key,
    /// exactly as OpenSSH records entries. An alias URL still finds the real host's key.
    host_name: String,
    port: u16,
    known_hosts: PathBuf,
    accepted: Option<String>,
    report: ReportSlot,
}

impl HostKeyHandler {
    pub(super) fn new(target: &SshTarget, accepted: Option<&str>, report: ReportSlot) -> Self {
        Self {
            host_name: target.host_name.clone(),
            port: target.port,
            known_hosts: target.known_hosts(),
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
        // known_hosts first: a contradicting entry is fatal whatever the app approved, and
        // an approval only stands in for a missing entry.
        let known = match russh::keys::check_known_hosts_path(
            &self.host_name,
            self.port,
            &key,
            &self.known_hosts,
        ) {
            Ok(known) => known,
            Err(russh::keys::Error::IO(source))
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                false
            }
            Err(russh::keys::Error::KeyChanged { .. }) => {
                self.record(HostKeyReport::Mismatch {
                    key_type,
                    fingerprint,
                    approved: None,
                });
                return Ok(false);
            }
            Err(source) => return Err(russh::Error::Keys(source)),
        };
        if known {
            return Ok(true);
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
