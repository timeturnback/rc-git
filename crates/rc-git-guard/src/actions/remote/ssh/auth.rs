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
use russh::keys::ssh_key::sec1::der::{
    self, Decode, Document, Reader, Tag, Tagged,
    asn1::{AnyRef, ObjectIdentifier},
};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};

use crate::actions::{ActionError, ActionResult, git_err};

use super::hostkey::HostKeyHandler;
use super::target::{AgentSocket, SshTarget};

type Agent = AgentClient<tokio::net::UnixStream>;

/// One key the ladder offers, by its index in the agent's list or in
/// [`SshTarget::identities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    Agent(usize),
    File(usize),
}

/// What one offer came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Accepted,
    Rejected,
    /// An identity file the passphrase (or its absence) cannot open: encrypted with no
    /// passphrase given, or the passphrase given does not decrypt it.
    Locked,
    /// An identity file no passphrase can make usable, with the reason: not a private
    /// key, unreadable, an unsupported format, or a configured file that does not exist.
    Unusable(String),
    /// A default identity file that does not exist; OpenSSH skips these silently.
    Absent,
}

/// One key the ladder offered, as a person recognizes it, and what came of it.
struct Tried {
    key: String,
    outcome: Outcome,
}

/// Authenticates `handle` as `target.user`, trying keys in [`offer_order`].
/// `passphrase` deciphers encrypted key files when present.
///
/// # Errors
/// [`ActionError::PassphraseRequired`] when no key was accepted and an encrypted key
/// file the passphrase (or its absence) cannot open remains,
/// [`ActionError::Git`] naming each identity file that cannot be used and why, when no
/// key was accepted and none is locked,
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
    let (mut agent, agent_keys) = connect_agent(&target.identity_agent).await;
    let file_keys: Vec<Option<FileKey>> = target
        .identities
        .iter()
        .map(|path| file_public_key(path))
        .collect();
    let order = key_order(&agent_keys, &file_keys, target.identities_only);
    let agent_reach = AgentReach::of(&target.identity_agent, agent.is_some());
    let mut tried = Vec::new();
    for offer in order {
        let (key, outcome) = match offer {
            Offer::Agent(index) => {
                let (Some(agent), Some(key)) = (agent.as_mut(), agent_keys.get(index)) else {
                    continue;
                };
                (key.describe(), ladder.offer_agent_key(agent, key).await?)
            }
            Offer::File(index) => {
                let Some(path) = target.identities.get(index) else {
                    continue;
                };
                let public_only = file_keys
                    .get(index)
                    .and_then(Option::as_ref)
                    .is_some_and(|file_key| file_key.public_only);
                let outcome = ladder
                    .offer_file(path, passphrase, target.identities_configured, public_only)
                    .await?;
                (path.display().to_string(), outcome)
            }
        };
        if let Outcome::Accepted = outcome {
            return Ok(());
        }
        tried.push(Tried { key, outcome });
    }
    Err(ladder_error(op, &target.host, &tried, agent_reach))
}

/// Whether the ladder had an ssh-agent to offer keys from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentReach {
    Reached,
    /// The socket was named but did not answer with a key list.
    Unreachable,
    /// [`AgentSocket::None`]: the config turned the agent off, so none was asked.
    Disabled,
}

impl AgentReach {
    fn of(socket: &AgentSocket, connected: bool) -> Self {
        match socket {
            AgentSocket::None => Self::Disabled,
            AgentSocket::Env | AgentSocket::Path(_) if connected => Self::Reached,
            AgentSocket::Env | AgentSocket::Path(_) => Self::Unreachable,
        }
    }
}

