//! `rc-git`: the command-line face of `rc-git-core`.
//!
//! Output is tab-separated so scripts and agents can parse it. `--time` prints how long the
//! repository open and the command took, which is how the budgets in `docs/performance.md`
//! are checked without the GUI.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rc_git_core::{
    BlobContent, BlobDiff, ChangeKind, CommitFilter, CommitPage, CommitSummary, DEFAULT_BLOB_LIMIT,
    DEFAULT_DIFF_CONTEXT, DiffLineKind, EntryKind, LineEnd, Oid, RefKind, Repository, TreeEntry,
    UnreadableEntry, WorktreeStatus,
};
use rc_git_guard::actions::{ActionError, Cancel};

const USAGE: &str = "\
usage: rc-git [--time] <command> <repo> [args]

  head    <repo>                          branch and commit HEAD points to
  refs    <repo>                          every branch, remote branch, and tag
  ls      <repo> [rev] [path]             entries of a directory at a revision (default HEAD)
  cat     <repo> <rev> <path> [limit]     file contents; limit in bytes, default 8 MiB
  log     <repo> [rev] [-n N] [--skip N]  commits, newest first (default 20)
  filelog <repo> <path> [rev] [-n N] [--max-scan N]  commits that changed a path
  search  <repo> [rev] [--message S] [--author S] [--since EPOCH] [--until EPOCH] [-n N]
          [--max-scan N]                  commits matching every given filter
  show    <repo> <rev>                    one commit: header lines, then the message
  diff    <repo> <rev>                    changed files of one commit against its first parent;
                                          a root commit lists every file as added
  diff    <repo> <old> <new> [path]       changed files between two revisions, or one file's hunks
  diff    <repo> --worktree <path>        one working-tree file's hunks against HEAD
  blame   <repo> <rev> <path>             which commit introduced each line
  status  <repo> [-z] [dir]               one working-tree directory against HEAD
  changes <repo> [-z] [--max-scan N]      every changed working-tree path against HEAD
  files   <repo> [-z] [--max-scan N]      every file on disk, ignored folders skipped
  stash   <repo>                          stash entries, newest first
  resolve <repo> <rev>                    object id a revision names
  default-branch <repo>                   the mainline branch: a remote's HEAD, else master/main

  --max-scan N stops after examining N commits (filelog, search; default: all history) or N
  working-tree entries (changes, files; default 200000) and warns that the output is
  truncated. filelog and search reject N = 0.

  commit      <repo> -m <msg> [--author \"Name <email>\"] [--confirm <phrase>]
              [--expect-head <oid>] <path>...
                                          commit files to the branch; with --expect-head,
                                          refuse if HEAD is no longer that full commit id
  stash-push  <repo> --yes [-m <msg>] [--author \"Name <email>\"] [--confirm <phrase>] <path>...
                                          stash files, restore them to HEAD
  stash-apply <repo> [n] [--confirm <phrase>]  write stash@{n} back to the working tree
  stash-pop   <repo> --yes [n] [--confirm <phrase>]  apply stash@{n}, then drop it
  stash-drop  <repo> [n] --confirm drop   delete stash@{n} without applying
  discard     <repo> --yes <path>...      restore files to HEAD, delete untracked; no undo
                                          (--confirm discard works in place of --yes)
  checkout    <repo> <branch>             switch the working tree and HEAD to a local branch

  fetch <repo> [remote options]           update remote-tracking refs from the upstream remote
  pull  <repo> [remote options]           fetch, then fast-forward the current branch
  push  <repo> [remote options]           upload the current branch to its upstream
  sync  <repo> [remote options]           fetch, fast-forward, then push
  Remote options: --username U, --token T (HTTPS; the token may also come from
  RC_GIT_TOKEN, which keeps it out of process lists and is read only with --username
  or an HTTPS upstream), --accept-hostkey SHA256:... (trust this ssh host-key
  fingerprint), --ssh-dir DIR (default ~/.ssh). Over ssh, an encrypted key's
  passphrase comes from RC_GIT_KEY_PASSPHRASE, which no other transport reads.

  Paths are quoted the way git quotes them: a name holding a control character, '\"', '\\'
  or bytes that are not UTF-8 is wrapped in double quotes with C escapes (\\n, \\t, \\\\, \\\",
  octal \\ooo), so \"a\\nb\" is one file. -z (files, changes, status) ends each record with
  NUL instead of a newline and prints the path raw, unquoted, as the last field.
  A commit title is its first paragraph on one line, as git's %s; a control character in a
  title, name or message column prints as a space.

  An unknown flag or a surplus argument is a usage error. `--` ends the flags, so a path
  after it may start with '-'. -h or --help anywhere before `--` prints this text.

exit codes: 0 ok, 1 error, 2 usage, 3 binary file, 4 file over limit,
  5 authentication required, 6 ssh host key unverified (rerun with --accept-hostkey),
  7 diverged or push rejected (reconcile with git, e.g. git pull --rebase),
  130 interrupted: Ctrl+C or SIGTERM stopped fetch, pull, push or sync between steps
  (a second one exits at once, even mid-step)
";

const EXIT_ERROR: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_BINARY: u8 = 3;
const EXIT_TOO_LARGE: u8 = 4;
const EXIT_AUTH: u8 = 5;
const EXIT_HOSTKEY: u8 = 6;
const EXIT_DIVERGED: u8 = 7;
const EXIT_INTERRUPTED: u8 = 130;

/// A command line the verb cannot act on; exits with [`EXIT_USAGE`] instead of [`EXIT_ERROR`].
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn usage_error(message: String) -> anyhow::Error {
    UsageError(message).into()
}

/// One verb's arguments, split by the flags that verb declares.
struct VerbArgs<'a> {
    label: &'static str,
    values: Vec<(&'static str, &'a str)>,
    switches: Vec<&'static str>,
    positionals: Vec<&'a str>,
}

/// Splits `args` into flag values, switches and positionals. A flag in `valued` takes the next
/// word as its value; one in `switches` stands alone. Any other word starting with `-` is a
/// usage error naming it, and every word after `--` is positional.
fn split_args<'a>(
    label: &'static str,
    args: &'a [String],
    valued: &[&'static str],
    switches: &[&'static str],
) -> Result<VerbArgs<'a>> {
    let mut parsed = VerbArgs {
        label,
        values: Vec::new(),
        switches: Vec::new(),
        positionals: Vec::new(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            parsed.positionals.extend(iter.by_ref().map(String::as_str));
        } else if !arg.starts_with('-') || arg == "-" {
            parsed.positionals.push(arg);
        } else if let Some(flag) = valued.iter().find(|flag| **flag == arg) {
            let value = iter
                .next()
                .ok_or_else(|| usage_error(format!("{flag} needs a value\n\n{USAGE}")))?;
            parsed.values.push((flag, value));
        } else if let Some(flag) = switches.iter().find(|flag| **flag == arg) {
            parsed.switches.push(flag);
        } else {
            return Err(usage_error(format!(
                "{label} does not take '{arg}'\n\n{USAGE}"
            )));
        }
    }
    Ok(parsed)
}

impl<'a> VerbArgs<'a> {
    /// Fails unless there are `min..=max` positionals; `shape` names them in the message.
    fn expect(&self, min: usize, max: usize, shape: &str) -> Result<()> {
        let label = self.label;
        if self.positionals.len() < min {
            return Err(usage_error(format!("{label} needs {shape}\n\n{USAGE}")));
        }
        if let Some(surplus) = self.positionals.get(max) {
            return Err(usage_error(format!(
                "{label} takes {shape}; unexpected '{surplus}'\n\n{USAGE}"
            )));
        }
        Ok(())
    }

    fn positional(&self, index: usize) -> Option<&'a str> {
        self.positionals.get(index).copied()
    }

    /// The last value given for any of `names`.
    fn value(&self, names: &[&str]) -> Option<&'a str> {
        let mut found = None;
        for (flag, value) in &self.values {
            if names.contains(flag) {
                found = Some(*value);
            }
        }
        found
    }

    fn number(&self, flag: &str) -> Result<Option<usize>> {
        let Some(raw) = self.value(&[flag]) else {
            return Ok(None);
        };
        let parsed = raw
            .parse()
            .map_err(|_| usage_error(format!("{flag} value '{raw}' is not a number")))?;
        Ok(Some(parsed))
    }

    fn has(&self, switch: &str) -> bool {
        self.switches.contains(&switch)
    }
}

