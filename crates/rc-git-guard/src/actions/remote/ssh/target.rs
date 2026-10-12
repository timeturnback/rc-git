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
    /// The host actually dialed: ssh-config `HostName` lowercased as OpenSSH does, or
    /// `host` when unset. The `known_hosts` lookup key (as `[host_name]:port` off port 22) unless
    /// `host_key_alias` is set.
    pub host_name: String,
    /// URL port, else ssh-config `Port`, else 22.
    pub port: u16,
    /// URL user, else ssh-config `User`, else `$USER`.
    pub user: String,
    /// Candidate private keys: the ssh-config `IdentityFile` entries, or the OpenSSH
    /// defaults when the config names none. Existence is checked at use.
    pub identities: Vec<PathBuf>,
    /// Whether `identities` came from ssh-config `IdentityFile` entries rather than the
    /// defaults: a configured file that is missing is worth reporting, a default is not.
    pub identities_configured: bool,
    /// ssh-config `IdentitiesOnly yes`: offer only `identities`, never an agent key
    /// that matches none of them.
    pub identities_only: bool,
    /// ssh-config `HostKeyAlias`, lowercased as OpenSSH does: when set, the `known_hosts`
    /// lookup key in place of `host_name` and the port, looked up bare.
    pub host_key_alias: Option<String>,
    /// The `known_hosts` files OpenSSH consults for this host, in order: the ssh-config
    /// `UserKnownHostsFile` entries (else `known_hosts` and `known_hosts2` in the ssh
    /// directory), then the system files when the ssh directory is the default one.
    /// Existence is checked at use.
    pub known_hosts: Vec<PathBuf>,
}

/// The global files OpenSSH reads after the user's (`GlobalKnownHostsFile` default).
const SYSTEM_KNOWN_HOSTS: [&str; 2] = ["/etc/ssh/ssh_known_hosts", "/etc/ssh/ssh_known_hosts2"];

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
        .ok_or_else(|| {
            ActionError::git(op, super::super::redacted(url), "the ssh URL has no host")
        })?
        .to_owned();
    // A caller-chosen ssh directory (tests, `--ssh-dir`) stands in for the whole
    // machine's ssh setup, so the system files are read only for the default one.
    let system_files = ssh_dir.is_none();
    let ssh_dir = match ssh_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_ssh_dir(op)?,
    };
    let (config, identities_only, host_key_alias) = match load_config(&ssh_dir, &host, op, url)? {
        Some(settings) => (
            Some(settings.config),
            settings.identities_only,
            settings.host_key_alias,
        ),
        None => (None, false, None),
    };
    let user = url
        .user()
        .map(str::to_owned)
        .or_else(|| config.as_ref().and_then(|config| config.user.clone()))
        .or_else(|| std::env::var("USER").ok())
        .filter(|user| !user.is_empty())
        .ok_or_else(|| {
            ActionError::git(
                op,
                super::super::redacted(url),
                "no ssh user: none in the URL, the ssh config, or $USER",
            )
        })?;
    let port = url
        .port
        .or_else(|| config.as_ref().and_then(|config| config.port))
        .unwrap_or(22);
    let host_name = config
        .as_ref()
        .and_then(|config| config.hostname.as_deref())
        .map_or_else(|| host.clone(), str::to_ascii_lowercase);
    let known_hosts = known_hosts_files(
        config
            .as_ref()
            .and_then(|config| config.user_known_hosts_file.as_deref()),
        &ssh_dir,
        &host_name,
        system_files,
    );
    // OpenSSH falls back to the default keys only when no `IdentityFile` applies; a
    // default key tried after the configured one can log in as another account.
    let configured = config
        .and_then(|config| config.identity_file)
        .filter(|files| !files.is_empty());
    let identities_configured = configured.is_some();
    let identities = configured.unwrap_or_else(|| {
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
        identities_configured,
        identities_only,
        host_key_alias,
        known_hosts,
    })
}

