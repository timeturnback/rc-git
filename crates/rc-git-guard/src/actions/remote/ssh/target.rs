//! Resolving an ssh URL against the user's OpenSSH configuration: which host to dial,
//! as which user, on which port, offering which identity files. Pure — no network.

use std::path::{Path, PathBuf};

use crate::actions::{ActionError, ActionResult};

/// Where and how to connect for one ssh remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::actions::remote) struct SshTarget {
    /// The host as written in the URL — what the user sees in prompts and the key
    /// app-side host-key approvals are stored under.
    pub host: String,
    /// The host actually dialed: ssh-config `HostName`, or `host` when unset. The
    /// `known_hosts` lookup key, exactly as OpenSSH records entries.
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
    // Only `host_config` carries what the config file said: `russh_config::parse`
    // leaves `Config::host_name` as the queried alias and `user`/`port` as `None`.
    let config = load_config(&ssh_dir, &host, op, url)?.map(|config| config.host_config);
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
        .and_then(|config| config.hostname.clone())
        .unwrap_or_else(|| host.clone());
    let mut identities = config
        .and_then(|config| config.identity_file)
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
///
/// Three legal OpenSSH spellings are normalized for russh-config's parser: tab
/// separators become spaces (it splits on a single space and would silently skip the
/// line), global options before the first `Host` block get a `Host *` header (it
/// rejects them outright), and `Host` patterns are lowercased along with the query —
/// OpenSSH matches host names case-insensitively, russh-config's glob does not, and
/// gix has already lowercased the URL's host.
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
    let parse_err = |source: &dyn std::fmt::Display| ActionError::Git {
        op,
        name: super::super::redacted(url),
        source: format!("cannot parse '{}': {source}", path.display()).into(),
    };
    let contents = std::fs::read_to_string(&path).map_err(|source| parse_err(&source))?;
    let contents = normalize_config(&contents);
    let host = host.to_lowercase();
    match russh_config::parse(&contents, &host) {
        Ok(config) => Ok(Some(config)),
        Err(russh_config::Error::HostNotFound) => {
            russh_config::parse(&format!("Host *\n{contents}"), &host)
                .map(Some)
                .map_err(|source| parse_err(&source))
        }
        Err(source) => Err(parse_err(&source)),
    }
}

/// Tabs become spaces; `Host` pattern lines are lowercased in full (patterns are host
/// names, matched case-insensitively by OpenSSH). Values on every other line keep
/// their case — an `IdentityFile` path is case-sensitive on disk.
fn normalize_config(contents: &str) -> String {
    let mut normalized = String::with_capacity(contents.len());
    for line in contents.lines() {
        let line = line.replace('\t', " ");
        if line
            .trim_start()
            .split(' ')
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case("host"))
        {
            normalized.push_str(&line.to_lowercase());
        } else {
            normalized.push_str(&line);
        }
        normalized.push('\n');
    }
    normalized
}

fn git_err(op: &'static str, url: &gix::Url, message: &str) -> ActionError {
    ActionError::Git {
        op,
        name: super::super::redacted(url),
        source: message.to_owned().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> gix::Url {
        gix::url::parse(gix::bstr::BStr::new(s)).expect("parse url")
    }

    /// The multi-account alias pattern: the alias resolves through `HostName`, and
    /// `User`, `Port`, and `IdentityFile` come from the matching block.
    #[test]
    fn config_host_name_user_port_and_identity_are_honored() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host bitbucket.org-work\n\
              \tHostName bitbucket.org\n\
              \tUser worker\n\
              \tPort 2222\n\
              \tIdentityFile ./work_key\n",
        );
        let target = resolve(
            &url("ssh://bitbucket.org-work/team/repo.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(target.host, "bitbucket.org-work");
        assert_eq!(target.host_name, "bitbucket.org");
        assert_eq!(target.user, "worker");
        assert_eq!(target.port, 2222);
        assert_eq!(target.identities[0], PathBuf::from("./work_key"));
    }

    /// The URL always wins over the config for user and port.
    #[test]
    fn url_user_and_port_beat_the_config() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("config", b"Host example.com\n  User worker\n  Port 2222\n");
        let target = resolve(
            &url("ssh://git@example.com:2200/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "example.com");
        assert_eq!(target.user, "git");
        assert_eq!(target.port, 2200);
    }

    /// A tab as the key/value separator is legal OpenSSH; russh-config splits on a
    /// single space and would silently skip the line without the normalization.
    #[test]
    fn tab_separated_key_value_lines_are_honored() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host example.com
HostName	real.example.com
",
        );
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "real.example.com");
    }

    /// Global options before the first `Host` block are legal OpenSSH and must not
    /// fail the parse (russh-config rejects them without the retry).
    #[test]
    fn global_options_before_the_first_host_block_parse() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"AddKeysToAgent yes\n\nHost example.com\n  HostName real.example.com\n",
        );
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "real.example.com");
    }

    /// The exact failure from the field: a mixed-case alias in the config, queried
    /// with the host gix already lowercased. OpenSSH matches case-insensitively;
    /// russh-config's glob does not, so both sides are normalized.
    #[test]
    fn mixed_case_host_patterns_match_the_lowercased_url_host() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host github.com-ExampleWork
              	HostName github.com
              	IdentityFile ./Work_Key
",
        );
        let target = resolve(
            &url("ssh://git@github.com-examplework/x/y.git"),
            Some(dir.path()),
            "push",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "github.com");
        // Values outside Host lines keep their case: paths are case-sensitive.
        assert_eq!(target.identities[0], PathBuf::from("./Work_Key"));
    }

    /// No config file: URL host is dialed as written, defaults fill the rest.
    #[test]
    fn no_config_uses_the_url_verbatim() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "example.com");
        assert_eq!(target.port, 22);
        assert_eq!(target.identities.len(), 3);
    }
}