/// Spells a repository path one way: leading `/` and `./`, doubled slashes and `.` segments
/// dropped, so `./src//lib.rs` is `src/lib.rs` and `/` is the root (empty). A `..` segment is
/// a usage error, since no tree entry can carry that name.
/// The tree `HEAD` points at, or `None` on an unborn branch, read once so the caller compares
/// the working tree against one consistent `HEAD`.
fn head_tree(repo: &Repository) -> Result<Option<Oid>> {
    match repo.head()?.target {
        Some(commit) => Ok(Some(repo.tree_of(commit)?)),
        None => Ok(None),
    }
}

fn canonical_path(raw: &str) -> Result<String> {
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                return Err(UsageError(format!(
                    "path '{raw}' has a '..' segment; name it from the repository root"
                ))
                .into());
            }
            _ => segments.push(segment),
        }
    }
    Ok(segments.join("/"))
}

fn canonical_paths(raw: &[&str]) -> Result<Vec<String>> {
    raw.iter().map(|path| canonical_path(path)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Head,
    Refs,
    Ls,
    Cat,
    Log,
    Filelog,
    Search,
    Show,
    Diff,
    Blame,
    Status,
    Changes,
    Files,
    Stash,
    Resolve,
    DefaultBranch,
    Commit,
    StashPush,
    StashApply,
    StashPop,
    StashDrop,
    Discard,
    Checkout,
    Fetch,
    Pull,
    Push,
    Sync,
}

impl Verb {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "head" => Some(Self::Head),
            "refs" => Some(Self::Refs),
            "ls" => Some(Self::Ls),
            "cat" => Some(Self::Cat),
            "log" => Some(Self::Log),
            "filelog" => Some(Self::Filelog),
            "search" => Some(Self::Search),
            "show" => Some(Self::Show),
            "diff" => Some(Self::Diff),
            "blame" => Some(Self::Blame),
            "status" => Some(Self::Status),
            "changes" => Some(Self::Changes),
            "files" => Some(Self::Files),
            "stash" => Some(Self::Stash),
            "resolve" => Some(Self::Resolve),
            "default-branch" => Some(Self::DefaultBranch),
            "commit" => Some(Self::Commit),
            "stash-push" => Some(Self::StashPush),
            "stash-apply" => Some(Self::StashApply),
            "stash-pop" => Some(Self::StashPop),
            "stash-drop" => Some(Self::StashDrop),
            "discard" => Some(Self::Discard),
            "checkout" => Some(Self::Checkout),
            "fetch" => Some(Self::Fetch),
            "pull" => Some(Self::Pull),
            "push" => Some(Self::Push),
            "sync" => Some(Self::Sync),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Refs => "refs",
            Self::Ls => "ls",
            Self::Cat => "cat",
            Self::Log => "log",
            Self::Filelog => "filelog",
            Self::Search => "search",
            Self::Show => "show",
            Self::Diff => "diff",
            Self::Blame => "blame",
            Self::Status => "status",
            Self::Changes => "changes",
            Self::Files => "files",
            Self::Stash => "stash",
            Self::Resolve => "resolve",
            Self::DefaultBranch => "default-branch",
            Self::Commit => "commit",
            Self::StashPush => "stash-push",
            Self::StashApply => "stash-apply",
            Self::StashPop => "stash-pop",
            Self::StashDrop => "stash-drop",
            Self::Discard => "discard",
            Self::Checkout => "checkout",
            Self::Fetch => "fetch",
            Self::Pull => "pull",
            Self::Push => "push",
            Self::Sync => "sync",
        }
    }
}

struct Invocation {
    time: bool,
    verb: Verb,
    repo: PathBuf,
    args: Vec<String>,
}

enum Command {
    Help,
    Run(Invocation),
}

fn main() -> ExitCode {
    let invocation = match parse(std::env::args_os().skip(1).collect()) {
        Ok(Command::Run(invocation)) => invocation,
        Ok(Command::Help) => {
            if let Err(err) = io::stdout().lock().write_all(USAGE.as_bytes()) {
                let _ = writeln!(io::stderr().lock(), "error: write usage to stdout: {err}");
                return ExitCode::from(EXIT_ERROR);
            }
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "error: {message}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match run(&invocation) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(io::stderr().lock(), "error: {}", error_line(&err));
            ExitCode::from(exit_code_for(&err))
        }
    }
}

/// The exit code a failed command ends with, per the table in [`USAGE`].
fn exit_code_for(err: &anyhow::Error) -> u8 {
    if err.downcast_ref::<UsageError>().is_some() {
        return EXIT_USAGE;
    }
    match err.downcast_ref::<ActionError>() {
        Some(ActionError::AuthRequired { .. } | ActionError::PassphraseRequired { .. }) => {
            EXIT_AUTH
        }
        Some(ActionError::UnknownHostKey { .. }) => EXIT_HOSTKEY,
        Some(ActionError::Diverged { .. } | ActionError::PushRejected { .. }) => EXIT_DIVERGED,
        Some(ActionError::Cancelled { .. }) => EXIT_INTERRUPTED,
        Some(_) | None => EXIT_ERROR,
    }
}

/// One cancel flag for every remote step of the command, set by SIGINT or SIGTERM so the guard
/// stops between steps and cleans up instead of the process dying with `.lock` and `tmp_pack_*`
/// files left in `.git`. A second signal, once the flag is set, exits at once with
/// [`EXIT_INTERRUPTED`]: some steps never look at the flag (the ssh dial, a blocked HTTPS read,
/// the pull's tree apply), and Ctrl+C must still end them.
fn interruptible_cancel() -> Result<Cancel> {
    let cancel = Cancel::new();
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        // Registered first so it runs first: it sees the flag still clear on the first signal.
        signal_hook::flag::register_conditional_shutdown(
            signal,
            i32::from(EXIT_INTERRUPTED),
            cancel.share(),
        )
        .with_context(|| format!("install the second-signal exit for signal {signal}"))?;
        signal_hook::flag::register(signal, cancel.share())
            .with_context(|| format!("install the cancel handler for signal {signal}"))?;
    }
    Ok(cancel)
}

