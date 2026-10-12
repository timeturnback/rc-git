//! Resolving an ssh URL against the user's OpenSSH configuration: which host to dial,
//! as which user, on which port, offering which identity files through which agent.
//! Pure — no network; the config file and the files it `Include`s are only read.

use std::path::{Path, PathBuf};

use crate::actions::remote::redacted;
use crate::actions::{ActionError, ActionResult};

/// Where and how to connect for one ssh remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::actions::remote) struct SshTarget {
    /// The host as written in the URL — what the user sees in prompts and the key
    /// app-side host-key approvals are stored under.
    pub host: String,
    /// The host actually dialed: ssh-config `HostName` with `%h` expanded and lowercased
    /// as OpenSSH does, or `host` when unset. The `known_hosts` lookup key (as
    /// `[host_name]:port` off port 22) unless `host_key_alias` is set.
    pub host_name: String,
    /// URL port, else ssh-config `Port`, else 22.
    pub port: u16,
    /// URL user, else ssh-config `User`, else `$USER`.
    pub user: String,
    /// Candidate private keys: the ssh-config `IdentityFile` entries with their `%`
    /// tokens expanded, or the OpenSSH defaults in OpenSSH's order when the config names
    /// none. The `_sk` defaults are left out because russh cannot sign with a
    /// security-key file; an agent-held security key is still offered as an agent key.
    /// Existence is checked at use.
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
    /// The ssh-agent to ask for keys: ssh-config `IdentityAgent`, else `$SSH_AUTH_SOCK`.
    pub identity_agent: AgentSocket,
}

/// Which ssh-agent to ask for keys, as ssh-config `IdentityAgent` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::actions::remote) enum AgentSocket {
    /// The socket `$SSH_AUTH_SOCK` names, read when connecting: the default, and
    /// `IdentityAgent SSH_AUTH_SOCK` or `$SSH_AUTH_SOCK`.
    Env,
    /// A socket path: `IdentityAgent <path>`, or `$VAR` with `VAR` set.
    Path(PathBuf),
    /// No agent: `IdentityAgent none`, or `$VAR` with `VAR` unset, as in OpenSSH.
    None,
}

/// The global files OpenSSH reads after the user's (`GlobalKnownHostsFile` default).
const SYSTEM_KNOWN_HOSTS: [&str; 2] = ["/etc/ssh/ssh_known_hosts", "/etc/ssh/ssh_known_hosts2"];

/// Resolves `url` (an ssh URL, `ssh://` or scp-like) into a dialable target.
///
/// # Errors
/// [`ActionError::Git`] when the URL lacks a host, when no user can be determined, when
/// an existing ssh config file cannot be read or parsed, when the config routes the host
/// through `ProxyJump`/`ProxyCommand` or a `Match` block that may apply to it, or when a
/// `HostName`, `IdentityFile` or `IdentityAgent` value uses a `%` token rc-git cannot
/// expand.
pub(in crate::actions::remote) fn resolve(
    url: &gix::Url,
    ssh_dir: Option<&Path>,
    op: &'static str,
) -> ActionResult<SshTarget> {
    let host = url
        .host()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| ActionError::git(op, redacted(url), "the ssh URL has no host"))?
        .to_owned();
    // A caller-chosen ssh directory (tests, `--ssh-dir`) stands in for the whole
    // machine's ssh setup, so the system files are read only for the default one.
    let system_files = ssh_dir.is_none();
    let ssh_dir = match ssh_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_ssh_dir(op)?,
    };
    let settings = load_config(&ssh_dir, &host, op, url)?.unwrap_or_default();
    refuse_proxy(&settings.config, &host, op, url)?;
    let config = settings.config;
    let user = url
        .user()
        .map(str::to_owned)
        .or_else(|| config.user.clone())
        .or_else(|| std::env::var("USER").ok())
        .filter(|user| !user.is_empty())
        .ok_or_else(|| {
            ActionError::git(
                op,
                redacted(url),
                "no ssh user: none in the URL, the ssh config, or $USER",
            )
        })?;
    let port = url.port.or(config.port).unwrap_or(22);
    let host_name = match config.hostname.as_deref() {
        Some(value) => expand_tokens(value, |token| (token == 'h').then(|| host.clone()))
            .map_err(|token| token_err(op, url, "HostName", &token))?
            .to_ascii_lowercase(),
        None => host.clone(),
    };
    let known_hosts = known_hosts_files(
        config.user_known_hosts_file.as_deref(),
        &ssh_dir,
        &host_name,
        system_files,
    );
    let paths = PathTokens {
        host_name: &host_name,
        user: &user,
        op,
        url,
    };
    let identity_agent = agent_socket(settings.identity_agent.as_deref(), &paths)?;
    // OpenSSH falls back to the default keys only when no `IdentityFile` applies; a
    // default key tried after the configured one can log in as another account.
    let mut identities = Vec::new();
    for file in config.identity_file.unwrap_or_default() {
        identities.push(paths.expand(&file.to_string_lossy(), "IdentityFile")?);
    }
    let identities_configured = !identities.is_empty();
    if !identities_configured {
        identities = ["id_rsa", "id_ecdsa", "id_ed25519"]
            .into_iter()
            .map(|name| ssh_dir.join(name))
            .collect();
    }
    Ok(SshTarget {
        host,
        host_name,
        port,
        user,
        identities,
        identities_configured,
        identities_only: settings.identities_only,
        host_key_alias: settings.host_key_alias,
        known_hosts,
        identity_agent,
    })
}

