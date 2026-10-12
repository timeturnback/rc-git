//! The ssh authentication ladder, in the order OpenSSH offers keys (`pubkey_prepare` in
//! its `sshconnect2.c`): agent keys that match an identity file the target names, then
//! the agent's other keys unless `IdentitiesOnly`, then the identity files the agent
//! does not hold. The order is what picks the account: a host such as GitHub accepts
//! the first key registered to any account, so an unrelated agent key offered first
//! logs in as the wrong user and the repository reads as missing.
//!
//! Credentials never leave the process — the agent signs in its own process, key files
//! are read directly, and gix's helper cascade is never involved.

use std::path::Path;
use std::sync::Arc;

use russh::AgentAuthError;
use russh::client::Handle;
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};

use crate::actions::{ActionError, ActionResult, git_err};

use super::hostkey::HostKeyHandler;
use super::target::SshTarget;

type Agent = AgentClient<tokio::net::UnixStream>;

/// One key the ladder offers, by its index in the agent's list or in
/// [`SshTarget::identities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    Agent(usize),
    File(usize),
}

/// What one offer came to.
enum Outcome {
    Accepted,
    Rejected,
    /// An identity file the passphrase (or its absence) cannot open.
    Locked,
}

/// Authenticates `handle` as `target.user`, trying keys in [`offer_order`].
/// `passphrase` deciphers encrypted key files when present.
///
/// # Errors
/// [`ActionError::PassphraseRequired`] when the only remaining candidates are
/// encrypted key files the passphrase (or its absence) cannot open,
/// [`ActionError::AuthRequired`] when every candidate was tried and rejected,
/// [`ActionError::AgentSignRefused`] when the ssh-agent refuses to sign with a key, which
/// leaves the connection unusable for further offers,
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
        .map_err(git_err(op, &target.host))?
        .flatten();
    let mut ladder = Ladder {
        handle,
        target,
        rsa_hash,
        op,
    };
    let (mut agent, agent_keys) = connect_agent().await;
    let file_keys: Vec<Option<PublicKey>> = target
        .identities
        .iter()
        .map(|path| file_public_key(path))
        .collect();
    let order = offer_order(
        &agent_keys
            .iter()
            .map(|agent_key| agent_key.key.key_data())
            .collect::<Vec<_>>(),
        &file_keys
            .iter()
            .map(|key| key.as_ref().map(PublicKey::key_data))
            .collect::<Vec<_>>(),
        target.identities_only,
    );
    let mut locked: Option<&Path> = None;
    for offer in order {
        let outcome = match offer {
            Offer::Agent(index) => {
                let (Some(agent), Some(key)) = (agent.as_mut(), agent_keys.get(index)) else {
                    continue;
                };
                ladder.offer_agent_key(agent, key).await?
            }
            Offer::File(index) => {
                let Some(path) = target.identities.get(index) else {
                    continue;
                };
                let outcome = ladder.offer_file(path, passphrase).await?;
                if let Outcome::Locked = outcome {
                    locked.get_or_insert(path);
                }
                outcome
            }
        };
        if let Outcome::Accepted = outcome {
            return Ok(());
        }
    }
    match locked {
        Some(key) => Err(ActionError::PassphraseRequired {
            op,
            host: target.host.clone(),
            key: key.display().to_string(),
        }),
        None => Err(ActionError::AuthRequired {
            op,
            host: target.host.clone(),
        }),
    }
}

/// The order keys are offered in. `agent` holds the agent's keys, `files` the public
/// half of each identity file (`None` when it cannot be read). An agent key matching an
/// identity file stands in for that file, so the file is not offered again.
fn offer_order<K: PartialEq>(
    agent: &[K],
    files: &[Option<K>],
    identities_only: bool,
) -> Vec<Offer> {
    let mut covered = vec![false; files.len()];
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    for (index, key) in agent.iter().enumerate() {
        let mut matches_a_file = false;
        for (file, file_key) in files.iter().enumerate() {
            if file_key.as_ref() == Some(key) {
                covered[file] = true;
                matches_a_file = true;
            }
        }
        if matches_a_file {
            matched.push(Offer::Agent(index));
        } else if !identities_only {
            unmatched.push(Offer::Agent(index));
        }
    }
    let mut order = matched;
    order.extend(unmatched);
    for (file, covered) in covered.into_iter().enumerate() {
        if !covered {
            order.push(Offer::File(file));
        }
    }
    order
}