/// Joins the error chain into one line, each cause once and without gix's source locations.
///
/// Core and guard errors already carry their cause in their message, and gix appends
/// `, at <file>:<line>` unless formatted with `{:#}`, so a frame whose text the line already
/// holds is skipped. Context causes such as a parse failure are kept.
fn error_line(err: &anyhow::Error) -> String {
    let mut line = String::new();
    for frame in err.chain() {
        let text = format!("{frame:#}");
        if line.contains(&text) {
            continue;
        }
        if !line.is_empty() {
            line.push_str(": ");
        }
        line.push_str(&text);
    }
    line
}

/// Splits argv into the verb, the repository path and the verb's arguments. The repository path
/// is kept as raw bytes; every other word must be UTF-8, since revisions, flag values and tree
/// paths are text to the core, so a word that is not is a usage error rather than a panic.
fn parse(mut args: Vec<OsString>) -> std::result::Result<Command, String> {
    let wants_help = args
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--help" || arg == "-h");
    if wants_help {
        return Ok(Command::Help);
    }
    let time = args.first().is_some_and(|a| a == "--time");
    if time {
        args.remove(0);
    }
    if args.len() < 2 {
        return Err("expected <command> <repo>".to_owned());
    }
    let command = utf8_arg(args.remove(0))?;
    let verb = Verb::parse(&command).ok_or_else(|| format!("unknown command '{command}'"))?;
    let repo = PathBuf::from(args.remove(0));
    let args = args
        .into_iter()
        .map(utf8_arg)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Command::Run(Invocation {
        time,
        verb,
        repo,
        args,
    }))
}

fn utf8_arg(arg: OsString) -> std::result::Result<String, String> {
    arg.into_string().map_err(|raw| {
        format!(
            "argument '{}' is not valid UTF-8; rc-git reads revisions, flags and paths as text",
            raw.to_string_lossy()
        )
    })
}

/// Writes the verb's output. A reader that closed early (`rc-git log | head`) is a normal end,
/// as it is for other Unix tools: the rest of the output is dropped without a message.
fn write_stdout(out: &[u8]) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(out).and_then(|()| stdout.flush()) {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result.context("write to stdout"),
    }
}

fn run(invocation: &Invocation) -> Result<ExitCode> {
    let opened_at = Instant::now();
    let repo = Repository::open(&invocation.repo)?;
    let open_elapsed = opened_at.elapsed();

    let started_at = Instant::now();
    let mut out = Vec::new();
    let code = match invocation.verb {
        Verb::Head => head(&repo, &invocation.args, &mut out)?,
        Verb::Refs => refs(&repo, &invocation.args, &mut out)?,
        Verb::Ls => ls(&repo, &invocation.args, &mut out)?,
        Verb::Cat => cat(&repo, &invocation.args, &mut out)?,
        Verb::Log => log(&repo, &invocation.args, &mut out)?,
        Verb::Filelog => filelog(&repo, &invocation.args, &mut out)?,
        Verb::Search => search(&repo, &invocation.args, &mut out)?,
        Verb::Show => show(&repo, &invocation.args, &mut out)?,
        Verb::Diff => diff(&repo, &invocation.args, &mut out)?,
        Verb::Blame => blame(&repo, &invocation.args, &mut out)?,
        Verb::Status => status(&repo, &invocation.args, &mut out)?,
        Verb::Changes => changes(&repo, &invocation.args, &mut out)?,
        Verb::Files => files(&repo, &invocation.args, &mut out)?,
        Verb::Stash => stash(&repo, &invocation.args, &mut out)?,
        Verb::Resolve => resolve(&repo, &invocation.args, &mut out)?,
        Verb::DefaultBranch => default_branch(&repo, &invocation.args, &mut out)?,
        Verb::Commit => commit_verb(&repo, &invocation.repo, &invocation.args, &mut out)?,
        Verb::StashPush => stash_push_verb(&invocation.repo, &invocation.args, &mut out)?,
        Verb::StashApply => stash_index_verb(
            &invocation.repo,
            &invocation.args,
            StashVerb::Apply,
            &mut out,
        )?,
        Verb::StashPop => {
            stash_index_verb(&invocation.repo, &invocation.args, StashVerb::Pop, &mut out)?
        }
        Verb::StashDrop => stash_index_verb(
            &invocation.repo,
            &invocation.args,
            StashVerb::Drop,
            &mut out,
        )?,
        Verb::Discard => discard_verb(&invocation.repo, &invocation.args)?,
        Verb::Checkout => checkout_verb(&invocation.repo, &invocation.args, &mut out)?,
        Verb::Fetch | Verb::Pull | Verb::Push | Verb::Sync => remote_verb(invocation, &mut out)?,
    };
    let command_elapsed = started_at.elapsed();

    write_stdout(&out)?;
    if invocation.time {
        writeln!(
            io::stderr().lock(),
            "time: open {:.2} ms, {} {:.2} ms",
            open_elapsed.as_secs_f64() * 1000.0,
            invocation.verb.name(),
            command_elapsed.as_secs_f64() * 1000.0
        )
        .context("write timing")?;
    }
    Ok(code)
}

/// Ends a record: a newline, or NUL under `-z`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Records {
    Lines,
    Nul,
}

impl Records {
    fn from_args(args: &VerbArgs<'_>) -> Self {
        if args.has("-z") {
            Self::Nul
        } else {
            Self::Lines
        }
    }

    /// Writes `path` as the last field of a record and ends the record.
    fn end_with_path(self, out: &mut Vec<u8>, path: &[u8]) {
        match self {
            Self::Lines => {
                write_path(out, path);
                out.push(b'\n');
            }
            Self::Nul => {
                out.extend_from_slice(path);
                out.push(0);
            }
        }
    }
}