/// The `known_hosts` files for `host_name`. russh-config keeps a `UserKnownHostsFile`
/// value as one path, so it is split on whitespace here and each part expanded the way
/// OpenSSH does (`~`, `%d`, `%u`, `%h`, `%%`); `none` names no file.
fn known_hosts_files(
    configured: Option<&Path>,
    ssh_dir: &Path,
    host_name: &str,
    system_files: bool,
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = match configured {
        Some(value) => value
            .to_string_lossy()
            .split_whitespace()
            .filter(|part| *part != "none")
            .map(|part| expand_known_hosts_path(part, host_name))
            .collect(),
        None => vec![ssh_dir.join("known_hosts"), ssh_dir.join("known_hosts2")],
    };
    if system_files {
        files.extend(SYSTEM_KNOWN_HOSTS.iter().map(PathBuf::from));
    }
    files
}

/// Expands a leading `~` and the `%d` (local home), `%u` (local user), `%h` (remote
/// host) and `%%` tokens in one `UserKnownHostsFile` path. A token whose value is
/// unknown (no home, no `$USER`) stays as written, naming a file that will not exist.
fn expand_known_hosts_path(part: &str, host_name: &str) -> PathBuf {
    let home = std::env::home_dir().map(|home| home.to_string_lossy().into_owned());
    let user = std::env::var("USER").ok();
    let part = match (part.strip_prefix('~'), &home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => part.to_owned(),
    };
    let mut expanded = String::with_capacity(part.len());
    let mut chars = part.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            expanded.push(c);
            continue;
        }
        let token = chars.next();
        let value = match token {
            Some('%') => Some("%"),
            Some('d') => home.as_deref(),
            Some('u') => user.as_deref(),
            Some('h') => Some(host_name),
            _ => None,
        };
        if let Some(value) = value {
            expanded.push_str(value);
        } else {
            expanded.push('%');
            expanded.extend(token);
        }
    }
    PathBuf::from(expanded)
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
    let path = std::str::from_utf8(path).map_err(|_| {
        ActionError::git(op, path.to_string(), "the remote path is not valid UTF-8")
    })?;
    if path
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\''))
    {
        return Err(ActionError::git(
            op,
            path,
            "the remote path contains NUL, newline, or a single quote",
        ));
    }
    Ok(format!("{} '{path}'", service.as_str()))
}

/// `~/.ssh`.
fn default_ssh_dir(op: &'static str) -> ActionResult<PathBuf> {
    std::env::home_dir()
        .map(|home| home.join(".ssh"))
        .ok_or(ActionError::git(
            op,
            "ssh",
            "no home directory; cannot locate ~/.ssh",
        ))
}