/// Refuses a host the config routes through a jump host or a proxy command: rc-git
/// dials directly and never spawns, so it would time out or fail DNS without saying why.
/// `none` disables either directive, as in OpenSSH.
fn refuse_proxy(
    config: &russh_config::HostConfig,
    host: &str,
    op: &'static str,
    url: &gix::Url,
) -> ActionResult<()> {
    for (directive, value) in [
        ("ProxyJump", &config.proxy_jump),
        ("ProxyCommand", &config.proxy_command),
    ] {
        let set = value
            .as_deref()
            .is_some_and(|value| !strip_matching_quotes(value.trim()).eq_ignore_ascii_case("none"));
        if set {
            return Err(ActionError::git(
                op,
                redacted(url),
                format!(
                    "ssh config sets {directive} for {host}; rc-git cannot tunnel, \
                     use git for this remote"
                ),
            ));
        }
    }
    Ok(())
}

/// What the `%` tokens of a path-valued directive (`IdentityFile`, `IdentityAgent`)
/// stand for: `%d` the local home, `%u` the local user, `%h` the dialed host, `%r` the
/// remote user, `%%` a percent sign.
struct PathTokens<'a> {
    host_name: &'a str,
    user: &'a str,
    op: &'static str,
    url: &'a gix::Url,
}

impl PathTokens<'_> {
    /// `value` unquoted, with a leading `~` and its `%` tokens expanded.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `directive` when `value` uses a token other than the
    /// five above, or one whose value is unknown (no home, no `$USER`).
    fn expand(&self, value: &str, directive: &str) -> ActionResult<PathBuf> {
        let value = strip_matching_quotes(value.trim());
        let home = std::env::home_dir().map(|home| home.to_string_lossy().into_owned());
        let value = match (value.strip_prefix('~'), &home) {
            (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                format!("{home}{rest}")
            }
            _ => value.to_owned(),
        };
        let expanded = expand_tokens(&value, |token| match token {
            'd' => home.clone(),
            'u' => std::env::var("USER").ok(),
            'h' => Some(self.host_name.to_owned()),
            'r' => Some(self.user.to_owned()),
            _ => None,
        })
        .map_err(|token| token_err(self.op, self.url, directive, &token))?;
        Ok(PathBuf::from(expanded))
    }
}

/// The agent `IdentityAgent` names, read the way OpenSSH's `ssh.c` does: `none`, the
/// literal `SSH_AUTH_SOCK`, `$VAR` (an unset variable means no agent), else a path.
///
/// # Errors
/// [`ActionError::Git`] when `$VAR` is not a valid variable name or a path uses a `%`
/// token [`PathTokens::expand`] cannot expand.
fn agent_socket(value: Option<&str>, paths: &PathTokens<'_>) -> ActionResult<AgentSocket> {
    let Some(value) = value else {
        return Ok(AgentSocket::Env);
    };
    let value = strip_matching_quotes(value.trim());
    if value == "none" {
        return Ok(AgentSocket::None);
    }
    let variable = match value.strip_prefix('$') {
        Some(name) => name,
        None if value == "SSH_AUTH_SOCK" => return Ok(AgentSocket::Env),
        None => return paths.expand(value, "IdentityAgent").map(AgentSocket::Path),
    };
    if variable.is_empty()
        || !variable
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ActionError::git(
            paths.op,
            redacted(paths.url),
            format!("ssh config IdentityAgent '{value}' names no valid environment variable"),
        ));
    }
    if variable == "SSH_AUTH_SOCK" {
        return Ok(AgentSocket::Env);
    }
    Ok(match std::env::var_os(variable) {
        Some(path) if !path.is_empty() => AgentSocket::Path(PathBuf::from(path)),
        _ => AgentSocket::None,
    })
}