/// Writes a path the way git's `core.quotePath` does: unchanged when it is UTF-8 with no control
/// character, `"` or `\`; otherwise in double quotes with C escapes and every other byte that
/// needs one in octal, so a newline in a name cannot split one record into two.
fn write_path(out: &mut Vec<u8>, path: &[u8]) {
    let plain = std::str::from_utf8(path).is_ok_and(|text| {
        !text
            .bytes()
            .any(|byte| byte < 0x20 || byte == 0x7f || byte == b'"' || byte == b'\\')
    });
    if plain {
        out.extend_from_slice(path);
        return;
    }
    let utf8_valid = std::str::from_utf8(path).is_ok();
    out.push(b'"');
    for &byte in path {
        match byte {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x20..=0x7e => out.push(byte),
            0x80..=0xff if utf8_valid => out.push(byte),
            _ => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    out.push(b'"');
}

fn head(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    split_args("head", args, &[], &[])?.expect(0, 0, "no arguments")?;
    let head = repo.head()?;
    let name = head.ref_name.as_deref().unwrap_or("detached");
    let target = head
        .target
        .map_or_else(|| "unborn".to_owned(), |oid| oid.to_hex());
    writeln!(out, "{name}\t{target}")?;
    Ok(ExitCode::SUCCESS)
}

fn default_branch(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    split_args("default-branch", args, &[], &[])?.expect(0, 0, "no arguments")?;
    let Some(branch) = repo.default_branch()? else {
        writeln!(io::stderr().lock(), "error: no default branch")?;
        return Ok(ExitCode::from(EXIT_ERROR));
    };
    writeln!(out, "{branch}")?;
    Ok(ExitCode::SUCCESS)
}

fn refs(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    split_args("refs", args, &[], &[])?.expect(0, 0, "no arguments")?;
    let listing = repo.refs()?;
    for entry in &listing.refs {
        let kind = match entry.kind {
            RefKind::LocalBranch => "branch",
            RefKind::RemoteBranch => "remote",
            RefKind::Tag => "tag",
            RefKind::Other => "other",
        };
        writeln!(
            out,
            "{kind}\t{}\t{}\t{}",
            entry.short_name, entry.target, entry.full_name
        )?;
    }
    for broken in &listing.broken {
        writeln!(
            io::stderr().lock(),
            "warning: {}: {}",
            broken.full_name,
            broken.error
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn ls(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("ls", args, &[], &[])?;
    args.expect(0, 2, "[rev] [path]")?;
    let rev = args.positional(0).unwrap_or("HEAD");
    let path = &canonical_path(args.positional(1).unwrap_or(""))?;
    let root = repo.resolve_tree(rev)?;
    let tree = if path.is_empty() {
        root
    } else {
        let entry = repo
            .entry_at_path(root, path)?
            .with_context(|| format!("'{path}' does not exist at {rev}"))?;
        if entry.kind != EntryKind::Tree {
            bail!("'{path}' is a file, not a directory");
        }
        entry.oid
    };
    for entry in repo.tree_entries(tree)? {
        write!(out, "{}\t{}\t", kind_name(entry.kind), entry.oid)?;
        Records::Lines.end_with_path(out, &entry.name);
    }
    Ok(ExitCode::SUCCESS)
}

/// The kind column `ls` and `status` print.
fn kind_name(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Tree => "tree",
        EntryKind::Blob => "blob",
        EntryKind::ExecutableBlob => "exec",
        EntryKind::Symlink => "link",
        EntryKind::Submodule => "submodule",
    }
}

/// The status column `status` and `changes` print.
fn status_letter(status: WorktreeStatus) -> &'static str {
    match status {
        WorktreeStatus::Unchanged => " ",
        WorktreeStatus::Modified => "M",
        WorktreeStatus::Untracked => "?",
        WorktreeStatus::Ignored => "!",
        WorktreeStatus::Deleted => "D",
    }
}

fn cat(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("cat", args, &[], &[])?;
    args.expect(2, 3, "<rev> <path> [limit]")?;
    let [rev, path, ..] = args.positionals[..] else {
        bail!("cat needs <rev> <path>\n\n{USAGE}");
    };
    let path = &canonical_path(path)?;
    let limit = match args.positional(2) {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| usage_error(format!("limit '{raw}' is not a number")))?,
        None => DEFAULT_BLOB_LIMIT,
    };
    let root = repo.resolve_tree(rev)?;
    let entry = repo
        .entry_at_path(root, path)?
        .with_context(|| format!("'{path}' does not exist at {rev}"))?;
    if entry.kind == EntryKind::Tree {
        bail!("'{path}' is a directory");
    }
    match repo.read_blob(entry.oid, limit)? {
        BlobContent::Text { data, .. } => {
            out.extend_from_slice(&data);
            Ok(ExitCode::SUCCESS)
        }
        BlobContent::Binary { size } => {
            writeln!(io::stderr().lock(), "'{path}' is binary ({size} bytes)")?;
            Ok(ExitCode::from(EXIT_BINARY))
        }
        BlobContent::TooLarge { size, limit } => {
            writeln!(
                io::stderr().lock(),
                "'{path}' is {size} bytes, over the {limit} byte limit"
            )?;
            Ok(ExitCode::from(EXIT_TOO_LARGE))
        }
    }
}

fn log(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("log", args, &["-n", "--skip"], &[])?;
    args.expect(0, 1, "[rev]")?;
    let rev = args.positional(0).unwrap_or("HEAD");
    let limit = args.number("-n")?.unwrap_or(20);
    let skip = args.number("--skip")?.unwrap_or(0);
    let tip = repo.resolve(rev)?;
    for commit in repo.log(tip, skip, limit)? {
        write_commit_row(out, &commit)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// One commit as `oid, author_time, author_name, title`, the row log, filelog and search print.
fn write_commit_row(out: &mut Vec<u8>, commit: &CommitSummary) -> Result<()> {
    writeln!(
        out,
        "{}\t{}\t{}\t{}",
        commit.oid,
        commit.author_time,
        field(&commit.author_name),
        field(&commit.title)
    )?;
    Ok(())
}

/// Free text from a commit (a name, a title, a message) as one TSV field: every control
/// character, tab and newline included, becomes a space, so the text cannot split a record.
fn field(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().any(char::is_control) {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .into()
    } else {
        text.into()
    }
}

/// The `--max-scan` commit budget: every commit when the flag is absent, at least one when given.
fn scan_budget(args: &VerbArgs<'_>) -> Result<usize> {
    match args.number("--max-scan")? {
        None => Ok(usize::MAX),
        Some(0) => Err(usage_error("--max-scan must be at least 1".to_owned())),
        Some(budget) => Ok(budget),
    }
}

/// Prints a page's rows, and warns when the budget ran out before `limit` matches or the end.
fn write_commit_page(out: &mut Vec<u8>, page: &CommitPage, limit: usize) -> Result<()> {
    for commit in &page.commits {
        write_commit_row(out, commit)?;
    }
    if !page.complete && page.commits.len() < limit {
        writeln!(
            io::stderr().lock(),
            "warning: truncated after scanning {} commits",
            page.scanned
        )?;
    }
    Ok(())
}

fn filelog(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("filelog", args, &["-n", "--max-scan"], &[])?;
    args.expect(1, 2, "<path> [rev]")?;
    let Some(path) = args.positional(0) else {
        bail!("filelog needs <path>\n\n{USAGE}");
    };
    let path = &canonical_path(path)?;
    let rev = args.positional(1).unwrap_or("HEAD");
    let limit = args.number("-n")?.unwrap_or(20);
    let max_scan = scan_budget(&args)?;
    let tip = repo.resolve(rev)?;
    let page = repo.path_history(tip, path, 0, limit, max_scan)?;
    write_commit_page(out, &page, limit)?;
    Ok(ExitCode::SUCCESS)
}

fn search(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args(
        "search",
        args,
        &[
            "-n",
            "--max-scan",
            "--message",
            "--author",
            "--since",
            "--until",
        ],
        &[],
    )?;
    args.expect(0, 1, "[rev]")?;
    let rev = args.positional(0).unwrap_or("HEAD");
    let limit = args.number("-n")?.unwrap_or(20);
    let max_scan = scan_budget(&args)?;
    let filter = CommitFilter {
        message: args.value(&["--message"]).map(str::to_owned),
        author: args.value(&["--author"]).map(str::to_owned),
        after: epoch(&args, "--since")?,
        before: epoch(&args, "--until")?,
    };
    let tip = repo.resolve(rev)?;
    let page = repo.search_commits(tip, &filter, 0, limit, max_scan)?;
    write_commit_page(out, &page, limit)?;
    Ok(ExitCode::SUCCESS)
}

fn epoch(args: &VerbArgs<'_>, flag: &str) -> Result<Option<i64>> {
    let Some(raw) = args.value(&[flag]) else {
        return Ok(None);
    };
    let seconds = raw.parse().map_err(|_| {
        usage_error(format!(
            "{flag} value '{raw}' is not a number of seconds since the Unix epoch"
        ))
    })?;
    Ok(Some(seconds))
}

fn show(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("show", args, &[], &[])?;
    args.expect(1, 1, "<rev>")?;
    let Some(rev) = args.positional(0) else {
        bail!("show needs <rev>\n\n{USAGE}");
    };
    let detail = repo.commit_details(repo.resolve(rev)?)?;
    let summary = &detail.summary;
    writeln!(out, "commit\t{}", summary.oid)?;
    writeln!(out, "tree\t{}", detail.tree)?;
    for parent in &summary.parents {
        writeln!(out, "parent\t{parent}")?;
    }
    writeln!(
        out,
        "author\t{} <{}>\t{}\t{}",
        field(&summary.author_name),
        field(&summary.author_email),
        summary.author_time,
        summary.author_offset
    )?;
    writeln!(
        out,
        "committer\t{} <{}>\t{}\t{}",
        field(&detail.committer_name),
        field(&detail.committer_email),
        detail.committer_time,
        detail.committer_offset
    )?;
    writeln!(out, "\n{}", summary.title)?;
    if !detail.body.is_empty() {
        writeln!(out, "\n{}", detail.body)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn diff(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let mut args = split_args("diff", args, &[], &["--worktree"])?;
    if args.has("--worktree") {
        args.label = "diff --worktree";
        args.expect(1, 1, "<path>")?;
        let Some(path) = args.positional(0) else {
            bail!("diff --worktree needs <path>\n\n{USAGE}");
        };
        let path = &canonical_path(path)?;
        let old = match head_tree(repo)? {
            Some(tree) => file_oid(repo.entry_at_path(tree, path)?, path)?,
            None => None,
        };
        let diff = if old.is_some() && is_missing_from_disk(repo, path) {
            repo.diff_blobs(old, None, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)?
        } else {
            repo.diff_worktree_file(old, path, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)?
        };
        return render_blob_diff(diff, path, out);
    }
    args.expect(1, 3, "<rev>, or <old> <new> [path]")?;
    if let [rev] = args.positionals[..] {
        let new = repo.resolve(rev)?;
        let parent = repo.commit(new)?.parents.first().copied();
        return render_tree_changes(repo, parent, new, out);
    }
    let [old_rev, new_rev, ..] = args.positionals[..] else {
        bail!("diff needs <old> <new>\n\n{USAGE}");
    };
    let old = repo.resolve(old_rev)?;
    let new = repo.resolve(new_rev)?;
    let Some(path) = args.positional(2) else {
        return render_tree_changes(repo, Some(old), new, out);
    };
    let path = &canonical_path(path)?;
    let old_tree = repo.tree_of(old)?;
    let new_tree = repo.tree_of(new)?;
    let old_entry = repo.entry_at_path(old_tree, path)?;
    let new_entry = repo.entry_at_path(new_tree, path)?;
    if is_submodule_bump(old_entry.as_ref(), new_entry.as_ref()) {
        return render_submodule_bump(old_entry, new_entry, out);
    }
    let old_blob = file_oid(old_entry, path)?;
    let new_blob = file_oid(new_entry, path)?;
    if old_blob.is_none() && new_blob.is_none() {
        bail!("'{path}' does not exist at {old_rev} or {new_rev}");
    }
    let diff = repo.diff_blobs(old_blob, new_blob, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)?;
    render_blob_diff(diff, path, out)
}

/// Whether the working-tree file at `path` is gone, so its diff against HEAD is a deletion.
/// Any other stat failure is left for `diff_worktree_file` to report.
fn is_missing_from_disk(repo: &Repository, path: &str) -> bool {
    repo.work_dir().is_some_and(|work_dir| {
        std::fs::symlink_metadata(work_dir.join(path))
            .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
    })
}

/// Whether every side present is a gitlink, whose id names a commit in another repository
/// and so has no blob to diff here.
fn is_submodule_bump(old: Option<&TreeEntry>, new: Option<&TreeEntry>) -> bool {
    let is_gitlink =
        |entry: Option<&TreeEntry>| entry.is_none_or(|entry| entry.kind == EntryKind::Submodule);
    (old.is_some() || new.is_some()) && is_gitlink(old) && is_gitlink(new)
}

/// The `-Subproject commit` / `+Subproject commit` lines git prints for a gitlink change.
fn render_submodule_bump(
    old: Option<TreeEntry>,
    new: Option<TreeEntry>,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    if let Some(old) = old {
        writeln!(out, "-Subproject commit {}", old.oid)?;
    }
    if let Some(new) = new {
        writeln!(out, "+Subproject commit {}", new.oid)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The blob id of a diffed side, refusing a directory as `cat` does.
fn file_oid(entry: Option<TreeEntry>, path: &str) -> Result<Option<Oid>> {
    match entry {
        Some(entry) if entry.kind == EntryKind::Tree => bail!("'{path}' is a directory"),
        entry => Ok(entry.map(|entry| entry.oid)),
    }
}

/// One `<letter>\t<path>[\t<source>]` line per changed file from `old` (the empty tree when
/// `None`) to `new`.
fn render_tree_changes(
    repo: &Repository,
    old: Option<Oid>,
    new: Oid,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    for change in repo.diff_trees(old, new)? {
        let letter = match change.kind {
            ChangeKind::Added => "A",
            ChangeKind::Deleted => "D",
            ChangeKind::Modified => "M",
            ChangeKind::Renamed => "R",
        };
        write!(out, "{letter}\t")?;
        match &change.source_path {
            Some(source) => {
                write_path(out, &change.path);
                out.push(b'\t');
                Records::Lines.end_with_path(out, source);
            }
            None => Records::Lines.end_with_path(out, &change.path),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn render_blob_diff(diff: BlobDiff, path: &str, out: &mut Vec<u8>) -> Result<ExitCode> {
    match diff {
        BlobDiff::Text { hunks } => {
            for hunk in hunks {
                writeln!(
                    out,
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
                )?;
                for line in hunk.lines {
                    let sign = match line.kind {
                        DiffLineKind::Context => " ",
                        DiffLineKind::Added => "+",
                        DiffLineKind::Removed => "-",
                    };
                    out.extend_from_slice(sign.as_bytes());
                    out.extend_from_slice(&line.text);
                    out.push(b'\n');
                    if line.eol == LineEnd::None {
                        out.extend_from_slice(b"\\ No newline at end of file\n");
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        BlobDiff::Binary { old_size, new_size } => {
            writeln!(
                io::stderr().lock(),
                "'{path}' is binary ({old_size} -> {new_size} bytes)"
            )?;
            Ok(ExitCode::from(EXIT_BINARY))
        }
        BlobDiff::TooLarge { size, limit } => {
            writeln!(
                io::stderr().lock(),
                "'{path}' is {size} bytes, over the {limit} byte limit"
            )?;
            Ok(ExitCode::from(EXIT_TOO_LARGE))
        }
    }
}

fn blame(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("blame", args, &[], &[])?;
    args.expect(2, 2, "<rev> <path>")?;
    let [rev, path] = args.positionals[..] else {
        bail!("blame needs <rev> <path>\n\n{USAGE}");
    };
    let path = &canonical_path(path)?;
    let tip = repo.resolve(rev)?;
    let mut titles = std::collections::HashMap::new();
    for range in repo.blame(tip, path)? {
        let commit = match titles.entry(range.commit) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(repo.commit(range.commit)?)
            }
        };
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            range.commit,
            range.start_line,
            range.line_count,
            field(&commit.author_name),
            field(&commit.title)
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn changes(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("changes", args, &["--max-scan"], &["-z"])?;
    args.expect(0, 0, "no positional arguments")?;
    let records = Records::from_args(&args);
    let max_scan = args.number("--max-scan")?.unwrap_or(200_000);
    let report = repo.worktree_status(head_tree(repo)?, max_scan)?;
    for entry in &report.entries {
        write!(out, "{}\t", status_letter(entry.status))?;
        records.end_with_path(out, &entry.path);
    }
    print_warnings(&report.warnings)?;
    print_unreadable(&report.unreadable)?;
    if report.truncated {
        writeln!(
            io::stderr().lock(),
            "warning: truncated after scanning {} entries",
            report.scanned
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn files(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("files", args, &["--max-scan"], &["-z"])?;
    args.expect(0, 0, "no positional arguments")?;
    let records = Records::from_args(&args);
    let max_scan = args.number("--max-scan")?.unwrap_or(200_000);
    let list = repo.worktree_files(head_tree(repo)?, max_scan)?;
    for file in &list.files {
        records.end_with_path(out, &file.path);
    }
    print_warnings(&list.warnings)?;
    print_unreadable(&list.unreadable)?;
    if list.truncated {
        writeln!(
            io::stderr().lock(),
            "warning: truncated after scanning {} entries",
            list.scanned
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn stash(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    split_args("stash", args, &[], &[])?.expect(0, 0, "no arguments")?;
    for entry in repo.stash_list()? {
        writeln!(
            out,
            "{}\t{}\t{}",
            entry.index,
            &entry.oid.to_hex()[..12],
            entry.message
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn status(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("status", args, &[], &["-z"])?;
    args.expect(0, 1, "[dir]")?;
    let records = Records::from_args(&args);
    let dir = &canonical_path(args.positional(0).unwrap_or(""))?;
    let listing = repo.worktree_entries(head_tree(repo)?, dir)?;
    for entry in &listing.entries {
        write!(
            out,
            "{}\t{}\t",
            status_letter(entry.status),
            kind_name(entry.kind)
        )?;
        records.end_with_path(out, &entry.name);
    }
    for missing in &listing.missing {
        write!(out, "D\t{}\t", kind_name(missing.kind))?;
        records.end_with_path(out, &missing.name);
    }
    print_warnings(&listing.warnings)?;
    Ok(ExitCode::SUCCESS)
}

/// A working-tree read's non-fatal failures, one `warning:` line each on stderr.
fn print_warnings(warnings: &[String]) -> Result<()> {
    let mut stderr = io::stderr().lock();
    for warning in warnings {
        writeln!(stderr, "warning: {warning}")?;
    }
    Ok(())
}

/// Working-tree entries a walk skipped because they could not be read, one `warning:` line
/// each on stderr.
fn print_unreadable(unreadable: &[UnreadableEntry]) -> Result<()> {
    let mut stderr = io::stderr().lock();
    for entry in unreadable {
        writeln!(stderr, "warning: skipped '{}': {}", entry.path, entry.error)?;
    }
    Ok(())
}

/// `"Name <email>"`, as `git --author` takes it.
fn parse_author(raw: &str) -> Result<rc_git_guard::actions::Identity> {
    let malformed = || usage_error(format!("--author needs \"Name <email>\", got '{raw}'"));
    let Some((name, rest)) = raw.split_once('<') else {
        return Err(malformed());
    };
    let Some(email) = rest.strip_suffix('>') else {
        return Err(malformed());
    };
    Ok(rc_git_guard::actions::Identity {
        name: name.trim().to_owned(),
        email: email.trim().to_owned(),
    })
}

fn author(args: &VerbArgs<'_>) -> Result<Option<rc_git_guard::actions::Identity>> {
    args.value(&["--author"]).map(parse_author).transpose()
}

const MESSAGE_FLAGS: [&str; 4] = ["-m", "--message", "--author", "--confirm"];
const COMMIT_FLAGS: [&str; 5] = ["-m", "--message", "--author", "--confirm", "--expect-head"];

/// The HEAD commit the guard checks the commit against: the caller's `--expect-head`, so a
/// commit made elsewhere since they looked is refused, or else HEAD as read now (`None` when
/// the branch is unborn).
fn expected_head(repo: &Repository, flag: Option<&str>) -> Result<Option<String>> {
    if let Some(hex) = flag {
        let oid = rc_git_core::Oid::from_hex(hex).map_err(|_| {
            usage_error(format!(
                "commit --expect-head takes a full commit id, not '{hex}'\n\n{USAGE}"
            ))
        })?;
        return Ok(Some(oid.to_hex()));
    }
    let head = repo.head().context("commit: read HEAD")?;
    Ok(head.target.map(|oid| oid.to_hex()))
}

fn commit_verb(
    repo: &Repository,
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let args = split_args("commit", args, &COMMIT_FLAGS, &[])?;
    args.expect(1, usize::MAX, "at least one <path>")?;
    let message = args
        .value(&["-m", "--message"])
        .ok_or_else(|| usage_error(format!("commit needs -m <msg>\n\n{USAGE}")))?;
    let expected = expected_head(repo, args.value(&["--expect-head"]))?;
    let selections: Vec<rc_git_guard::actions::FileSelection> = canonical_paths(&args.positionals)?
        .into_iter()
        .map(|path| rc_git_guard::actions::FileSelection::Whole { path })
        .collect();
    let done = rc_git_guard::actions::commit(
        repo_path,
        &rc_git_guard::Policy::default(),
        message,
        author(&args)?,
        expected.as_deref(),
        &selections,
        args.value(&["--confirm"]),
    )?;
    writeln!(out, "{}\t{}", done.oid, done.branch)?;
    Ok(ExitCode::SUCCESS)
}

fn stash_push_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let args = split_args("stash-push", args, &MESSAGE_FLAGS, &["--yes"])?;
    args.expect(1, usize::MAX, "at least one <path>")?;
    let action = rc_git_guard::Action::StashPush {
        files: args.positionals.len(),
    };
    require_yes(&args, &action)?;
    let saved = rc_git_guard::actions::stash_push(
        repo_path,
        &rc_git_guard::Policy::default(),
        &canonical_paths(&args.positionals)?,
        args.value(&["-m", "--message"]),
        author(&args)?,
        args.value(&["--confirm"]),
    )?;
    writeln!(out, "{}\t{}", saved.oid, field(&saved.message))?;
    Ok(ExitCode::SUCCESS)
}

/// The CLI's form of the confirm button. When the policy asks only for a confirm, with no typed
/// phrase, the guard accepts any input, so the explicit `--yes` is what stops a mistyped command
/// from running; an action with a typed phrase is gated by `--confirm` inside the guard instead.
fn require_yes(args: &VerbArgs<'_>, action: &rc_git_guard::Action) -> Result<()> {
    let rc_git_guard::Decision::Confirm(challenge) =
        rc_git_guard::Policy::default().evaluate(action)
    else {
        return Ok(());
    };
    if challenge.typed_phrase.is_some() || args.has("--yes") {
        return Ok(());
    }
    Err(usage_error(format!(
        "{} needs --yes to confirm: {}\n\n{USAGE}",
        args.label, challenge.summary
    )))
}

#[derive(Clone, Copy)]
enum StashVerb {
    Apply,
    Pop,
    Drop,
}

fn stash_index_verb(
    repo_path: &std::path::Path,
    args: &[String],
    verb: StashVerb,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let label = match verb {
        StashVerb::Apply => "stash-apply",
        StashVerb::Pop => "stash-pop",
        StashVerb::Drop => "stash-drop",
    };
    let switches: &[&'static str] = match verb {
        StashVerb::Pop => &["--yes"],
        StashVerb::Apply | StashVerb::Drop => &[],
    };
    let args = split_args(label, args, &["--confirm"], switches)?;
    args.expect(0, 1, "[n]")?;
    if let StashVerb::Pop = verb {
        require_yes(&args, &rc_git_guard::Action::StashPop)?;
    }
    let index = match args.positional(0) {
        Some(raw) => raw
            .parse()
            .map_err(|_| usage_error(format!("'{raw}' is not a stash index")))?,
        None => 0,
    };
    let policy = rc_git_guard::Policy::default();
    let input = args.value(&["--confirm"]);
    match verb {
        StashVerb::Apply => {
            rc_git_guard::actions::stash_apply(repo_path, &policy, index, input)?;
        }
        StashVerb::Pop => {
            let deleted = rc_git_guard::actions::stash_pop(repo_path, &policy, index, input)?;
            if let Some(backup) = deleted.backup_ref {
                writeln!(out, "{backup}")?;
            }
        }
        StashVerb::Drop => {
            let deleted = rc_git_guard::actions::stash_drop(repo_path, &policy, index, input)?;
            if let Some(backup) = deleted.backup_ref {
                writeln!(out, "{backup}")?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Discarding cannot be undone and the guard asks no typed phrase for it, so the CLI's form of
/// the confirm button is an explicit `--yes` or `--confirm discard`.
fn discard_verb(repo_path: &std::path::Path, args: &[String]) -> Result<ExitCode> {
    let args = split_args("discard", args, &["--confirm"], &["--yes"])?;
    args.expect(1, usize::MAX, "at least one <path>")?;
    let confirm = args.value(&["--confirm"]);
    let confirmed = match confirm {
        Some(phrase) => phrase.trim() == "discard",
        None => args.has("--yes"),
    };
    if !confirmed {
        return Err(usage_error(format!(
            "discard cannot be undone; pass --yes (or --confirm discard) to restore the files\n\n\
             {USAGE}"
        )));
    }
    rc_git_guard::actions::discard_files(
        repo_path,
        &rc_git_guard::Policy::default(),
        &canonical_paths(&args.positionals)?,
        confirm,
    )?;
    Ok(ExitCode::SUCCESS)
}

fn resolve(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("resolve", args, &[], &[])?;
    args.expect(1, 1, "<rev>")?;
    let Some(rev) = args.positional(0) else {
        bail!("resolve needs <rev>\n\n{USAGE}");
    };
    writeln!(out, "{}", repo.resolve(rev)?)?;
    Ok(ExitCode::SUCCESS)
}

/// Everything a remote verb's arguments and environment provide.
struct RemoteArgs {
    creds: Option<rc_git_guard::actions::Credentials>,
    accepted_hostkey: Option<String>,
    ssh_dir: Option<std::path::PathBuf>,
}

impl RemoteArgs {
    fn auth(&self) -> rc_git_guard::actions::RemoteAuth<'_> {
        rc_git_guard::actions::RemoteAuth {
            creds: self.creds.as_ref(),
            accepted_hostkey: self.accepted_hostkey.as_deref(),
            ssh_dir: self.ssh_dir.as_deref(),
        }
    }
}

/// `--username U` / `--token T`, `--accept-hostkey F`, and `--ssh-dir D`. The environment
/// fills in only what the upstream's transport reads: `RC_GIT_TOKEN` stands in for
/// `--token` when `--username` was given or the upstream is HTTPS, and
/// `RC_GIT_KEY_PASSPHRASE` supplies the key passphrase only when the upstream is ssh, so
/// the passphrase never answers an HTTP challenge.
fn parse_remote_args(
    label: &'static str,
    repo_path: &std::path::Path,
    args: &[String],
) -> Result<RemoteArgs> {
    let args = split_args(
        label,
        args,
        &["--username", "--token", "--accept-hostkey", "--ssh-dir"],
        &[],
    )?;
    args.expect(0, 0, "no positional arguments")?;
    let username = args.value(&["--username"]).map(str::to_owned);
    let token = args.value(&["--token"]).map(str::to_owned);
    let accepted_hostkey = args.value(&["--accept-hostkey"]).map(str::to_owned);
    let ssh_dir = args.value(&["--ssh-dir"]).map(std::path::PathBuf::from);
    let creds = credentials(repo_path, username, token)?;
    Ok(RemoteArgs {
        creds,
        accepted_hostkey,
        ssh_dir,
    })
}

/// The credentials a remote verb sends: `--username` with `--token` (or `RC_GIT_TOKEN`), or,
/// with neither flag and an ssh upstream, `RC_GIT_KEY_PASSPHRASE` as the key passphrase.
///
/// # Errors
///
/// Fails when the upstream cannot be read, or when only one of username and token is known.
fn credentials(
    repo_path: &std::path::Path,
    username: Option<String>,
    mut token: Option<String>,
) -> Result<Option<rc_git_guard::actions::Credentials>> {
    let transport = if token.is_none() {
        upstream_transport(repo_path)?
    } else {
        Transport::Other
    };
    if token.is_none() && (username.is_some() || transport == Transport::Https) {
        token = non_empty_env("RC_GIT_TOKEN");
    }
    match (username, token) {
        (Some(username), Some(token)) => {
            Ok(Some(rc_git_guard::actions::Credentials { username, token }))
        }
        (None, None) if transport == Transport::Ssh => Ok(non_empty_env("RC_GIT_KEY_PASSPHRASE")
            .map(|passphrase| rc_git_guard::actions::Credentials {
                username: String::new(),
                token: passphrase,
            })),
        (None, None) => Ok(None),
        (Some(_), None) => Err(usage_error(
            "--username was given without --token (or RC_GIT_TOKEN)".to_owned(),
        )),
        (None, Some(_)) => Err(usage_error(
            "a token was given without --username".to_owned(),
        )),
    }
}

/// Which credential the upstream's transport reads: a token over HTTPS, a key
/// passphrase over ssh, nothing for anything else (the guard refuses those remotes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Https,
    Ssh,
    Other,
}

/// The transport of the checked-out branch's upstream, read from the repository's own
/// configuration without any network I/O.
fn upstream_transport(repo_path: &std::path::Path) -> Result<Transport> {
    let status = rc_git_guard::actions::upstream_status(repo_path)?;
    if !status.has_upstream {
        return Ok(Transport::Other);
    }
    let url = status.url.as_str();
    if url.starts_with("https://") || url.starts_with("http://") {
        return Ok(Transport::Https);
    }
    let scp_like = !url.contains("://")
        && url
            .split_once(':')
            .is_some_and(|(host, _)| !host.is_empty() && !host.contains('/'));
    if url.starts_with("ssh://") || scp_like {
        return Ok(Transport::Ssh);
    }
    Ok(Transport::Other)
}

/// The variable's value, `None` when it is unset, empty, or not UTF-8.
fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn write_fetch(out: &mut Vec<u8>, outcome: &rc_git_guard::actions::FetchOutcome) -> Result<()> {
    if outcome.updated_refs.is_empty() {
        writeln!(out, "fetch\t{}\tup to date", outcome.remote)?;
    }
    for update in &outcome.updated_refs {
        writeln!(out, "fetch\t{}\t{}", update.name, update.target)?;
    }
    Ok(())
}

fn write_pull(out: &mut Vec<u8>, outcome: &rc_git_guard::actions::PullOutcome) -> Result<()> {
    if outcome.old == outcome.new {
        writeln!(out, "pull\t{}\tup to date", outcome.branch)?;
    } else {
        writeln!(
            out,
            "pull\t{}\t{}\t{}\t{} file(s)",
            outcome.branch, outcome.old, outcome.new, outcome.files_changed
        )?;
    }
    Ok(())
}

fn write_push(out: &mut Vec<u8>, outcome: &rc_git_guard::actions::PushOutcome) -> Result<()> {
    if outcome.old == Some(outcome.new) {
        writeln!(out, "push\t{}\tup to date", outcome.branch)?;
    } else {
        writeln!(
            out,
            "push\t{}\t{}/{}\t{} commit(s)",
            outcome.branch, outcome.remote, outcome.branch, outcome.pushed_commits
        )?;
    }
    Ok(())
}

fn fetch_verb(
    repo_path: &std::path::Path,
    args: &[String],
    cancel: &Cancel,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let remote = parse_remote_args("fetch", repo_path, args)?;
    let outcome = rc_git_guard::actions::fetch_upstream(
        repo_path,
        &rc_git_guard::Policy::default(),
        remote.auth(),
        cancel,
    )?;
    write_fetch(out, &outcome)?;
    Ok(ExitCode::SUCCESS)
}

/// Only the fetch half honors `cancel`: the fast-forward's tree apply takes no flag, so a
/// second signal during it can leave files rewritten with the branch ref not yet moved.
fn pull_verb(
    repo_path: &std::path::Path,
    args: &[String],
    cancel: &Cancel,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let remote = parse_remote_args("pull", repo_path, args)?;
    let policy = rc_git_guard::Policy::default();
    let fetch = rc_git_guard::actions::fetch_upstream(repo_path, &policy, remote.auth(), cancel)?;
    write_fetch(out, &fetch)?;
    let outcome = rc_git_guard::actions::pull_fast_forward(repo_path, &policy)?;
    write_pull(out, &outcome)?;
    Ok(ExitCode::SUCCESS)
}

/// Runs a verb that talks to the remote, with Ctrl+C and SIGTERM wired to its cancel flag.
fn remote_verb(invocation: &Invocation, out: &mut Vec<u8>) -> Result<ExitCode> {
    let cancel = interruptible_cancel()?;
    let (repo, args) = (invocation.repo.as_path(), invocation.args.as_slice());
    match invocation.verb {
        Verb::Fetch => fetch_verb(repo, args, &cancel, out),
        Verb::Pull => pull_verb(repo, args, &cancel, out),
        Verb::Push => push_verb(repo, args, &cancel, out),
        Verb::Sync => sync_verb(repo, args, &cancel, out),
        other => bail!("'{}' is not a remote command", other.name()),
    }
}

fn checkout_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let args = split_args("checkout", args, &[], &[])?;
    args.expect(1, 1, "exactly one branch name")?;
    let Some(branch) = args.positional(0) else {
        bail!("checkout needs exactly one branch name\n\n{USAGE}");
    };
    let outcome = rc_git_guard::actions::checkout_branch(
        repo_path,
        &rc_git_guard::Policy::default(),
        branch,
    )?;
    if outcome.already_on_branch {
        writeln!(out, "checkout\t{}\talready checked out", outcome.branch)?;
    } else {
        writeln!(
            out,
            "checkout\t{}\t{}\t{} file(s)",
            outcome.branch, outcome.new_head, outcome.files_changed
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn push_verb(
    repo_path: &std::path::Path,
    args: &[String],
    cancel: &Cancel,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let remote = parse_remote_args("push", repo_path, args)?;
    let outcome = rc_git_guard::actions::push_upstream(
        repo_path,
        &rc_git_guard::Policy::default(),
        remote.auth(),
        cancel,
    )?;
    write_push(out, &outcome)?;
    Ok(ExitCode::SUCCESS)
}

fn sync_verb(
    repo_path: &std::path::Path,
    args: &[String],
    cancel: &Cancel,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let remote = parse_remote_args("sync", repo_path, args)?;
    let outcome = rc_git_guard::actions::sync(
        repo_path,
        &rc_git_guard::Policy::default(),
        remote.auth(),
        cancel,
    )?;
    write_fetch(out, &outcome.fetch)?;
    write_pull(out, &outcome.pull)?;
    match &outcome.push {
        Some(push) => write_push(out, push)?,
        None => writeln!(out, "push\t{}\tup to date", outcome.pull.branch)?,
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use crate::write_path;

    fn quoted(path: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_path(&mut out, path);
        out
    }

    #[test]
    fn a_cancelled_remote_step_exits_130() {
        let err = anyhow::Error::new(crate::ActionError::Cancelled { op: "fetch" });
        assert_eq!(crate::exit_code_for(&err), 130);
    }

    #[test]
    fn sigint_sets_the_shared_cancel_flag() {
        let cancel = crate::interruptible_cancel().unwrap();
        assert!(!cancel.is_cancelled());
        signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn plain_and_utf8_names_print_unchanged() {
        assert_eq!(quoted(b"src/lib.rs"), b"src/lib.rs");
        assert_eq!(
            quoted("caf\u{e9}.txt".as_bytes()),
            "caf\u{e9}.txt".as_bytes()
        );
    }

    #[test]
    fn control_quote_and_backslash_are_escaped() {
        assert_eq!(quoted(b"a\nb"), br#""a\nb""#);
        assert_eq!(quoted(b"a\x01\x7f\"\\"), br#""a\001\177\"\\""#);
    }

    #[test]
    fn bytes_that_are_not_utf8_print_in_octal() {
        assert_eq!(quoted(b"caf\xe9\xff"), br#""caf\351\377""#);
    }
}
