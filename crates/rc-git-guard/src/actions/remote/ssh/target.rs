//! Resolving an ssh URL against the user's OpenSSH configuration: which host to dial,
//! as which user, on which port, offering which identity files. Pure — no network.

use std::path::{Path, PathBuf};

use crate::actions::{ActionError, ActionResult};

/// Where and how to connect for one ssh remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::actions::remote) struct SshTarget {
    /// The host as written in the URL — the `known_hosts` lookup key and what the
    /// user sees in prompts.
    pub host: String,
    /// The host actually dialed: ssh-config `HostName`, or `host` when unset.
    pub host_name: String,
    /// URL port, else ssh-config `Port`, else 22.
    pub port: u16,
    /// URL user, else ssh-config `User`, else `$USER`.
    pub user: String,
    /// Candidate private keys: ssh-config `IdentityFile` entries first, then the
    /// OpenSSH defaults. Existence is checked at use.
    pub identities: Vec<PathBuf>,
    /// The directory holding `config`, `known_hosts`, and the default keys.
    pub ssh_dir: PathBuf,
}

impl SshTarget {
    /// Path of the `known_hosts` file consulted for this target.
    pub fn known_hosts(&self) -> PathBuf {
        self.ssh_dir.join("known_hosts")
    }
}

/// Resolves `url` (an ssh URL, `ssh://` or scp-like) into a dialable target.
///
/// # Errors
/// [`ActionError::Git`] when the URL lacks a host, when no user can be determined, or
/// when an existing ssh config file cannot be parsed.
pub(in crate::actions::remote) fn resolve(
    url: &gix::Url,
    ssh_dir: Option<&Path>,
    op: &'static str,
) -> ActionResult<SshTarget> {
    let host = url
        .host()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| git_err(op, url, "the ssh URL has no host"))?
        .to_owned();
    let ssh_dir = match ssh_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_ssh_dir(op)?,
    };
    let config = load_config(&ssh_dir, &host, op, url)?;
    let user = url
        .user()
        .map(str::to_owned)
        .or_else(|| config.as_ref().and_then(|config| config.user.clone()))
        .or_else(|| std::env::var("USER").ok())
        .filter(|user| !user.is_empty())
        .ok_or_else(|| {
            git_err(
                op,
                url,
                "no ssh user: none in the URL, the ssh config, or $USER",
            )
        })?;
    let port = url
        .port
        .or_else(|| config.as_ref().and_then(|config| config.port))
        .unwrap_or(22);
    let host_name = config
        .as_ref()
        .map_or_else(|| host.clone(), |config| config.host_name.clone());
    let mut identities = config
        .and_then(|config| config.host_config.identity_file)
        .unwrap_or_default();
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let default = ssh_dir.join(name);
        if !identities.contains(&default) {
            identities.push(default);
        }
    }
    Ok(SshTarget {
        host,
        host_name,
        port,
        user,
        identities,
        ssh_dir,
    })
}

/// The command line executed on the server, e.g. `git-upload-pack 'repo.git'`, with the
/// path single-quoted the way OpenSSH-served git expects.
///
/// # Errors
/// [`ActionError::Git`] when the path contains a byte that cannot travel inside single
/// quotes (NUL, newline, or a single quote itself).
pub(in crate::actions::remote) fn quoted_command(
    service: gix::protocol::transport::Service,
    path: &gix::bstr::BStr,
    op: &'static str,
) -> ActionResult<String> {
    let path = std::str::from_utf8(path).map_err(|_| ActionError::Git {
        op,
        name: path.to_string(),
        source: "the remote path is not valid UTF-8".into(),
    })?;
    if path
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\''))
    {
        return Err(ActionError::Git {
            op,
            name: path.to_owned(),
            source: "the remote path contains NUL, newline, or a single quote".into(),
        });
    }
    Ok(format!("{} '{path}'", service.as_str()))
}

/// `~/.ssh`.
fn default_ssh_dir(op: &'static str) -> ActionResult<PathBuf> {
    std::env::home_dir()
        .map(|home| home.join(".ssh"))
        .ok_or(ActionError::Git {
            op,
            name: "ssh".to_owned(),
            source: "no home directory; cannot locate ~/.ssh".into(),
        })
}

/// Parses `<ssh_dir>/config` for `host`. `None` when the file does not exist; an
/// unreadable or unparseable file is an error rather than a silently wrong connection.
fn load_config(
    ssh_dir: &Path,
    host: &str,
    op: &'static str,
    url: &gix::Url,
) -> ActionResult<Option<russh_config::Config>> {
    let path = ssh_dir.join("config");
    if !path.exists() {
        return Ok(None);
    }
    russh_config::parse_path(&path, host)
        .map(Some)
        .map_err(|source| ActionError::Git {
            op,
            name: super::super::redacted(url),
            source: format!("cannot parse '{}': {source}", path.display()).into(),
        })
}

fn git_err(op: &'static str, url: &gix::Url, message: &str) -> ActionError {
    ActionError::Git {
        op,
        name: super::super::redacted(url),
        source: message.to_owned().into(),
    }
}