/// `value` with each `%x` token replaced by `lookup(x)` and `%%` by `%`. The error is the
/// first token `lookup` has no value for, as written (`%C`, or `%` at the end).
fn expand_tokens(value: &str, lookup: impl Fn(char) -> Option<String>) -> Result<String, String> {
    let mut expanded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            expanded.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => expanded.push('%'),
            Some(token) => match lookup(token) {
                Some(value) => expanded.push_str(&value),
                None => return Err(format!("%{token}")),
            },
            None => return Err("%".to_owned()),
        }
    }
    Ok(expanded)
}

/// The error for a `%` token rc-git cannot expand in `directive`.
fn token_err(op: &'static str, url: &gix::Url, directive: &str, token: &str) -> ActionError {
    ActionError::git(
        op,
        redacted(url),
        format!(
            "ssh config {directive} uses the token '{token}', which rc-git cannot expand; \
             use git for this remote"
        ),
    )
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
#[derive(Default)]
struct HostSettings {
    config: russh_config::HostConfig,
    identities_only: bool,
    host_key_alias: Option<String>,
    /// The raw `IdentityAgent` value, read by [`agent_socket`].
    identity_agent: Option<String>,
}

/// Parses `<ssh_dir>/config`, with the files it `Include`s, for `host`. `None` when the
/// file does not exist; an unreadable or unparseable file is an error rather than a
/// silently wrong connection.
///
/// russh-config 0.58.0 parses a subset of `ssh_config(5)`, so [`ConfigReader`] first
/// rewrites the file into that subset: it splits `Key=Value` and tab separators, follows
/// `Include`, turns `Match` into a block that never matches (or refuses one that may
/// apply), keeps the first value of each scalar directive within a block, unquotes, and
/// lowercases `Host` patterns. Global options before the first `Host` block get a
/// `Host *` header ([`parse_for_host`]).
///
/// OpenSSH matches `Host` patterns case-sensitively against the host as typed and
/// lowercases only afterwards. gix has already lowercased the URL's host, so rc-git
/// lowercases both sides — a deliberate superset of OpenSSH's matching. Matching exactly
/// would need the raw host spelling from `.git/config`; that is worth doing only if the
/// superset ever picks a different block than ssh does (two blocks that differ only in
/// case).
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
    let host = host.to_lowercase();
    let mut reader = ConfigReader {
        ssh_dir,
        host: &host,
        op,
        url,
        out: String::new(),
        header: None,
        active: true,
        headers: 0,
        seen: Vec::new(),
    };
    reader.read_file(&path, 0)?;
    let contents = reader.out;
    let parse = |contents: &str| {
        parse_for_host(contents, &host).map_err(|source| {
            ActionError::git(
                op,
                redacted(url),
                format!("cannot parse '{}': {source}", path.display()),
            )
        })
    };
    // Only `host_config` carries what the config file said: `russh_config::parse`
    // leaves `Config::host_name` as the queried alias and `user`/`port` as `None`.
    let config = parse(&contents)?.host_config;
    let directive =
        |keyword| parse(&directive_as_user(&contents, keyword)).map(|c| c.host_config.user);
    let identities_only = directive("identitiesonly")?.is_some_and(|value| {
        let value = strip_matching_quotes(&value);
        value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("true")
    });
    let host_key_alias = directive("hostkeyalias")?
        .map(|alias| alias.trim().to_lowercase())
        .filter(|alias| !alias.is_empty());
    let identity_agent = directive("identityagent")?;
    Ok(Some(HostSettings {
        config,
        identities_only,
        host_key_alias,
        identity_agent,
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

/// russh-config drops `IdentitiesOnly`, `HostKeyAlias` and `IdentityAgent`. This view
/// keeps only the `Host` lines and rewrites each line whose keyword is `keyword`
/// (lowercase) as `User`, so the same parser resolves it with the same block matching
/// as every other directive; the resulting `user` field holds that directive's value.
/// [`ConfigReader`] has already kept only the first value within a block, and the
/// parser keeps the first across blocks, so the first value in file order wins.
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

/// The directives whose quoted value OpenSSH unquotes and russh-config keeps verbatim.
const UNQUOTED_KEYWORDS: [&str; 4] = ["hostname", "user", "port", "identitiesonly"];

/// The single-valued directives rc-git reads. OpenSSH keeps the first value it obtains;
/// russh-config overwrites within a block, so later duplicates are dropped.
/// `IdentityFile` is cumulative and stays out.
const SCALAR_KEYWORDS: [&str; 11] = [
    "user",
    "hostname",
    "port",
    "proxyjump",
    "proxycommand",
    "userknownhostsfile",
    "stricthostkeychecking",
    "addkeystoagent",
    "identitiesonly",
    "hostkeyalias",
    "identityagent",
];

/// OpenSSH's limit on nested `Include`s (`READCONF_MAX_DEPTH`).
const MAX_INCLUDE_DEPTH: usize = 16;

/// The header a neutralized `Match` block gets: a pattern no host can match.
const NEVER_MATCHES: &str = "host !*";

/// Flattens an ssh config and the files it includes into the single-space,
/// one-`Host`-scope subset russh-config parses, for one queried host. Reads files only.
struct ConfigReader<'a> {
    ssh_dir: &'a Path,
    /// The queried host, lowercased.
    host: &'a str,
    op: &'static str,
    url: &'a gix::Url,
    out: String,
    /// The current block's header line as written to `out`; `None` before the first.
    header: Option<String>,
    /// Whether the current block applies to `host`.
    active: bool,
    /// Header lines written so far, to tell whether an included file opened a block.
    headers: usize,
    /// The [`SCALAR_KEYWORDS`] the current block has already set.
    seen: Vec<&'static str>,
}

impl ConfigReader<'_> {
    fn err(&self, message: String) -> ActionError {
        ActionError::git(self.op, redacted(self.url), message)
    }

    /// Appends `path`'s lines, `depth` includes below the top-level config.
    fn read_file(&mut self, path: &Path, depth: usize) -> ActionResult<()> {
        let contents = std::fs::read_to_string(path)
            .map_err(|source| self.err(format!("cannot read '{}': {source}", path.display())))?;
        for line in contents.lines() {
            self.line(line, depth)?;
        }
        Ok(())
    }

    fn line(&mut self, line: &str, depth: usize) -> ActionResult<()> {
        let Some((key, value)) = split_directive(line) else {
            return Ok(());
        };
        let keyword = key.to_ascii_lowercase();
        match keyword.as_str() {
            "host" => {
                let patterns = value.to_lowercase();
                let active = header_matches(&patterns, self.host);
                self.open_block(format!("host {patterns}"), active);
            }
            "match" => {
                if match_may_apply(&value, self.host) {
                    return Err(self.err(format!(
                        "ssh config 'Match {value}' may apply to {}; rc-git does not \
                         evaluate Match blocks, use git for this remote",
                        self.host
                    )));
                }
                self.open_block(NEVER_MATCHES.to_owned(), false);
            }
            "include" => return self.include(&value, depth),
            _ => self.directive(&keyword, &value),
        }
        Ok(())
    }

    fn open_block(&mut self, header: String, active: bool) {
        self.out.push_str(&header);
        self.out.push('\n');
        self.header = Some(header);
        self.active = active;
        self.headers += 1;
        self.seen.clear();
    }

    fn directive(&mut self, keyword: &str, value: &str) {
        if let Some(scalar) = SCALAR_KEYWORDS.iter().find(|scalar| **scalar == keyword) {
            if self.seen.contains(scalar) {
                return;
            }
            self.seen.push(scalar);
        }
        let value = if UNQUOTED_KEYWORDS.contains(&keyword) {
            strip_matching_quotes(value)
        } else {
            value
        };
        self.out.push_str(keyword);
        self.out.push(' ');
        self.out.push_str(value);
        self.out.push('\n');
    }

    /// Splices the files an `Include` names in place, as OpenSSH reads them: relative
    /// paths under the ssh directory, glob patterns expanded in sorted order, a missing
    /// file skipped. An `Include` inside a block that does not apply is not read at all.
    /// A block an included file opens ends with that file, so the including block's
    /// header is written again for the lines after the `Include`.
    fn include(&mut self, value: &str, depth: usize) -> ActionResult<()> {
        if !self.active {
            return Ok(());
        }
        if depth >= MAX_INCLUDE_DEPTH {
            return Err(self.err(format!(
                "ssh config Include '{value}' nests deeper than {MAX_INCLUDE_DEPTH} files"
            )));
        }
        let (header, active, headers) = (self.header.clone(), self.active, self.headers);
        for word in value.split_whitespace() {
            let pattern = self.include_path(strip_matching_quotes(word))?;
            let files = glob_files(&pattern).map_err(|source| {
                self.err(format!(
                    "cannot list Include '{}': {source}",
                    pattern.display()
                ))
            })?;
            for file in files {
                self.read_file(&file, depth + 1)?;
            }
        }
        if self.headers != headers {
            self.open_block(header.unwrap_or_else(|| "host *".to_owned()), active);
        }
        Ok(())
    }

    /// An `Include` argument as an absolute path: `~` is the home directory, and a
    /// relative path lives in the ssh directory (`ssh_config(5)`).
    fn include_path(&self, word: &str) -> ActionResult<PathBuf> {
        let Some(rest) = word.strip_prefix('~') else {
            return Ok(self.ssh_dir.join(word));
        };
        let rest = match rest.strip_prefix('/') {
            Some(rest) => rest,
            None if rest.is_empty() => rest,
            None => return Ok(self.ssh_dir.join(word)),
        };
        std::env::home_dir()
            .map(|home| home.join(rest))
            .ok_or_else(|| self.err(format!("no home directory to expand Include '{word}'")))
    }
}

/// One config line as `(keyword, value)`, split the way OpenSSH does: the keyword ends
/// at whitespace or `=`, and whitespace with at most one `=` separates it from the
/// value. Tabs inside the value become spaces. `None` for a blank line, a comment, or a
/// line with no value.
fn split_directive(line: &str) -> Option<(&str, String)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c == '=' || c.is_ascii_whitespace())
        .unwrap_or(line.len());
    let (key, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let value = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value.replace('\t', " ")))
}

