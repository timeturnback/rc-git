//! The ssh authentication ladder: every identity the ssh-agent holds, then every key
//! file the target names. Credentials never leave the process — the agent signs in its
//! own process, key files are read directly, and gix's helper cascade is never involved.

use std::sync::Arc;

use russh::client::Handle;
use russh::keys::{Algorithm, HashAlg, PrivateKeyWithHashAlg};

use crate::actions::{ActionError, ActionResult};

use super::hostkey::HostKeyHandler;
use super::target::SshTarget;

/// Authenticates `handle` as `target.user`, agent first, key files second.
/// `passphrase` deciphers encrypted key files when present.
///
/// # Errors
/// [`ActionError::PassphraseRequired`] when the only remaining candidates are
/// encrypted key files the passphrase (or its absence) cannot open,
/// [`ActionError::AuthRequired`] when every candidate was tried and rejected,
/// [`ActionError::Git`] when the server connection fails mid-auth.
pub(super) async fn authenticate(
    handle: &mut Handle<HostKeyHandler>,
    target: &SshTarget,
    passphrase: Option<&str>,
    op: &'static str,
) -> ActionResult<()> {
    let rsa_hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|source| connection_err(op, &target.host, source))?
        .flatten();
    if try_agent(handle, target, rsa_hash, op).await? {
        return Ok(());
    }
    let mut encrypted: Option<String> = None;
    for path in &target.identities {
        if !path.exists() {
            continue;
        }
        let key = match russh::keys::load_secret_key(path, passphrase) {
            Ok(key) => key,
            Err(russh::keys::Error::KeyIsEncrypted | russh::keys::Error::CouldNotReadKey) => {
                encrypted.get_or_insert_with(|| path.display().to_string());
                continue;
            }
            Err(_) => continue,
        };
        let hash_alg = rsa_hash_for(&key.algorithm(), rsa_hash);
        let result = handle
            .authenticate_publickey(
                target.user.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
            )
            .await
            .map_err(|source| connection_err(op, &target.host, source))?;
        if result.success() {
            return Ok(());
        }
    }
    match encrypted {
        Some(key) => Err(ActionError::PassphraseRequired {
            op,
            host: target.host.clone(),
            key,
        }),
        None => Err(ActionError::AuthRequired {
            op,
            host: target.host.clone(),
        }),
    }
}

/// Offers every public key the ssh-agent holds. A missing or broken agent is not an
/// error — the ladder falls through to key files.
async fn try_agent(
    handle: &mut Handle<HostKeyHandler>,
    target: &SshTarget,
    rsa_hash: Option<HashAlg>,
    op: &'static str,
) -> ActionResult<bool> {
    let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await else {
        return Ok(false);
    };
    let Ok(identities) = agent.request_identities().await else {
        return Ok(false);
    };
    for identity in identities {
        let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash_alg = rsa_hash_for(&key.algorithm(), rsa_hash);
        let result = handle
            .authenticate_publickey_with(target.user.clone(), key, hash_alg, &mut agent)
            .await
            .map_err(|source| connection_err(op, &target.host, source))?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// RSA keys sign with the negotiated hash; every other algorithm ignores it.
fn rsa_hash_for(algorithm: &Algorithm, rsa_hash: Option<HashAlg>) -> Option<HashAlg> {
    match algorithm {
        Algorithm::Rsa { .. } => rsa_hash,
        _ => None,
    }
}

fn connection_err(
    op: &'static str,
    host: &str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ActionError {
    ActionError::Git {
        op,
        name: host.to_owned(),
        source: Box::new(source),
    }
}