/// The error for a ladder that ended with no key accepted. A locked key comes first: a
/// passphrase can still open it. Then the files no passphrase can fix, by name and
/// reason, so the user is not asked for one. Otherwise every key offered was rejected.
fn ladder_error(op: &'static str, host: &str, tried: &[Tried], agent: AgentReach) -> ActionError {
    let mut unusable = Vec::new();
    let mut rejected = 0usize;
    for attempt in tried {
        match &attempt.outcome {
            Outcome::Locked => {
                return ActionError::PassphraseRequired {
                    op,
                    host: host.to_owned(),
                    key: attempt.key.clone(),
                };
            }
            Outcome::Unusable(reason) => unusable.push(format!("'{}' ({reason})", attempt.key)),
            Outcome::Rejected => rejected += 1,
            Outcome::Accepted | Outcome::Absent => {}
        }
    }
    if unusable.is_empty() {
        return ActionError::AuthRequired {
            op,
            host: host.to_owned(),
        };
    }
    let mut parts = vec![format!(
        "no ssh key was accepted; cannot use the identity file {}",
        unusable.join(", ")
    )];
    if rejected > 0 {
        parts.push(format!("the server rejected {rejected} other key(s)"));
    }
    match agent {
        AgentReach::Reached => {}
        AgentReach::Unreachable => parts.push("no ssh-agent was reachable".to_owned()),
        AgentReach::Disabled => parts.push(
            "no ssh-agent was asked (IdentityAgent is none or names an unset variable)".to_owned(),
        ),
    }
    ActionError::git(op, host, parts.join("; "))
}

/// [`offer_order`] over the agent's keys and the identity files' public halves, compared
/// by [`key_identity`] so a key matches whatever comment each side carries.
fn key_order(
    agent_keys: &[AgentKey],
    file_keys: &[Option<FileKey>],
    identities_only: bool,
) -> Vec<Offer> {
    let mut agent = Vec::new();
    for agent_key in agent_keys {
        agent.push(key_identity(&agent_key.key));
    }
    let mut files = Vec::new();
    for file_key in file_keys {
        files.push(
            file_key
                .as_ref()
                .map(|file_key| key_identity(&file_key.key)),
        );
    }
    offer_order(&agent, &files, identities_only)
}