/// Whether a `Host` header with these (lowercased) patterns applies to `host`, decided
/// by russh-config's own matcher so it agrees with the parse.
fn header_matches(patterns: &str, host: &str) -> bool {
    // The header comes first, so the parse cannot fail with `HostNotFound`; any error
    // reads as "no match", which only skips an `Include` that could not apply.
    russh_config::parse(&format!("host {patterns}\nuser matched\n"), host)
        .is_ok_and(|config| config.host_config.user.is_some())
}

/// Whether a `Match` line may select `host`: it uses `all` or `exec` (rc-git cannot
/// evaluate a command), or a `host`/`originalhost` criterion that holds. Other criteria
/// do not depend on the host and are not evaluated.
fn match_may_apply(criteria: &str, host: &str) -> bool {
    let mut words = criteria.split_whitespace();
    while let Some(word) = words.next() {
        let (negated, criterion) = word
            .strip_prefix('!')
            .map_or((false, word), |rest| (true, rest));
        match criterion.to_ascii_lowercase().as_str() {
            "all" | "exec" => return true,
            "canonical" | "final" => {}
            "host" | "originalhost" => {
                let patterns = words.next().unwrap_or_default().replace(',', " ");
                if header_matches(&patterns.to_lowercase(), host) != negated {
                    return true;
                }
            }
            _ => {
                words.next();
            }
        }
    }
    false
}