/// What the ssh config says about one host.
struct HostSettings {
    config: russh_config::HostConfig,
    identities_only: bool,
    host_key_alias: Option<String>,
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
    let parse_err = |source: &dyn std::fmt::Display| {
        ActionError::git(
            op,
            super::super::redacted(url),
            format!("cannot parse '{}': {source}", path.display()),
        )
    };
    let contents = std::fs::read_to_string(&path).map_err(|source| parse_err(&source))?;
    let contents = normalize_config(&contents);
    let host = host.to_lowercase();
    // Only `host_config` carries what the config file said: `russh_config::parse`
    // leaves `Config::host_name` as the queried alias and `user`/`port` as `None`.
    let config = parse_for_host(&contents, &host)
        .map_err(|source| parse_err(&source))?
        .host_config;
    let identities_only = parse_for_host(&directive_as_user(&contents, "identitiesonly"), &host)
        .map_err(|source| parse_err(&source))?
        .host_config
        .user
        .is_some_and(|value| value.eq_ignore_ascii_case("yes"));
    let host_key_alias = parse_for_host(&directive_as_user(&contents, "hostkeyalias"), &host)
        .map_err(|source| parse_err(&source))?
        .host_config
        .user
        .map(|alias| alias.trim().to_lowercase())
        .filter(|alias| !alias.is_empty());
    Ok(Some(HostSettings {
        config,
        identities_only,
        host_key_alias,
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

/// russh-config drops `IdentitiesOnly` and `HostKeyAlias`. This view keeps only the
/// `Host` lines and rewrites each line whose keyword is `keyword` (lowercase) as `User`,
/// so the same parser resolves it with the same block matching and first-value-wins rule
/// as every other directive; the resulting `user` field holds that directive's value.
fn directive_as_user(normalized: &str, keyword: &str) -> String {
    let mut view = String::new();
    for line in normalized.lines() {
        let line = line.trim_start();
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key.eq_ignore_ascii_case("host") {
            view.push_str(line);
        } else if key.eq_ignore_ascii_case(keyword) {
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
        assert!(target.identities_configured);
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

    /// `HostKeyAlias` comes from the matching block only, lowercased as OpenSSH does,
    /// and leaves `host_name` and `port` as the address actually dialed.
    #[test]
    fn host_key_alias_is_read_from_the_matching_block() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host gitbox\n\
              \tHostName localhost\n\
              \tPort 2222\n\
              \tHostKeyAlias Git.Corp.Example\n\
              Host other.example.com\n\
              \tPort 2200\n",
        );
        let target =
            resolve(&url("ssh://git@gitbox/x.git"), Some(dir.path()), "fetch").expect("resolve");
        assert_eq!(target.host_key_alias.as_deref(), Some("git.corp.example"));
        assert_eq!(
            (target.host_name.as_str(), target.port),
            ("localhost", 2222)
        );
        let other = resolve(
            &url("ssh://git@other.example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(other.host_key_alias, None);
    }

    /// OpenSSH lowercases `HostName` and records `known_hosts` entries under that name;
    /// russh compares hosts exactly, so a mixed-case value would miss a known key.
    #[test]
    fn host_name_is_lowercased_as_openssh_does() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("config", b"Host alias\n  HostName GitHub.COM\n");
        let target =
            resolve(&url("ssh://git@alias/x.git"), Some(dir.path()), "fetch").expect("resolve");
        assert_eq!(target.host_name, "github.com");
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

    /// With no `UserKnownHostsFile`, OpenSSH reads `known_hosts` then `known_hosts2`;
    /// a caller-chosen ssh directory never adds the machine's /etc files.
    #[test]
    fn default_known_hosts_files_are_known_hosts_and_known_hosts2() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(
            target.known_hosts,
            [
                dir.path().join("known_hosts"),
                dir.path().join("known_hosts2")
            ]
        );
    }

    /// The default ssh directory also reads the system files, after the user's.
    #[test]
    fn the_default_ssh_dir_appends_the_system_known_hosts_files() {
        let files = known_hosts_files(None, Path::new("/home/u/.ssh"), "example.com", true);
        assert_eq!(
            files,
            [
                PathBuf::from("/home/u/.ssh/known_hosts"),
                PathBuf::from("/home/u/.ssh/known_hosts2"),
                PathBuf::from("/etc/ssh/ssh_known_hosts"),
                PathBuf::from("/etc/ssh/ssh_known_hosts2"),
            ]
        );
    }

    /// A configured `UserKnownHostsFile` replaces the default files.
    #[test]
    fn a_configured_user_known_hosts_file_replaces_the_defaults() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host example.com\n  HostName real.example.com\n  UserKnownHostsFile /keys/%h_hosts\n",
        );
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(
            target.known_hosts,
            [PathBuf::from("/keys/real.example.com_hosts")]
        );
    }

    /// Several files on one line are several paths, each expanded on its own; `none`
    /// names no file.
    #[test]
    fn a_multi_file_user_known_hosts_file_is_split_and_expanded() {
        let home = std::env::home_dir().expect("home");
        let user = std::env::var("USER").expect("USER");
        let files = known_hosts_files(
            Some(Path::new("/a/hosts ~/b_hosts\t%d/c none /d/%u/100%%")),
            Path::new("/unused"),
            "example.com",
            false,
        );
        assert_eq!(
            files,
            [
                PathBuf::from("/a/hosts"),
                home.join("b_hosts"),
                home.join("c"),
                PathBuf::from(format!("/d/{user}/100%")),
            ]
        );
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
        assert!(!target.identities_configured);
        assert!(!target.identities_only);
    }
}