/// What makes two public keys the same key: the algorithm and key material, not the
/// comment. The agent reports the comment `ssh-add` read, a `.pub` file carries its own,
/// and the two rarely agree.
fn key_identity(key: &PublicKey) -> &russh::keys::ssh_key::public::KeyData {
    key.key_data()
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

    async fn offer_file(
        &mut self,
        path: &Path,
        passphrase: Option<&str>,
        configured: bool,
        public_only: bool,
    ) -> ActionResult<Outcome> {
        let key = match load_identity(path, passphrase, configured, public_only) {
            Ok(key) => key,
            Err(outcome) => return Ok(outcome),
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

/// Reads and decodes one identity file, or says why it cannot be offered. A missing
/// default file is [`Outcome::Absent`]; a missing configured one is unusable. A
/// `public_only` identity (a public key named as `IdentityFile`) stands only for the
/// agent key it matches, so reaching here means the agent does not hold it: no
/// passphrase can open a public key.
fn load_identity(
    path: &Path,
    passphrase: Option<&str>,
    configured: bool,
    public_only: bool,
) -> Result<PrivateKey, Outcome> {
    if public_only {
        return Err(Outcome::Unusable(NOT_HELD.to_owned()));
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(if configured {
                Outcome::Unusable("no such file".to_owned())
            } else {
                Outcome::Absent
            });
        }
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            return Err(Outcome::Unusable(NOT_A_KEY.to_owned()));
        }
        Err(err) => return Err(Outcome::Unusable(err.to_string())),
    };
    if is_security_key(&contents) {
        return Err(Outcome::Unusable(SECURITY_KEY.to_owned()));
    }
    let has_header = |header: &str| contents.lines().any(|line| line == header);
    let encrypted_pkcs8 = has_header("-----BEGIN ENCRYPTED PRIVATE KEY-----");
    if encrypted_pkcs8 && pbkdf2_prf(&contents) == Some(HMAC_WITH_SHA1) {
        return Err(Outcome::Unusable(SHA1_PKCS8.to_owned()));
    }
    // russh fails a plain PKCS#8 key handed any passphrase, so a stored passphrase meant
    // for another identity must not reach it.
    let passphrase = if has_header("-----BEGIN PRIVATE KEY-----") {
        None
    } else {
        passphrase
    };
    russh::keys::decode_secret_key(&contents, passphrase)
        .map_err(|err| classify_load_error(&err, passphrase.is_some(), encrypted_pkcs8))
}

/// Whether an OpenSSH-format key file holds a FIDO security key (`sk-*`). Its public half
/// is stored in the clear, so an encrypted one is recognized without a passphrase. Signing
/// needs the hardware token, which rc-git cannot drive.
fn is_security_key(contents: &str) -> bool {
    let Ok(key) = PrivateKey::from_openssh(contents) else {
        return false;
    };
    matches!(
        key.algorithm(),
        Algorithm::SkEcdsaSha2NistP256 | Algorithm::SkEd25519
    )
}

const NOT_A_KEY: &str = "not a private key";
const SECURITY_KEY: &str = "a security key, which rc-git does not support";
const NOT_HELD: &str = "names a key the agent does not hold";
const SHA1_PKCS8: &str = "encrypted with PBKDF2-SHA1, which rc-git cannot decrypt; \
                          re-encrypt it with ssh-keygen -p";

const PBES2: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.5.13");
const PBKDF2: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.5.12");
/// PBKDF2's default PRF, which russh's PKCS#5 build leaves out: it fails such a key with
/// the same error a wrong passphrase gives, so no passphrase would ever open it.
const HMAC_WITH_SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.2.7");

/// The PBKDF2 PRF of an encrypted PKCS#8 PEM key (RFC 8018 `PBES2-params`), or `None`
/// when the key uses another scheme or does not parse — russh then reports the error.
fn pbkdf2_prf(pem: &str) -> Option<ObjectIdentifier> {
    let (_, doc) = Document::from_pem(pem).ok()?;
    let info = sequence(AnyRef::from_der(doc.as_bytes()).ok()?)?;
    let algorithm = sequence(*info.first()?)?;
    if oid(algorithm.first()?)? != PBES2 {
        return None;
    }
    let kdf = sequence(*sequence(*algorithm.get(1)?)?.first()?)?;
    if oid(kdf.first()?)? != PBKDF2 {
        return None;
    }
    // salt, iterationCount, then an optional keyLength INTEGER and an optional prf.
    let params = sequence(*kdf.get(1)?)?;
    match params
        .iter()
        .skip(2)
        .find(|field| field.tag() == Tag::Sequence)
    {
        Some(prf) => oid(sequence(*prf)?.first()?),
        None => Some(HMAC_WITH_SHA1),
    }
}

/// The elements of a DER `SEQUENCE`, undecoded.
fn sequence(any: AnyRef<'_>) -> Option<Vec<AnyRef<'_>>> {
    any.sequence(|reader| {
        let mut elements = Vec::new();
        while !reader.is_finished() {
            elements.push(AnyRef::decode(reader)?);
        }
        Ok::<_, der::Error>(elements)
    })
    .ok()
}

fn oid(any: &AnyRef<'_>) -> Option<ObjectIdentifier> {
    any.decode_as().ok()
}

/// What a failed key decode means for the ladder. russh reports a wrong passphrase as a
/// decrypt failure that differs by format — `SshKey(Crypto)` for OpenSSH, `Unpad` for
/// PEM, `Pkcs8` for encrypted PKCS#8 — and an encrypted PKCS#8 key read with no
/// passphrase fails as a DER parse error. Everything else no passphrase can fix.
fn classify_load_error(
    err: &russh::keys::Error,
    passphrase_given: bool,
    encrypted_pkcs8: bool,
) -> Outcome {
    use russh::keys::Error;
    match err {
        Error::KeyIsEncrypted => Outcome::Locked,
        Error::SshKey(russh::keys::ssh_key::Error::Crypto) | Error::Unpad(_) | Error::Pkcs8(_)
            if passphrase_given =>
        {
            Outcome::Locked
        }
        Error::Der(_) if encrypted_pkcs8 && !passphrase_given => Outcome::Locked,
        Error::CouldNotReadKey => Outcome::Unusable(NOT_A_KEY.to_owned()),
        other => Outcome::Unusable(other.to_string()),
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

/// The ssh-agent the target's `IdentityAgent` names and the plain public keys it holds.
/// A missing or broken agent is not an error — the ladder offers identity files only —
/// and [`AgentSocket::None`] connects to none, so no agent key reaches the server.
async fn connect_agent(socket: &AgentSocket) -> (Option<Agent>, Vec<AgentKey>) {
    let connected = match socket {
        AgentSocket::Env => Agent::connect_env().await,
        AgentSocket::Path(path) => Agent::connect_uds(path).await,
        AgentSocket::None => return (None, Vec::new()),
    };
    let Ok(mut agent) = connected else {
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

/// The public half of an identity file, and whether the file is only that half.
struct FileKey {
    key: PublicKey,
    /// The file itself is a public key: `IdentityFile` names a key the agent holds.
    public_only: bool,
}

/// The public half of an identity file, in OpenSSH's `sshkey_load_public` order: the
/// file itself as a public key (`ssh_config(5)` lets `IdentityFile` name a `.pub` to pick
/// an agent key), then the `.pub` beside it, then the public key an OpenSSH-format
/// private key stores in the clear, encrypted or not. `None` when none reads; the file
/// is still offered, it just never matches an agent key.
fn file_public_key(path: &Path) -> Option<FileKey> {
    if let Ok(key) = russh::keys::load_public_key(path) {
        return Some(FileKey {
            key,
            public_only: true,
        });
    }
    let mut public_path = path.as_os_str().to_owned();
    public_path.push(".pub");
    if let Ok(key) = russh::keys::load_public_key(&public_path) {
        return Some(FileKey {
            key,
            public_only: false,
        });
    }
    let contents = std::fs::read(path).ok()?;
    let key = PrivateKey::from_openssh(contents).ok()?;
    Some(FileKey {
        key: key.public_key().clone(),
        public_only: false,
    })
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
        let file_key = file_public_key(&dir.path().join("id_work")).expect("public key");
        assert_eq!(file_key.key.key_data(), keypair(1).public_key().key_data());
        assert!(!file_key.public_only);
    }

    #[test]
    fn without_a_pub_file_the_public_key_comes_from_the_private_key() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let private = keypair(2)
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("encode");
        dir.write("id_work", private.as_bytes());
        let file_key = file_public_key(&dir.path().join("id_work")).expect("public key");
        assert_eq!(file_key.key.key_data(), keypair(2).public_key().key_data());
        assert!(!file_key.public_only);
    }

    /// `ssh_config(5)`: `IdentityFile` may name a public key to pick the agent key it
    /// matches. The file must read as that key, not as `<file>.pub.pub` or a private key.
    #[test]
    fn a_pub_identity_file_is_its_own_public_key() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let public = keypair(1).public_key().to_openssh().expect("encode");
        dir.write("id_work.pub", public.as_bytes());
        let file_key = file_public_key(&dir.path().join("id_work.pub")).expect("public key");
        assert_eq!(file_key.key.key_data(), keypair(1).public_key().key_data());
        assert!(file_key.public_only);
    }

    fn pub_identity_order(agent: &[PrivateKey], identities_only: bool) -> Vec<Offer> {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let public = keypair(1).public_key().to_openssh().expect("encode");
        dir.write("id_work.pub", public.as_bytes());
        let file_key = file_public_key(&dir.path().join("id_work.pub")).expect("public key");
        let agent: Vec<_> = agent
            .iter()
            .map(|key| key.public_key().key_data())
            .collect();
        offer_order(&agent, &[Some(file_key.key.key_data())], identities_only)
    }

    #[test]
    fn a_pub_identity_file_picks_its_agent_key_first() {
        let agent = [keypair(2), keypair(1)];
        assert_eq!(
            pub_identity_order(&agent, false),
            [Offer::Agent(1), Offer::Agent(0)]
        );
    }

    #[test]
    fn a_pub_identity_file_under_identities_only_offers_only_its_agent_key() {
        let agent = [keypair(2), keypair(1)];
        assert_eq!(pub_identity_order(&agent, true), [Offer::Agent(1)]);
    }

    /// The agent does not hold the key a `.pub` identity names: no passphrase can open a
    /// public key, so it must not read as locked.
    #[test]
    fn a_pub_identity_the_agent_does_not_hold_is_unusable_not_locked() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let public = keypair(1).public_key().to_openssh().expect("encode");
        dir.write("id_work.pub", public.as_bytes());
        let path = dir.path().join("id_work.pub");
        for passphrase in [None, Some("pass")] {
            assert_eq!(
                load_identity(&path, passphrase, true, true).err(),
                Some(Outcome::Unusable(NOT_HELD.to_owned())),
                "{passphrase:?}"
            );
        }
    }

    fn tried(key: &str, outcome: Outcome) -> Tried {
        Tried {
            key: key.to_owned(),
            outcome,
        }
    }

    fn security_key_pem(encrypted: bool) -> String {
        use russh::keys::ssh_key::{private, public};
        let public = public::SkEd25519::new(
            *keypair(4)
                .public_key()
                .key_data()
                .ed25519()
                .expect("ed25519"),
            "ssh:",
        );
        let sk = private::SkEd25519::new(public, 0x01, vec![7; 32]).expect("sk key");
        let mut key =
            PrivateKey::new(private::KeypairData::SkEd25519(sk), "token").expect("private key");
        if encrypted {
            let kdf = russh::keys::ssh_key::Kdf::Bcrypt {
                salt: vec![9; 16],
                rounds: 1,
            };
            key = key
                .encrypt_with(russh::keys::ssh_key::Cipher::Aes256Ctr, kdf, 7, "pass")
                .expect("encrypt");
        }
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("encode")
            .to_string()
    }

    /// A security key needs its hardware token to sign; offering it used to fail the
    /// whole ladder mid-auth instead of moving on to the next key.
    #[test]
    fn a_security_key_file_is_skipped_as_unusable_encrypted_or_not() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        for encrypted in [false, true] {
            dir.write("id_ed25519_sk", security_key_pem(encrypted).as_bytes());
            let path = dir.path().join("id_ed25519_sk");
            for passphrase in [None, Some("pass")] {
                assert_eq!(
                    load_identity(&path, passphrase, false, false).err(),
                    Some(Outcome::Unusable(SECURITY_KEY.to_owned())),
                    "encrypted {encrypted}, {passphrase:?}"
                );
            }
        }
    }

    #[test]
    fn a_missing_default_identity_is_skipped_but_a_missing_configured_one_is_unusable() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let path = dir.path().join("id_work");
        assert_eq!(
            load_identity(&path, None, false, false).err(),
            Some(Outcome::Absent)
        );
        assert_eq!(
            load_identity(&path, None, true, false).err(),
            Some(Outcome::Unusable("no such file".to_owned()))
        );
    }

    /// A `.pub` file or any other headerless file used to read as encrypted, which asked
    /// for a passphrase the key does not have.
    #[test]
    fn a_public_key_or_garbage_file_is_unusable_not_locked() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let public = keypair(1).public_key().to_openssh().expect("encode");
        dir.write("id_work.pub", public.as_bytes());
        dir.write("id_garbage", b"not a key");
        dir.write("id_binary", &[0xff, 0xfe, 0x00]);
        for name in ["id_work.pub", "id_garbage", "id_binary"] {
            let outcome = load_identity(&dir.path().join(name), Some("pass"), true, false).err();
            assert_eq!(
                outcome,
                Some(Outcome::Unusable(NOT_A_KEY.to_owned())),
                "{name}"
            );
        }
    }

    /// A passphrase stored for another identity must not lock out a plain PKCS#8 key.
    #[test]
    fn an_unencrypted_pkcs8_key_loads_even_with_a_passphrase_given() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let key = keypair(2);
        let mut pem = Vec::new();
        russh::keys::encode_pkcs8_pem(&key, &mut pem).expect("encode");
        dir.write("id_plain", &pem);
        let loaded = load_identity(&dir.path().join("id_plain"), Some("x"), true, false);
        assert_eq!(
            loaded.map(|loaded| loaded.public_key().clone()),
            Ok(key.public_key().clone())
        );
    }

    /// An encrypted PKCS#8 key with its PBKDF2 PRF OID ending in `prf_last_byte`: russh
    /// writes hmacWithSHA256 (`0x09`), which differs from hmacWithSHA1 (`0x07`) only there.
    fn encrypted_pkcs8_pem(prf_last_byte: u8) -> String {
        let sha256_prf = [0x06, 0x08, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x02, 0x09];
        let mut der =
            russh::keys::pkcs8::encode_pkcs8_encrypted(b"right", 1, &keypair(3)).expect("encode");
        let at = der
            .windows(sha256_prf.len())
            .position(|window| window == sha256_prf)
            .expect("an explicit hmacWithSHA256 PRF");
        der[at + sha256_prf.len() - 1] = prf_last_byte;
        Document::try_from(der.as_slice())
            .expect("der")
            .to_pem("ENCRYPTED PRIVATE KEY", der::pem::LineEnding::LF)
            .expect("pem")
    }

    /// russh cannot derive a PBKDF2-SHA1 key and fails it like a wrong passphrase, which
    /// used to prompt again until the attempt limit.
    #[test]
    fn a_pbkdf2_sha1_pkcs8_key_is_unusable_with_or_without_a_passphrase() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("id_sha1", encrypted_pkcs8_pem(0x07).as_bytes());
        let path = dir.path().join("id_sha1");
        for passphrase in [Some("right"), Some("wrong"), None] {
            assert_eq!(
                load_identity(&path, passphrase, true, false).err(),
                Some(Outcome::Unusable(SHA1_PKCS8.to_owned())),
                "{passphrase:?}"
            );
        }
    }

    #[test]
    fn a_pbkdf2_sha256_pkcs8_key_is_locked_until_the_right_passphrase() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("id_sha256", encrypted_pkcs8_pem(0x09).as_bytes());
        let path = dir.path().join("id_sha256");
        assert_eq!(
            load_identity(&path, Some("wrong"), true, false).err(),
            Some(Outcome::Locked)
        );
        assert_eq!(
            load_identity(&path, None, true, false).err(),
            Some(Outcome::Locked)
        );
        assert_eq!(
            load_identity(&path, Some("right"), true, false).map(|key| key.public_key().clone()),
            Ok(keypair(3).public_key().clone())
        );
    }

    #[test]
    fn decrypt_failures_are_locked_only_when_a_passphrase_was_given() {
        use russh::keys::Error;
        let crypto = Error::SshKey(russh::keys::ssh_key::Error::Crypto);
        assert_eq!(classify_load_error(&crypto, true, false), Outcome::Locked);
        assert_eq!(
            classify_load_error(&crypto, false, false),
            Outcome::Unusable(crypto.to_string())
        );
        assert_eq!(
            classify_load_error(&Error::KeyIsEncrypted, false, false),
            Outcome::Locked
        );
        assert_eq!(
            classify_load_error(&Error::CouldNotReadKey, true, false),
            Outcome::Unusable(NOT_A_KEY.to_owned())
        );
    }

    /// russh reads an encrypted PKCS#8 key with no passphrase as a DER parse error.
    #[test]
    fn a_der_error_is_locked_only_for_an_encrypted_pkcs8_file_read_without_a_passphrase() {
        let der = russh::keys::Error::Der(der_error());
        assert_eq!(classify_load_error(&der, false, true), Outcome::Locked);
        assert_eq!(
            classify_load_error(&der, false, false),
            Outcome::Unusable(der.to_string())
        );
        assert_eq!(
            classify_load_error(&der, true, true),
            Outcome::Unusable(der.to_string())
        );
    }

    fn der_error() -> russh::keys::ssh_key::sec1::der::Error {
        russh::keys::ssh_key::sec1::der::ErrorKind::Failed.into()
    }

    #[test]
    fn a_locked_key_wins_over_unusable_files_and_is_named() {
        let err = ladder_error(
            "fetch",
            "example.com",
            &[
                tried("/k/id_bad", Outcome::Unusable(NOT_A_KEY.to_owned())),
                tried("/k/id_locked", Outcome::Locked),
            ],
            AgentReach::Reached,
        );
        let ActionError::PassphraseRequired { key, .. } = &err else {
            panic!("expected PassphraseRequired, got {err:?}");
        };
        assert_eq!(key, "/k/id_locked");
        assert!(
            err.to_string().contains("or the passphrase is wrong"),
            "{err}"
        );
    }

    #[test]
    fn unusable_files_are_named_with_their_reasons_instead_of_asking_for_auth() {
        let err = ladder_error(
            "fetch",
            "example.com",
            &[
                tried("'work' (SHA256:x)", Outcome::Rejected),
                tried("/k/id_work", Outcome::Unusable("no such file".to_owned())),
                tried("/k/id_rsa", Outcome::Absent),
                tried("/k/id_bad", Outcome::Unusable(NOT_A_KEY.to_owned())),
            ],
            AgentReach::Unreachable,
        );
        assert!(matches!(&err, ActionError::Git { .. }), "{err:?}");
        assert_eq!(
            err.to_string(),
            "fetch 'example.com': no ssh key was accepted; cannot use the identity file \
             '/k/id_work' (no such file), '/k/id_bad' (not a private key); the server \
             rejected 1 other key(s); no ssh-agent was reachable"
        );
    }

    #[test]
    fn only_rejections_and_absent_defaults_stay_auth_required() {
        let err = ladder_error(
            "push",
            "example.com",
            &[
                tried("/k/id_ed25519", Outcome::Rejected),
                tried("/k/id_rsa", Outcome::Absent),
            ],
            AgentReach::Reached,
        );
        assert!(
            matches!(&err, ActionError::AuthRequired { op: "push", .. }),
            "{err:?}"
        );
    }

    #[test]
    fn an_unreadable_identity_has_no_public_key() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("id_work", b"not a key");
        assert!(file_public_key(&dir.path().join("id_work")).is_none());
    }

    /// `IdentityAgent none` asks no agent, so the error must not blame an unreachable one.
    #[test]
    fn a_disabled_agent_is_named_as_turned_off_not_unreachable() {
        let err = ladder_error(
            "fetch",
            "example.com",
            &[tried(
                "/k/id_work",
                Outcome::Unusable("no such file".to_owned()),
            )],
            AgentReach::of(&AgentSocket::None, false),
        );
        assert_eq!(
            err.to_string(),
            "fetch 'example.com': no ssh key was accepted; cannot use the identity file \
             '/k/id_work' (no such file); no ssh-agent was asked (IdentityAgent is none or \
             names an unset variable)"
        );
    }

    #[test]
    fn an_agent_socket_that_answered_is_reached_and_one_that_did_not_is_unreachable() {
        let path = AgentSocket::Path("/tmp/agent.sock".into());
        assert_eq!(AgentReach::of(&AgentSocket::Env, true), AgentReach::Reached);
        assert_eq!(AgentReach::of(&path, false), AgentReach::Unreachable);
        assert_eq!(
            AgentReach::of(&AgentSocket::None, false),
            AgentReach::Disabled
        );
    }

    /// The agent reports the comment `ssh-add` read; the `.pub` beside the identity file
    /// carries its own. The same key must still match, so the file is not offered twice
    /// and `IdentitiesOnly` keeps the agent key.
    #[test]
    fn the_same_key_matches_across_different_comments() {
        let mut agent_public = keypair(1).public_key().clone();
        agent_public.set_comment("work@laptop");
        let mut file_public = keypair(1).public_key().clone();
        file_public.set_comment("long@desktop");
        assert_ne!(
            agent_public, file_public,
            "the comments must differ for the test"
        );
        let agent_keys = [AgentKey {
            key: agent_public,
            comment: "work@laptop".to_owned(),
        }];
        let file_keys = [Some(FileKey {
            key: file_public,
            public_only: false,
        })];
        assert_eq!(key_order(&agent_keys, &file_keys, true), [Offer::Agent(0)]);
    }
}