/// The files an absolute `pattern` names, as glob(3) expands an `Include` argument:
/// `*`, `?` and `[...]` in any component, a leading `.` matched only literally, results
/// sorted. Only regular files are kept, so a pattern naming nothing yields no file.
///
/// # Errors
/// The I/O error when a directory a wildcard component searches cannot be listed.
fn glob_files(pattern: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut candidates = vec![PathBuf::new()];
    for component in pattern.components() {
        let part = component.as_os_str();
        let Some(wildcard) = part.to_str().filter(|part| part.contains(['*', '?', '['])) else {
            for candidate in &mut candidates {
                candidate.push(part);
            }
            continue;
        };
        let mut matched = Vec::new();
        for dir in &candidates {
            if !dir.is_dir() {
                continue;
            }
            let mut names = Vec::new();
            for entry in std::fs::read_dir(dir)? {
                let name = entry?.file_name();
                if name
                    .to_str()
                    .is_some_and(|name| wildcard_matches(wildcard, name))
                {
                    names.push(name);
                }
            }
            names.sort();
            matched.extend(names.into_iter().map(|name| dir.join(name)));
        }
        candidates = matched;
    }
    candidates.retain(|candidate| candidate.is_file());
    Ok(candidates)
}

/// Whether the file name `name` matches the glob component `pattern`.
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, &name)
}

fn matches_from(pattern: &[char], name: &[char]) -> bool {
    let Some((&first, rest)) = pattern.split_first() else {
        return name.is_empty();
    };
    let class = if first == '[' { char_class(rest) } else { None };
    match (first, class) {
        ('*', _) => {
            (0..=name.len()).any(|skip| matches_from(rest, name.get(skip..).unwrap_or_default()))
        }
        ('?', _) => name
            .split_first()
            .is_some_and(|(_, tail)| matches_from(rest, tail)),
        (_, Some((class, after))) => name
            .split_first()
            .is_some_and(|(&c, tail)| class.contains(c) && matches_from(after, tail)),
        (literal, None) => name
            .split_first()
            .is_some_and(|(&c, tail)| c == literal && matches_from(rest, tail)),
    }
}

