//! Host-key verification: `known_hosts` is read, never written. An unknown host is
//! refused with its fingerprint so the user can approve it (trust-on-first-use); the
//! approval lives app-side and returns as `RemoteAuth::accepted_hostkey`. A key that
//! contradicts `known_hosts` is refused with no in-app override.

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
    /// The host has a `known_hosts` entry and the offered key contradicts it.
    Mismatch {
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
    host: String,
    port: u16,
    known_hosts: PathBuf,
    accepted: Option<String>,
    report: ReportSlot,
}

impl HostKeyHandler {
    pub(super) fn new(target: &SshTarget, accepted: Option<&str>, report: ReportSlot) -> Self {
        Self {
            host: target.host.clone(),
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
        if self.accepted.as_deref() == Some(fingerprint.as_str()) {
            return Ok(true);
        }
        match russh::keys::check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => {
                self.record(HostKeyReport::Unknown {
                    key_type,
                    fingerprint,
                });
                Ok(false)
            }
            Err(russh::keys::Error::KeyChanged { .. }) => {
                self.record(HostKeyReport::Mismatch {
                    key_type,
                    fingerprint,
                });
                Ok(false)
            }
            Err(russh::keys::Error::IO(source))
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                self.record(HostKeyReport::Unknown {
                    key_type,
                    fingerprint,
                });
                Ok(false)
            }
            Err(source) => Err(russh::Error::Keys(source)),
        }
    }
}