/// The connection state every offer needs.
struct Ladder<'a> {
    handle: &'a mut Handle<HostKeyHandler>,
    target: &'a SshTarget,
    rsa_hash: Option<HashAlg>,
    op: &'static str,
}

impl Ladder<'_> {
    async fn offer_agent_key(
        &mut self,
        agent: &mut Agent,
        agent_key: &AgentKey,
    ) -> ActionResult<Outcome> {
        let hash_alg = rsa_hash_for(&agent_key.key.algorithm(), self.rsa_hash);
        let result = self
            .handle
            .authenticate_publickey_with(
                self.target.user.clone(),
                agent_key.key.clone(),
                hash_alg,
                agent,
            )
            .await
            .map_err(|source| agent_auth_err(self.op, &self.target.host, agent_key, source))?;
        Ok(accepted_or_rejected(result.success()))
    }

    async fn offer_file(&mut self, path: &Path, passphrase: Option<&str>) -> ActionResult<Outcome> {
        if !path.exists() {
            return Ok(Outcome::Rejected);
        }
        let key = match russh::keys::load_secret_key(path, passphrase) {
            Ok(key) => key,
            Err(russh::keys::Error::KeyIsEncrypted | russh::keys::Error::CouldNotReadKey) => {
                return Ok(Outcome::Locked);
            }
            Err(_) => return Ok(Outcome::Rejected),
        };
        let hash_alg = rsa_hash_for(&key.algorithm(), self.rsa_hash);
        let result = self
            .handle
            .authenticate_publickey(
                self.target.user.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
            )
            .await
            .map_err(git_err(self.op, &self.target.host))?;
        Ok(accepted_or_rejected(result.success()))
    }
}

/// Maps a failed agent-key offer. A refused signature becomes
/// [`ActionError::AgentSignRefused`] naming the key: russh discards every server message
/// until the signature arrives, so the connection cannot try the next key. Anything else
/// is a transport failure.
fn agent_auth_err(
    op: &'static str,
    host: &str,
    agent_key: &AgentKey,
    source: AgentAuthError,
) -> ActionError {
    match source {
        AgentAuthError::Key(russh::keys::Error::AgentFailure) => ActionError::AgentSignRefused {
            op,
            host: host.to_owned(),
            key: agent_key.describe(),
        },
        other => git_err(op, host)(other),
    }
}

fn accepted_or_rejected(success: bool) -> Outcome {
    if success {
        Outcome::Accepted
    } else {
        Outcome::Rejected
    }
}

/// One plain public key the ssh-agent holds, with the comment it was added under.
struct AgentKey {
    key: PublicKey,
    comment: String,
}

impl AgentKey {
    /// The key as a person recognizes it: its comment, when it has one, and its
    /// `SHA256:...` fingerprint.
    fn describe(&self) -> String {
        let fingerprint = self.key.fingerprint(HashAlg::Sha256);
        if self.comment.is_empty() {
            format!("the agent key {fingerprint}")
        } else {
            format!("'{}' ({fingerprint})", self.comment)
        }
    }
}

/// The ssh-agent and the plain public keys it holds. A missing or broken agent is not
/// an error — the ladder offers identity files only.
async fn connect_agent() -> (Option<Agent>, Vec<AgentKey>) {
    let Ok(mut agent) = Agent::connect_env().await else {
        return (None, Vec::new());
    };
    let Ok(identities) = agent.request_identities().await else {
        return (None, Vec::new());
    };
    let mut keys = Vec::new();
    for identity in identities {
        let AgentIdentity::PublicKey { key, comment } = identity else {
            continue;
        };
        keys.push(AgentKey { key, comment });
    }
    (Some(agent), keys)
}

/// The public half of an identity file: the `.pub` beside it, else the public key an
/// OpenSSH-format private key stores in the clear, encrypted or not. `None` when
/// neither reads; the file is still offered, it just never matches an agent key.
fn file_public_key(path: &Path) -> Option<PublicKey> {
    let mut public_path = path.as_os_str().to_owned();
    public_path.push(".pub");
    if let Ok(key) = russh::keys::load_public_key(&public_path) {
        return Some(key);
    }
    let contents = std::fs::read(path).ok()?;
    PrivateKey::from_openssh(contents)
        .ok()
        .map(|key| key.public_key().clone())
}