/// A `[...]` bracket expression: its ranges, and whether `!` or `^` negates it.
struct CharClass {
    negated: bool,
    ranges: Vec<(char, char)>,
}

impl CharClass {
    fn contains(&self, c: char) -> bool {
        self.ranges
            .iter()
            .any(|(low, high)| (*low..=*high).contains(&c))
            != self.negated
    }
}

/// Parses the bracket expression after a `[`, returning it and the pattern after its
/// `]`; `None` when it never closes, so the `[` is literal.
fn char_class(pattern: &[char]) -> Option<(CharClass, &[char])> {
    let (negated, mut rest) = match pattern.split_first() {
        Some((&('!' | '^'), tail)) => (true, tail),
        _ => (false, pattern),
    };
    let mut ranges = Vec::new();
    loop {
        let (&c, tail) = rest.split_first()?;
        if c == ']' && !ranges.is_empty() {
            return Some((CharClass { negated, ranges }, tail));
        }
        match tail {
            ['-', high, after @ ..] if *high != ']' => {
                ranges.push((c, *high));
                rest = after;
            }
            _ => {
                ranges.push((c, c));
                rest = tail;
            }
        }
    }
}

/// `value` without one surrounding pair of matching `"` or `'` quotes.
fn strip_matching_quotes(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
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

    /// OpenSSH's flag parser takes `true` as well as `yes`, quoted or not.
    #[test]
    fn identities_only_accepts_true() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("config", b"Host example.com\n  IdentitiesOnly true\n");
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert!(target.identities_only);
    }

    #[test]
    fn identities_only_accepts_a_quoted_yes() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("config", b"Host example.com\n  IdentitiesOnly \"yes\"\n");
        let target = resolve(
            &url("ssh://git@example.com/x.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert!(target.identities_only);
    }

    /// OpenSSH strips the quotes from `HostName`, `User` and `Port`; dialing the quoted
    /// name would fail DNS.
    #[test]
    fn quoted_host_name_user_and_port_are_unquoted() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Host alias\n  HostName \"real.example\"\n  User 'worker'\n  Port \"2222\"\n",
        );
        let target =
            resolve(&url("ssh://alias/x.git"), Some(dir.path()), "fetch").expect("resolve");
        assert_eq!(target.host_name, "real.example");
        assert_eq!(target.user, "worker");
        assert_eq!(target.port, 2222);
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
    /// with the host gix already lowercased. OpenSSH matches the host as typed, which
    /// rc-git no longer has, so both sides are lowercased: a deliberate superset.
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
        assert_eq!(
            target.identities,
            ["id_rsa", "id_ecdsa", "id_ed25519"].map(|name| dir.path().join(name))
        );
        assert!(!target.identities_configured);
        assert!(!target.identities_only);
        assert_eq!(target.identity_agent, AgentSocket::Env);
    }

    fn resolve_with(config: &[u8], url_text: &str) -> ActionResult<SshTarget> {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("config", config);
        resolve(&url(url_text), Some(dir.path()), "fetch")
    }

    fn refusal(config: &[u8], url_text: &str) -> String {
        resolve_with(config, url_text)
            .expect_err("the config is refused")
            .to_string()
    }

    /// `Key=Value` and `Key = Value` are legal OpenSSH; russh-config splits on the first
    /// space and would read `= real.example` or drop the line.
    #[test]
    fn equals_separated_key_value_lines_are_honored() {
        let target = resolve_with(
            b"Host=alias\n  HostName=real.example\n  Port = 2222\n  User =worker\n  \
              IdentitiesOnly=yes\n",
            "ssh://alias/x.git",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "real.example");
        assert_eq!(target.port, 2222);
        assert_eq!(target.user, "worker");
        assert!(target.identities_only);
    }

    /// The shape `OrbStack` and Colima write: the alias lives only in an included file, named by
    /// a glob relative to the ssh directory, above the first `Host` block.
    #[test]
    fn include_resolves_an_alias_defined_only_in_an_included_file() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write(
            "config",
            b"Include config.d/*\n\nHost other.example\n  User other\n",
        );
        dir.write(
            "config.d/work",
            b"Host github.com-work\n  HostName github.com\n  User git\n",
        );
        dir.write("config.d/.hidden", b"Host github.com-work\n  Port 2200\n");
        let target = resolve(
            &url("ssh://github.com-work/team/repo.git"),
            Some(dir.path()),
            "fetch",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "github.com");
        assert_eq!(target.user, "git");
        assert_eq!(
            target.port, 22,
            "`*` does not match a dot file, as in glob(3)"
        );
    }

    /// A block an included file opens ends with that file: the lines after the
    /// `Include` belong to the including block again.
    #[test]
    fn include_inside_a_block_returns_to_that_block() {
        let dir = rc_git_testkit::Fixture::empty_dir();
        dir.write("config", b"Host alias\n  Include extra\n  Port 2200\n");
        dir.write(
            "extra",
            b"HostName real.example\nHost other\n  User other\n",
        );
        let target =
            resolve(&url("ssh://git@alias/x.git"), Some(dir.path()), "fetch").expect("resolve");
        assert_eq!(target.host_name, "real.example");
        assert_eq!(target.port, 2200);
        assert_eq!(target.user, "git");
    }

    /// A missing include is skipped, as OpenSSH does; one inside a block that does not
    /// apply is never read, so its self-reference cannot recurse.
    #[test]
    fn include_skips_missing_files_and_blocks_that_do_not_apply() {
        let target = resolve_with(
            b"Include nowhere/* absent\nHost other\n  Include config\nHost alias\n  \
              HostName real.example\n",
            "ssh://git@alias/x.git",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "real.example");
    }

    #[test]
    fn include_recursion_stops_at_openssh_depth() {
        let message = refusal(b"Include config\n", "ssh://git@alias/x.git");
        assert!(message.contains("deeper than 16"), "{message}");
    }

    #[test]
    fn include_glob_matches_classes_and_question_marks() {
        assert!(wildcard_matches("w[n-p]rk?", "work1"));
        assert!(wildcard_matches("[!x]*", "abc"));
        assert!(!wildcard_matches("[!a]*", "abc"));
        assert!(
            wildcard_matches("[a", "[a"),
            "an unclosed bracket is literal"
        );
        assert!(wildcard_matches(".*", ".hidden"));
        assert!(!wildcard_matches("*", ".hidden"));
    }

    /// A `Match` block's directives must not leak into the `Host` block above it.
    #[test]
    fn match_block_directives_do_not_leak_into_the_host_block_above() {
        let target = resolve_with(
            b"Host github.com-work\n  HostName github.com\n  User git\n\
              Match host *.internal\n  HostName %h.corp.example\n  User admin\n  \
              IdentityFile ./admin_key\n",
            "ssh://github.com-work/x.git",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "github.com");
        assert_eq!(target.user, "git");
        assert!(!target.identities_configured, "{:?}", target.identities);
    }

    /// A `Match` that may select the host is refused by name rather than ignored.
    #[test]
    fn match_block_that_may_apply_is_refused() {
        for config in [
            &b"Match host *.internal\n  User admin\n"[..],
            b"Match originalhost db.*,other\n  User admin\n",
            b"Match !host other.example\n  User admin\n",
            b"Match all\n  User admin\n",
            b"Match exec \"true\"\n  User admin\n",
        ] {
            let message = refusal(config, "ssh://db.internal/x.git");
            assert!(message.contains("Match"), "{message}");
        }
        let target = resolve_with(
            b"Match host other.example user admin\n  User admin\n",
            "ssh://git@db.internal/x.git",
        )
        .expect("a Match for another host is neutralized");
        assert_eq!(target.user, "git");
    }

    #[test]
    fn proxy_jump_and_proxy_command_are_refused() {
        let message = refusal(
            b"Host inner\n  ProxyJump bastion.example\n",
            "ssh://git@inner/x.git",
        );
        assert!(message.contains("sets ProxyJump for inner"), "{message}");
        let message = refusal(
            b"Host inner\n  ProxyCommand nc %h %p\n",
            "ssh://git@inner/x.git",
        );
        assert!(message.contains("sets ProxyCommand for inner"), "{message}");
        resolve_with(
            b"Host inner\n  ProxyJump none\nHost *\n  ProxyJump bastion.example\n",
            "ssh://git@inner/x.git",
        )
        .expect("`ProxyJump none` comes first and disables the jump");
    }

    /// OpenSSH keeps the first value within a block too; russh-config keeps the last.
    #[test]
    fn duplicate_keys_in_a_block_resolve_to_the_first_value() {
        let target = resolve_with(
            b"Host alias\n  User first\n  User second\n  IdentitiesOnly yes\n  \
              IdentitiesOnly no\n",
            "ssh://alias/x.git",
        )
        .expect("resolve");
        assert_eq!(target.user, "first");
        assert!(target.identities_only);
    }

    #[test]
    fn host_name_expands_percent_h() {
        let target = resolve_with(
            b"Host *.internal\n  HostName %h.Corp.Example\n",
            "ssh://git@db.internal/x.git",
        )
        .expect("resolve");
        assert_eq!(target.host_name, "db.internal.corp.example");
        let message = refusal(
            b"Host alias\n  HostName %p.example\n",
            "ssh://git@alias/x.git",
        );
        assert!(message.contains("'%p'"), "{message}");
    }

    #[test]
    fn identity_file_tokens_expand_and_unknown_ones_are_refused() {
        let local = std::env::var("USER").expect("USER");
        let target = resolve_with(
            b"Host alias\n  HostName real.example\n  IdentityFile /keys/%h/%r/%u_100%%\n",
            "ssh://git@alias/x.git",
        )
        .expect("resolve");
        assert_eq!(
            target.identities,
            [PathBuf::from(format!(
                "/keys/real.example/git/{local}_100%"
            ))]
        );
        let message = refusal(
            b"Host alias\n  IdentityFile ~/.ssh/%C\n",
            "ssh://git@alias/x.git",
        );
        assert!(
            message.contains("IdentityFile uses the token '%C'"),
            "{message}"
        );
    }

    fn identity_agent(value: &str) -> ActionResult<AgentSocket> {
        resolve_with(
            format!("Host alias\n  IdentityAgent {value}\n").as_bytes(),
            "ssh://git@alias/x.git",
        )
        .map(|target| target.identity_agent)
    }

    #[test]
    fn identity_agent_none_means_no_agent() {
        assert_eq!(identity_agent("none").expect("resolve"), AgentSocket::None);
    }

    #[test]
    fn identity_agent_ssh_auth_sock_means_the_environment() {
        assert_eq!(
            identity_agent("SSH_AUTH_SOCK").expect("resolve"),
            AgentSocket::Env
        );
        assert_eq!(
            identity_agent("$SSH_AUTH_SOCK").expect("resolve"),
            AgentSocket::Env
        );
    }

    #[test]
    fn identity_agent_dollar_names_an_environment_variable() {
        let home = std::env::var_os("HOME").expect("HOME");
        assert_eq!(
            identity_agent("$HOME").expect("resolve"),
            AgentSocket::Path(PathBuf::from(home))
        );
        assert_eq!(
            identity_agent("$RC_GIT_TEST_UNSET_AGENT_VARIABLE").expect("resolve"),
            AgentSocket::None,
            "an unset variable means no agent, as in OpenSSH"
        );
        assert!(identity_agent("${HOME}/agent.sock").is_err());
    }

    /// The 1Password shape: a quoted path with a space under the home directory.
    #[test]
    fn identity_agent_path_is_unquoted_and_tilde_expanded() {
        let home = std::env::home_dir().expect("home");
        assert_eq!(
            identity_agent("\"~/Group Containers/agent.sock\"").expect("resolve"),
            AgentSocket::Path(home.join("Group Containers/agent.sock"))
        );
        assert_eq!(
            identity_agent("/run/%r.sock").expect("resolve"),
            AgentSocket::Path(PathBuf::from("/run/git.sock"))
        );
    }

    #[test]
    fn identity_agent_takes_the_first_value() {
        let target = resolve_with(
            b"Host alias\n  IdentityAgent none\n  IdentityAgent /run/agent.sock\n\
              Host *\n  IdentityAgent /run/other.sock\n",
            "ssh://git@alias/x.git",
        )
        .expect("resolve");
        assert_eq!(target.identity_agent, AgentSocket::None);
    }

    #[test]
    fn quoted_command_single_quotes_the_path() {
        let command = quoted_command(
            gix::protocol::transport::Service::ReceivePack,
            gix::bstr::BStr::new("repo.git"),
            "push",
        )
        .expect("quote");
        assert_eq!(command, "git-receive-pack 'repo.git'");
    }

    /// A byte that cannot travel inside single quotes, or a path that is not UTF-8,
    /// is refused rather than sent to the server.
    #[test]
    fn quoted_command_refuses_unquotable_paths() {
        let paths: [&[u8]; 4] = [b"it's.git", b"repo\n.git", b"repo\0.git", b"repo\xff.git"];
        for path in paths {
            let path = gix::bstr::BStr::new(path);
            let result =
                quoted_command(gix::protocol::transport::Service::UploadPack, path, "fetch");
            assert!(
                matches!(&result, Err(ActionError::Git { op: "fetch", .. })),
                "{path:?}: {result:?}"
            );
        }
    }
}
