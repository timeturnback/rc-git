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
    /// Candidate private keys: the ssh-config `IdentityFile` entries, or the OpenSSH
    /// defaults when the config names none. Existence is checked at use.
    pub identities: Vec<PathBuf>,
    /// ssh-config `IdentitiesOnly yes`: offer only `identities`, never an agent key
    /// that matches none of them.
    pub identities_only: bool,
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
    let (config, identities_only) = match load_config(&ssh_dir, &host, op, url)? {
        Some(settings) => (Some(settings.config), settings.identities_only),
        None => (None, false),
    };
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
    // OpenSSH falls back to the default keys only when no `IdentityFile` applies; a
    // default key tried after the configured one can log in as another account.
    let identities = config
        .and_then(|config| config.identity_file)
        .filter(|files| !files.is_empty())
        .unwrap_or_else(|| {
            ["id_ed25519", "id_ecdsa", "id_rsa"]
                .into_iter()
                .map(|name| ssh_dir.join(name))
                .collect()
        });
    Ok(SshTarget {
        host,
        host_name,
        port,
        user,
        identities,
        identities_only,
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

/// What the ssh config says about one host.
struct HostSettings {
    config: russh_config::HostConfig,
    identities_only: bool,
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
) -> ActionResult<Option<HostSettings>> {
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
    // Only `host_config` carries what the config file said: `russh_config::parse`
    // leaves `Config::host_name` as the queried alias and `user`/`port` as `None`.
    let config = parse_for_host(&contents, &host)
        .map_err(|source| parse_err(&source))?
        .host_config;
    let identities_only = parse_for_host(&identities_only_as_user(&contents), &host)
        .map_err(|source| parse_err(&source))?
        .host_config
        .user
        .is_some_and(|value| value.eq_ignore_ascii_case("yes"));
    Ok(Some(HostSettings {
        config,
        identities_only,
    }))
}

/// `russh_config::parse`, retried once under a synthetic `Host *` when options precede
/// the first `Host` block.
fn parse_for_host(contents: &str, host: &str) -> Result<russh_config::Config, russh_config::Error> {
    let parsed = russh_config::parse(contents, host);
    if let Err(russh_config::Error::HostNotFound) = parsed {
        return russh_config::parse(&format!("Host *\n{contents}"), host);
    }
    parsed
}

/// russh-config drops `IdentitiesOnly`. This view keeps only the `Host` lines and
/// rewrites each `IdentitiesOnly` line as `User`, so the same parser resolves it with
/// the same block matching and first-value-wins rule as every other directive; the
/// resulting `user` field holds the `IdentitiesOnly` value.
fn identities_only_as_user(normalized: &str) -> String {
    let mut view = String::new();
    for line in normalized.lines() {
        let line = line.trim_start();
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key.eq_ignore_ascii_case("host") {
            view.push_str(line);
        } else if key.eq_ignore_ascii_case("identitiesonly") {
            view.push_str("User ");
            view.push_str(value);
        } else {
            continue;
        }
        view.push('\n');
    }
    view
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
        assert_eq!(
            target.identities,
            [PathBuf::from("./work_key")],
            "a configured IdentityFile replaces the default keys, as in OpenSSH"
        );
        assert!(!target.identities_only);
    }

    /// The multi-account pattern that broke in the field: the alias names its key and
    /// sets `IdentitiesOnly`, and the agent holds other accounts' keys.
    #[test]
    fn identities_only_is_read_from_the_matching_block() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host github.com-Work\n\
              \tHostName github.com\n\
              \tIdentityFile ./work_key\n\
              \tIdentitiesOnly yes\n\
              Host other.example.com\n\
              \tIdentitiesOnly no\n",
        );
        let target = resolve(
            &url("git@github.com-work:team/repo.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert!(target.identities_only);
        let other = resolve(
            &url("git@other.example.com:team/repo.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert!(!other.identities_only);
    }

    /// OpenSSH keeps the first value obtained, so a global line above every `Host`
    /// block wins over a later block's.
    #[test]
    fn identities_only_takes_the_first_value_in_file_order() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"identitiesonly YES\n\nHost example.com\n  IdentitiesOnly no\n",
        );
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert!(target.identities_only);
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
        assert!(!target.identities_only);
    }
}