/// RSA keys sign with the negotiated hash; every other algorithm ignores it.
fn rsa_hash_for(algorithm: &Algorithm, rsa_hash: Option<HashAlg>) -> Option<HashAlg> {
    match algorithm {
        Algorithm::Rsa { .. } => rsa_hash,
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The field failure: the agent holds another account's key ahead of the one the
    /// config names. The named key must be offered first.
    #[test]
    fn an_agent_key_matching_an_identity_file_is_offered_before_the_rest() {
        let order = offer_order(&[1, 2, 3], &[Some(3)], false);
        assert_eq!(
            order,
            [Offer::Agent(2), Offer::Agent(0), Offer::Agent(1)],
            "the matched agent key stands in for the file, which is not offered again"
        );
    }

    #[test]
    fn identities_only_drops_agent_keys_that_match_no_identity_file() {
        let order = offer_order(&[1, 2, 3], &[Some(3)], true);
        assert_eq!(order, [Offer::Agent(2)]);
    }

    /// A configured key the agent does not hold is still offered, from its file; with
    /// `IdentitiesOnly` it is the only thing offered.
    #[test]
    fn identities_only_with_the_key_outside_the_agent_offers_only_the_file() {
        let order = offer_order(&[1, 2], &[Some(3)], true);
        assert_eq!(order, [Offer::File(0)]);
    }

    #[test]
    fn identity_files_the_agent_does_not_hold_follow_every_agent_key_in_config_order() {
        let order = offer_order(&[1, 2], &[None, Some(2), Some(4)], false);
        assert_eq!(
            order,
            [
                Offer::Agent(1),
                Offer::Agent(0),
                Offer::File(0),
                Offer::File(2)
            ]
        );
    }

    #[test]
    fn no_agent_offers_every_identity_file_in_config_order() {
        let order = offer_order::<u8>(&[], &[Some(1), None], true);
        assert_eq!(order, [Offer::File(0), Offer::File(1)]);
    }

    fn keypair(seed: u8) -> PrivateKey {
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]).into()
    }

    fn agent_key(comment: &str) -> AgentKey {
        AgentKey {
            key: keypair(3).public_key().clone(),
            comment: comment.to_owned(),
        }
    }

    /// The agent answering `SSH_AGENT_FAILURE` (an `ssh-add -c` key, a dismissed approval)
    /// stops the ladder with an auth-class error naming the key, not a bare
    /// "Agent failure".
    #[test]
    fn a_refused_agent_signature_names_the_key_by_comment_and_fingerprint() {
        let key = agent_key("work@laptop");
        let fingerprint = key.key.fingerprint(HashAlg::Sha256).to_string();
        let err = agent_auth_err(
            "fetch",
            "github.com",
            &key,
            AgentAuthError::Key(russh::keys::Error::AgentFailure),
        );
        let ActionError::AgentSignRefused { op, host, key } = err else {
            panic!("expected AgentSignRefused, got {err:?}");
        };
        assert_eq!((op, host.as_str()), ("fetch", "github.com"));
        assert_eq!(key, format!("'work@laptop' ({fingerprint})"));
    }

    #[test]
    fn an_agent_key_without_a_comment_is_named_by_its_fingerprint() {
        let key = agent_key("");
        let fingerprint = key.key.fingerprint(HashAlg::Sha256).to_string();
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
        assert_eq!(key.describe(), format!("the agent key {fingerprint}"));
    }

    #[test]
    fn a_lost_connection_during_an_agent_offer_stays_a_transport_error() {
        let err = agent_auth_err(
            "fetch",
            "github.com",
            &agent_key("work@laptop"),
            AgentAuthError::Send(russh::SendError {}),
        );
        let ActionError::Git { op, .. } = err else {
            panic!("expected a transport error, got {err:?}");
        };
        assert_eq!(op, "fetch");
    }

    /// The `.pub` file wins even when no private key sits beside it — the setup where
    /// the private half lives only in an agent.
    #[test]
    fn the_public_key_comes_from_the_pub_file_beside_the_identity() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let public = keypair(1).public_key().to_openssh().expect("encode");
        dir.write("id_work.pub", public.as_bytes());
        let key = file_public_key(&dir.path().join("id_work")).expect("public key");
        assert_eq!(key.key_data(), keypair(1).public_key().key_data());
    }

    #[test]
    fn without_a_pub_file_the_public_key_comes_from_the_private_key() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let private = keypair(2)
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("encode");
        dir.write("id_work", private.as_bytes());
        let key = file_public_key(&dir.path().join("id_work")).expect("public key");
        assert_eq!(key.key_data(), keypair(2).public_key().key_data());
    }

    #[test]
    fn an_unreadable_identity_has_no_public_key() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("id_work", b"not a key");
        assert!(file_public_key(&dir.path().join("id_work")).is_none());
    }
}
