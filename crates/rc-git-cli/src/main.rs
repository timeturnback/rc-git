//! `rc-git`: the command-line face of `rc-git-core` reads, `rc-git-guard` actions, and
//! `rc-git-highlight` spans and import links.
//!
//! Output is tab-separated so scripts and agents can parse it. `--time` prints how long the
//! repository open and the command took, which is how the CLI rows of the budget table in
//! `docs/performance.md` are checked without the GUI; the rows that run in Swift are not.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rc_git_core::{
    Blame, BlobContent, BlobDiff, ChangeKind, CommitFilter, CommitPage, CommitSummary,
    DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, DiffLineKind, EntryKind, LineEnd, Oid, RefKind,
    Repository, TreeEntry, UnreadableEntry, WorktreeStatus,
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
  blame   <repo> <rev> <path> [--since EPOCH]  which commit introduced each line; a file over
                                          8 MiB at <rev> exits 4. --since stops the walk at
                                          commits older than EPOCH and prefixes the commit of
                                          lines it could not trace further with '^', like git
  status  <repo> [-z] [dir]               one working-tree directory against HEAD
  changes <repo> [-z] [--max-scan N]      every changed working-tree path against HEAD
  files   <repo> [-z] [--max-scan N]      every file on disk, ignored folders skipped
  stash   <repo>                          stash entries, newest first
  resolve <repo> <rev>                    object id a revision names
  default-branch <repo>                   the mainline branch: a remote's HEAD, else master/main
  upstream <repo>                         the current branch's upstream:
                                          branch<TAB>remote<TAB>url<TAB>ahead<TAB>behind,
                                          or branch<TAB>none when it has none
  highlight <repo> <rev> <path>           syntax spans: start<TAB>end<TAB>class, byte offsets;
                                          a binary file exits 3, one over 2 MiB exits 4
  links   <repo> <rev> <path>             import links of a JS/TS file: start<TAB>end<TAB>
                                          specifier_start<TAB>specifier_end; limits as highlight

  --max-scan N stops after examining N commits (filelog, search; default: all history) or N
  working-tree entries (changes, files; default 200000) and warns that the output is
  truncated. filelog and search reject N = 0.

  commit      <repo> -m <msg> [--author \"Name <email>\"] [--confirm <phrase>]
              [--expect-head <oid>] [--hunk <hunk>]... [<path>...]
                                          commit files to the branch; with --expect-head,
                                          refuse if HEAD is no longer that full commit id;
                                          --hunk commits only that hunk of a tracked file
  stash-push  <repo> --yes [-m <msg>] [--author \"Name <email>\"] [--confirm <phrase>] <path>...
                                          stash files, restore them to HEAD
  stash-apply <repo> [n] [--confirm <phrase>]  write stash@{n} back to the working tree
  stash-pop   <repo> --yes [n] [--confirm <phrase>]  apply stash@{n}, then drop it
  stash-drop  <repo> [n] --confirm drop   delete stash@{n} without applying
  discard     <repo> --yes <path>...      restore files to HEAD, delete untracked; no undo
                                          (--confirm discard works in place of --yes);
                                          prints restored|deleted<TAB>path per path
  discard     <repo> --yes --hunk <hunk>...  restore only those hunks of one file to HEAD;
                                          prints hunks<TAB>count<TAB>path
  checkout    <repo> <branch>             switch the working tree and HEAD to a local branch
  branch-create <repo> <name> [rev]       create a branch at rev (default HEAD); never moves
                                          an existing one; prints created<TAB>full name
  tag-create  <repo> <name> [rev]         create a lightweight tag the same way
  branch-delete <repo> <name> [--confirm <name>]  delete a branch; an unmerged one needs its
                                          name typed and is backed up first
  tag-delete  <repo> <name> --confirm <name>  delete a tag, backed up first; both print
                                          deleted<TAB>full name, then backup<TAB>ref if written
  plan-delete <repo> branch|tag <name>    what deleting would need, one record:
                                          allow|confirm|deny<TAB>risk<TAB>phrase<TAB>backup
                                          yes|no<TAB>summary or reason ('-' when none)

  <hunk> is <path>:<old_start>,<old_lines>,<new_start>,<new_lines>, the numbers of a
  '@@ -a,b +c,d @@' header that `diff --worktree` prints for the file.

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
  8 a --hunk no longer matches the file (rerun diff --worktree and pick again),
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
const EXIT_STALE_HUNK: u8 = 8;
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

    /// Every value given for `flag`, in command-line order.
    fn values_of(&self, flag: &str) -> Vec<&'a str> {
        let mut found = Vec::new();
        for (name, value) in &self.values {
            if *name == flag {
                found.push(*value);
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
    BranchCreate,
    TagCreate,
    BranchDelete,
    TagDelete,
    PlanDelete,
    Upstream,
    Highlight,
    Links,
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
            "branch-create" => Some(Self::BranchCreate),
            "tag-create" => Some(Self::TagCreate),
            "branch-delete" => Some(Self::BranchDelete),
            "tag-delete" => Some(Self::TagDelete),
            "plan-delete" => Some(Self::PlanDelete),
            "upstream" => Some(Self::Upstream),
            "highlight" => Some(Self::Highlight),
            "links" => Some(Self::Links),
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
            Self::BranchCreate => "branch-create",
            Self::TagCreate => "tag-create",
            Self::BranchDelete => "branch-delete",
            Self::TagDelete => "tag-delete",
            Self::PlanDelete => "plan-delete",
            Self::Upstream => "upstream",
            Self::Highlight => "highlight",
            Self::Links => "links",
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
            let _ = writeln!(
                io::stderr().lock(),
                "error: {}{}",
                error_line(&err),
                cli_hint(&err)
            );
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
        Some(
            ActionError::AuthRequired { .. }
            | ActionError::PassphraseRequired { .. }
            | ActionError::AgentSignRefused { .. },
        ) => EXIT_AUTH,
        Some(ActionError::UnknownHostKey { .. }) => EXIT_HOSTKEY,
        Some(ActionError::Diverged { .. } | ActionError::PushRejected { .. }) => EXIT_DIVERGED,
        Some(ActionError::HunkMismatch { .. }) => EXIT_STALE_HUNK,
        Some(ActionError::Cancelled { .. }) => EXIT_INTERRUPTED,
        Some(_) | None => EXIT_ERROR,
    }
}

/// What the command line can do about `err` that the guard's message cannot name: the guard
/// says "pass an identity", the CLI knows that is `--author`; it says what to type, the CLI
/// knows that is `--confirm`.
fn cli_hint(err: &anyhow::Error) -> String {
    match err.downcast_ref::<ActionError>() {
        Some(ActionError::MissingIdentity { .. }) => " (--author \"Name <email>\")".to_owned(),
        Some(ActionError::NotConfirmed { phrase, .. }) => {
            format!(" (rerun with --confirm {})", shell_word(phrase))
        }
        Some(_) | None => String::new(),
    }
}

/// `word` as one shell word: bare when it holds only characters no shell treats specially,
/// single-quoted otherwise.
fn shell_word(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./@+:,%".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// One cancel flag for every remote step of the command, set by SIGINT or SIGTERM so the guard
/// stops between steps and cleans up instead of the process dying with `.lock` and `tmp_pack_*`
/// files left in `.git`. A second signal, once the flag is set, exits at once with
/// [`EXIT_INTERRUPTED`]: some steps never look at the flag (the ssh dial, the pull's tree
/// apply), and Ctrl+C must still end them.
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
/// holds is skipped. Context causes such as a parse failure are kept. The chain stops at a
/// core error: its message already says what it means to say about the gix frames below it,
/// and a revision error deliberately leaves out gix's parser internals.
fn error_line(err: &anyhow::Error) -> String {
    let mut line = String::new();
    for frame in err.chain() {
        let text = format!("{frame:#}");
        if !line.contains(&text) {
            if !line.is_empty() {
                line.push_str(": ");
            }
            line.push_str(&text);
        }
        if frame.is::<rc_git_core::Error>() {
            break;
        }
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

/// Runs the verb, then writes whatever it put in `out` even when it failed: a remote verb that
/// fails at a later step still reports the steps that already ran, as git does.
fn run(invocation: &Invocation) -> Result<ExitCode> {
    let opened_at = Instant::now();
    let repo = Repository::open(&invocation.repo)?;
    let open_elapsed = opened_at.elapsed();

    let started_at = Instant::now();
    let mut out = Vec::new();
    let result = dispatch(&repo, invocation, &mut out);
    let command_elapsed = started_at.elapsed();

    let written = write_stdout(&out);
    let timed = if invocation.time {
        writeln!(
            io::stderr().lock(),
            "time: open {:.2} ms, {} {:.2} ms",
            open_elapsed.as_secs_f64() * 1000.0,
            invocation.verb.name(),
            command_elapsed.as_secs_f64() * 1000.0
        )
        .context("write timing")
    } else {
        Ok(())
    };
    let code = result?;
    written?;
    timed?;
    Ok(code)
}

fn dispatch(repo: &Repository, invocation: &Invocation, out: &mut Vec<u8>) -> Result<ExitCode> {
    match invocation.verb {
        Verb::Head => head(repo, &invocation.args, out),
        Verb::Refs => refs(repo, &invocation.args, out),
        Verb::Ls => ls(repo, &invocation.args, out),
        Verb::Cat => cat(repo, &invocation.args, out),
        Verb::Log => log(repo, &invocation.args, out),
        Verb::Filelog => filelog(repo, &invocation.args, out),
        Verb::Search => search(repo, &invocation.args, out),
        Verb::Show => show(repo, &invocation.args, out),
        Verb::Diff => diff(repo, &invocation.args, out),
        Verb::Blame => blame(repo, &invocation.args, out),
        Verb::Status => status(repo, &invocation.args, out),
        Verb::Changes => changes(repo, &invocation.args, out),
        Verb::Files => files(repo, &invocation.args, out),
        Verb::Stash => stash(repo, &invocation.args, out),
        Verb::Resolve => resolve(repo, &invocation.args, out),
        Verb::DefaultBranch => default_branch(repo, &invocation.args, out),
        Verb::Commit => commit_verb(repo, &invocation.repo, &invocation.args, out),
        Verb::StashPush => stash_push_verb(&invocation.repo, &invocation.args, out),
        Verb::StashApply => {
            stash_index_verb(&invocation.repo, &invocation.args, StashVerb::Apply, out)
        }
        Verb::StashPop => stash_index_verb(&invocation.repo, &invocation.args, StashVerb::Pop, out),
        Verb::StashDrop => {
            stash_index_verb(&invocation.repo, &invocation.args, StashVerb::Drop, out)
        }
        Verb::Discard => discard_verb(&invocation.repo, &invocation.args, out),
        Verb::Checkout => checkout_verb(&invocation.repo, &invocation.args, out),
        Verb::BranchCreate => {
            ref_create_verb(&invocation.repo, &invocation.args, RefSort::Branch, out)
        }
        Verb::TagCreate => ref_create_verb(&invocation.repo, &invocation.args, RefSort::Tag, out),
        Verb::BranchDelete => {
            ref_delete_verb(&invocation.repo, &invocation.args, RefSort::Branch, out)
        }
        Verb::TagDelete => ref_delete_verb(&invocation.repo, &invocation.args, RefSort::Tag, out),
        Verb::PlanDelete => plan_delete_verb(&invocation.repo, &invocation.args, out),
        Verb::Upstream => upstream_verb(&invocation.repo, &invocation.args, out),
        Verb::Highlight => highlight_verb(repo, &invocation.args, out),
        Verb::Links => links_verb(repo, &invocation.args, out),
        Verb::Fetch | Verb::Pull | Verb::Push | Verb::Sync => remote_verb(invocation, out),
    }
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
    let cursor = repo.log_cursor(repo.resolve(rev)?)?;
    cursor.skip(skip)?;
    for commit in cursor.next_page(repo, limit)? {
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
    let filter = CommitFilter {
        path: Some(path.to_owned()),
        ..CommitFilter::default()
    };
    let walk = repo.history_walk(repo.resolve(rev)?, filter)?;
    let page = walk.next_page(repo, limit, max_scan)?;
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
        path: None,
    };
    let walk = repo.history_walk(repo.resolve(rev)?, filter)?;
    let page = walk.next_page(repo, limit, max_scan)?;
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
    let (old_link, old_blob) = diff_side(repo.entry_at_path(old_tree, path)?, path)?;
    let (new_link, new_blob) = diff_side(repo.entry_at_path(new_tree, path)?, path)?;
    if old_link.is_none() && old_blob.is_none() && new_link.is_none() && new_blob.is_none() {
        bail!("'{path}' does not exist at {old_rev} or {new_rev}");
    }
    if let Some(commit) = old_link {
        writeln!(out, "-Subproject commit {commit}")?;
    }
    let code = if old_blob.is_some() || new_blob.is_some() {
        let diff = repo.diff_blobs(old_blob, new_blob, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)?;
        render_blob_diff(diff, path, out)?
    } else {
        ExitCode::SUCCESS
    };
    if let Some(commit) = new_link {
        writeln!(out, "+Subproject commit {commit}")?;
    }
    Ok(code)
}

/// Whether the working-tree file at `path` is gone, so its diff against HEAD is a deletion.
/// Any other stat failure is left for `diff_worktree_file` to report.
fn is_missing_from_disk(repo: &Repository, path: &str) -> bool {
    repo.work_dir().is_some_and(|work_dir| {
        std::fs::symlink_metadata(work_dir.join(path))
            .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
    })
}

/// Splits a diffed side into its gitlink id and its blob id. A gitlink names a commit in
/// another repository, so it has no blob to diff here and prints as git's
/// `Subproject commit` line instead, while a file on the other side diffs as an addition or
/// a deletion.
fn diff_side(entry: Option<TreeEntry>, path: &str) -> Result<(Option<Oid>, Option<Oid>)> {
    match entry {
        Some(entry) if entry.kind == EntryKind::Submodule => Ok((Some(entry.oid), None)),
        entry => Ok((None, file_oid(entry, path)?)),
    }
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
                    match line.eol {
                        LineEnd::Lf => out.push(b'\n'),
                        LineEnd::CrLf => out.extend_from_slice(b"\r\n"),
                        LineEnd::None => out.extend_from_slice(b"\n\\ No newline at end of file\n"),
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
    let args = split_args("blame", args, &["--since"], &[])?;
    args.expect(2, 2, "<rev> <path>")?;
    let [rev, path] = args.positionals[..] else {
        bail!("blame needs <rev> <path>\n\n{USAGE}");
    };
    let path = &canonical_path(path)?;
    let since = epoch(&args, "--since")?;
    let tip = repo.resolve(rev)?;
    let ranges = match repo.blame(tip, path, DEFAULT_BLOB_LIMIT, since)? {
        Blame::Ranges(ranges) => ranges,
        Blame::TooLarge { size, limit } => {
            writeln!(
                io::stderr().lock(),
                "'{path}' is {size} bytes, over the {limit} byte limit"
            )?;
            return Ok(ExitCode::from(EXIT_TOO_LARGE));
        }
    };
    let mut titles = std::collections::HashMap::new();
    for range in ranges {
        let commit = match titles.entry(range.commit) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(repo.commit(range.commit)?)
            }
        };
        writeln!(
            out,
            "{}{}\t{}\t{}\t{}\t{}",
            if range.boundary { "^" } else { "" },
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

/// `"Name <email>"`, as `git --author` takes it: one `<`, one `>` at the end, a non-empty
/// name and email, and no line break.
fn parse_author(raw: &str) -> Result<rc_git_guard::actions::Identity> {
    let malformed = || usage_error(format!("--author needs \"Name <email>\", got '{raw}'"));
    let Some((name, rest)) = raw.split_once('<') else {
        return Err(malformed());
    };
    let Some(email) = rest.strip_suffix('>') else {
        return Err(malformed());
    };
    let (name, email) = (name.trim(), email.trim());
    let stray = |part: &str| part.contains(['<', '>', '\n', '\r']);
    if name.is_empty() || email.is_empty() || stray(name) || stray(email) {
        return Err(malformed());
    }
    Ok(rc_git_guard::actions::Identity {
        name: name.to_owned(),
        email: email.to_owned(),
    })
}

/// `--author` is recorded as given, ahead of the repository's own `user.name`/`user.email`,
/// as `git commit --author` is; without it the repository's config decides.
fn author(args: &VerbArgs<'_>) -> Result<rc_git_guard::actions::Author> {
    let explicit = args.value(&["--author"]).map(parse_author).transpose()?;
    Ok(rc_git_guard::actions::Author {
        explicit,
        fallback: None,
    })
}

const MESSAGE_FLAGS: [&str; 4] = ["-m", "--message", "--author", "--confirm"];
const COMMIT_FLAGS: [&str; 6] = [
    "-m",
    "--message",
    "--author",
    "--confirm",
    "--expect-head",
    "--hunk",
];

/// One `--hunk <path>:<old_start>,<old_lines>,<new_start>,<new_lines>` value: the path and the
/// numbers of the `@@ -a,b +c,d @@` header `diff --worktree` printed for the hunk.
fn parse_hunk(raw: &str) -> Result<(String, rc_git_guard::actions::HunkStamp)> {
    let malformed = || {
        usage_error(format!(
            "--hunk needs <path>:<old_start>,<old_lines>,<new_start>,<new_lines>, got '{raw}'"
        ))
    };
    let Some((path, numbers)) = raw.rsplit_once(':') else {
        return Err(malformed());
    };
    let mut parsed = [0_u32; 4];
    let mut parts = numbers.split(',');
    for slot in &mut parsed {
        *slot = parts
            .next()
            .and_then(|part| part.parse().ok())
            .ok_or_else(malformed)?;
    }
    if path.is_empty() || parts.next().is_some() {
        return Err(malformed());
    }
    let [old_start, old_lines, new_start, new_lines] = parsed;
    let stamp = rc_git_guard::actions::HunkStamp {
        old_start,
        old_lines,
        new_start,
        new_lines,
    };
    Ok((canonical_path(path)?, stamp))
}

/// Every `--hunk` value grouped by path, in the order each path first appears, with each
/// path's hunks in file order.
fn hunk_selections(
    args: &VerbArgs<'_>,
) -> Result<Vec<(String, Vec<rc_git_guard::actions::HunkStamp>)>> {
    let mut grouped: Vec<(String, Vec<rc_git_guard::actions::HunkStamp>)> = Vec::new();
    for raw in args.values_of("--hunk") {
        let (path, stamp) = parse_hunk(raw)?;
        match grouped.iter_mut().find(|(known, _)| *known == path) {
            Some((_, stamps)) => stamps.push(stamp),
            None => grouped.push((path, vec![stamp])),
        }
    }
    for (_, stamps) in &mut grouped {
        stamps.sort_by_key(|stamp| (stamp.old_start, stamp.new_start));
        stamps.dedup();
    }
    Ok(grouped)
}

/// The whole files named as positionals plus the files `--hunk` picks hunks of. A path named
/// both ways is a usage error: the request would be ambiguous.
fn commit_selections(args: &VerbArgs<'_>) -> Result<Vec<rc_git_guard::actions::FileSelection>> {
    let whole = canonical_paths(&args.positionals)?;
    let hunks = hunk_selections(args)?;
    if whole.is_empty() && hunks.is_empty() {
        return Err(usage_error(format!(
            "commit needs at least one <path> or --hunk\n\n{USAGE}"
        )));
    }
    let mut selections = Vec::with_capacity(whole.len() + hunks.len());
    for (path, stamps) in hunks {
        if whole.contains(&path) {
            return Err(usage_error(format!(
                "commit names '{path}' both whole and with --hunk; pick one"
            )));
        }
        selections.push(rc_git_guard::actions::FileSelection::Hunks {
            path,
            hunks: stamps,
        });
    }
    for path in whole {
        selections.push(rc_git_guard::actions::FileSelection::Whole { path });
    }
    Ok(selections)
}

/// The HEAD commit the guard checks the commit against: the caller's `--expect-head`, so a
/// commit made elsewhere since they looked is refused, or else HEAD as read now (`None` when
/// the branch is unborn).
fn expected_head(repo: &Repository, flag: Option<&str>) -> Result<Option<rc_git_core::Oid>> {
    if let Some(hex) = flag {
        let oid = rc_git_core::Oid::from_hex(hex).map_err(|_| {
            usage_error(format!(
                "commit --expect-head takes a full commit id, not '{hex}'\n\n{USAGE}"
            ))
        })?;
        return Ok(Some(oid));
    }
    let head = repo.head().context("commit: read HEAD")?;
    Ok(head.target)
}

fn commit_verb(
    repo: &Repository,
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let args = split_args("commit", args, &COMMIT_FLAGS, &[])?;
    let selections = commit_selections(&args)?;
    let message = args
        .value(&["-m", "--message"])
        .ok_or_else(|| usage_error(format!("commit needs -m <msg>\n\n{USAGE}")))?;
    let expected = expected_head(repo, args.value(&["--expect-head"]))?;
    let done = rc_git_guard::actions::commit(
        repo_path,
        &rc_git_guard::Policy::default(),
        rc_git_guard::actions::CommitRequest {
            message,
            author: author(&args)?,
            expected_head: expected,
            selections: &selections,
        },
        args.value(&["--confirm"]),
    )?;
    writeln!(out, "{}\t{}", done.oid, done.branch)?;
    if !done.index_refreshed {
        writeln!(
            io::stderr().lock(),
            "warning: commit: .git/index was not updated; run `git reset -q -- <paths>` so \
             git status agrees with the commit"
        )?;
    }
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
        rc_git_guard::actions::StashRequest {
            paths: &canonical_paths(&args.positionals)?,
            message: args.value(&["-m", "--message"]),
            author: author(&args)?,
        },
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
    if challenge.typed_phrase().is_some() || args.has("--yes") {
        return Ok(());
    }
    Err(usage_error(format!(
        "{} needs --yes to confirm: {}\n\n{USAGE}",
        args.label,
        challenge.summary()
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
    let index = match args.positional(0) {
        Some(raw) => raw
            .parse()
            .map_err(|_| usage_error(format!("'{raw}' is not a stash index")))?,
        None => 0,
    };
    if let StashVerb::Pop = verb {
        let name = format!("stash@{{{index}}}");
        require_yes(&args, &rc_git_guard::Action::StashPop { name })?;
    }
    let expected = stash_oid_at(repo_path, index)?;
    let policy = rc_git_guard::Policy::default();
    let input = args.value(&["--confirm"]);
    match verb {
        StashVerb::Apply => {
            rc_git_guard::actions::stash_apply(repo_path, &policy, index, expected, input)?;
        }
        StashVerb::Pop => {
            let deleted =
                rc_git_guard::actions::stash_pop(repo_path, &policy, index, expected, input)?;
            if let Some(backup) = deleted.backup_ref {
                writeln!(out, "{backup}")?;
            }
        }
        StashVerb::Drop => {
            let deleted =
                rc_git_guard::actions::stash_drop(repo_path, &policy, index, expected, input)?;
            if let Some(backup) = deleted.backup_ref {
                writeln!(out, "{backup}")?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The stash commit at `stash@{index}` now. The CLI names an entry by position alone, so the
/// commit it read is the one the guard is told to expect.
fn stash_oid_at(repo_path: &std::path::Path, index: usize) -> Result<Oid> {
    Repository::open(repo_path)?
        .stash_list()?
        .into_iter()
        .find(|entry| entry.index == index)
        .map(|entry| entry.oid)
        .ok_or_else(|| anyhow::anyhow!("no stash entry at stash@{{{index}}}"))
}

/// Discarding cannot be undone and the guard asks no typed phrase for it, so the CLI's form of
/// the confirm button is an explicit `--yes` or `--confirm discard`. Prints one
/// `restored|deleted<TAB>path` line per path.
fn discard_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let args = split_args("discard", args, &["--confirm", "--hunk"], &["--yes"])?;
    let hunks = hunk_selections(&args)?;
    if hunks.is_empty() {
        args.expect(1, usize::MAX, "at least one <path>, or --hunk")?;
    } else if hunks.len() > 1 || !args.positionals.is_empty() {
        return Err(usage_error(format!(
            "discard --hunk takes the hunks of one file and no other path\n\n{USAGE}"
        )));
    }
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
    if let [(path, stamps)] = &hunks[..] {
        rc_git_guard::actions::discard_hunks(
            repo_path,
            &rc_git_guard::Policy::default(),
            path,
            stamps,
            confirm,
        )?;
        write!(out, "hunks\t{}\t", stamps.len())?;
        write_path(out, path.as_bytes());
        out.push(b'\n');
        return Ok(ExitCode::SUCCESS);
    }
    let outcomes = rc_git_guard::actions::discard_files(
        repo_path,
        &rc_git_guard::Policy::default(),
        &canonical_paths(&args.positionals)?,
        confirm,
    )?;
    for outcome in outcomes {
        let kind = match outcome.kind {
            rc_git_guard::actions::DiscardKind::Restored => "restored",
            rc_git_guard::actions::DiscardKind::Deleted => "deleted",
        };
        write!(out, "{kind}\t")?;
        write_path(out, outcome.path.as_bytes());
        out.push(b'\n');
    }
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
            host: None,
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
/// configuration without any network I/O. A detached `HEAD` has no branch upstream to read;
/// a fetch there still runs against `origin`, so it reads as [`Transport::Other`].
fn upstream_transport(repo_path: &std::path::Path) -> Result<Transport> {
    let status = match rc_git_guard::actions::upstream_status(repo_path) {
        Ok(status) => status,
        Err(ActionError::DetachedHead { .. }) => return Ok(Transport::Other),
        Err(err) => return Err(err.into()),
    };
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
    if outcome.unpublished {
        writeln!(out, "pull\t{}\tnothing upstream yet", outcome.branch)?;
    } else if outcome.already_up_to_date {
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
    if let Some(error) = &outcome.tracking_ref_error {
        writeln!(
            io::stderr().lock(),
            "warning: push landed, but the tracking ref was not moved: {error}"
        )?;
    }
    if outcome.already_up_to_date {
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
    let policy = rc_git_guard::Policy::default();
    let fetch = rc_git_guard::actions::fetch_upstream(repo_path, &policy, remote.auth(), cancel)?;
    write_fetch(out, &fetch)?;
    let pull = rc_git_guard::actions::pull_fast_forward(repo_path, &policy)?;
    write_pull(out, &pull)?;
    if pull.unpublished || rc_git_guard::actions::upstream_status(repo_path)?.ahead > 0 {
        let push = rc_git_guard::actions::push_upstream(repo_path, &policy, remote.auth(), cancel)?;
        write_push(out, &push)?;
    } else {
        writeln!(out, "push\t{}\tup to date", pull.branch)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Which kind of ref a create or delete verb manages.
#[derive(Clone, Copy)]
enum RefSort {
    Branch,
    Tag,
}

impl RefSort {
    fn full_name(self, name: &str) -> String {
        match self {
            Self::Branch => format!("refs/heads/{name}"),
            Self::Tag => format!("refs/tags/{name}"),
        }
    }
}

/// `branch-create` and `tag-create`: the guard allows creation outright and refuses to move a
/// ref that already exists.
fn ref_create_verb(
    repo_path: &std::path::Path,
    args: &[String],
    sort: RefSort,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let label = match sort {
        RefSort::Branch => "branch-create",
        RefSort::Tag => "tag-create",
    };
    let args = split_args(label, args, &[], &[])?;
    args.expect(1, 2, "<name> [rev]")?;
    let Some(name) = args.positional(0) else {
        bail!("{label} needs <name>\n\n{USAGE}");
    };
    let rev = args.positional(1).unwrap_or("HEAD");
    let policy = rc_git_guard::Policy::default();
    match sort {
        RefSort::Branch => rc_git_guard::actions::create_branch(repo_path, &policy, name, rev)?,
        RefSort::Tag => rc_git_guard::actions::create_tag(repo_path, &policy, name, rev)?,
    }
    writeln!(out, "created\t{}", sort.full_name(name))?;
    Ok(ExitCode::SUCCESS)
}

/// What deleting the `sort` ref `name` would need, as the guard plans it.
fn plan_delete(
    repo_path: &std::path::Path,
    policy: &rc_git_guard::Policy,
    sort: RefSort,
    name: &str,
) -> Result<rc_git_guard::Decision> {
    Ok(match sort {
        RefSort::Branch => {
            rc_git_guard::actions::plan_branch_delete(repo_path, policy, name)?.decision
        }
        RefSort::Tag => rc_git_guard::actions::plan_tag_delete(policy, name),
    })
}

/// `branch-delete` and `tag-delete`. The guard's plan runs first, so a deletion that needs the
/// name typed and got no `--confirm` is a usage error naming the phrase before anything is
/// read for the write; a denial, a checked-out branch or a wrong phrase is the guard's refusal.
fn ref_delete_verb(
    repo_path: &std::path::Path,
    args: &[String],
    sort: RefSort,
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let label = match sort {
        RefSort::Branch => "branch-delete",
        RefSort::Tag => "tag-delete",
    };
    let args = split_args(label, args, &["--confirm"], &[])?;
    args.expect(1, 1, "<name>")?;
    let Some(name) = args.positional(0) else {
        bail!("{label} needs <name>\n\n{USAGE}");
    };
    let policy = rc_git_guard::Policy::default();
    let input = args.value(&["--confirm"]);
    if let rc_git_guard::Decision::Confirm(challenge) = plan_delete(repo_path, &policy, sort, name)?
        && let Some(phrase) = challenge.typed_phrase()
        && input.is_none()
    {
        return Err(usage_error(format!(
            "{label} '{name}' needs --confirm {}: {}\n\n{USAGE}",
            shell_word(phrase),
            challenge.summary()
        )));
    }
    let deleted = match sort {
        RefSort::Branch => rc_git_guard::actions::delete_branch(repo_path, &policy, name, input)?,
        RefSort::Tag => rc_git_guard::actions::delete_tag(repo_path, &policy, name, input)?,
    };
    writeln!(out, "deleted\t{}", deleted.full_name)?;
    if let Some(backup) = deleted.backup_ref {
        writeln!(out, "backup\t{backup}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `plan-delete branch|tag <name>`: the guard's decision as one record, so a caller can see
/// what a deletion will ask for before running it.
fn plan_delete_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let args = split_args("plan-delete", args, &[], &[])?;
    args.expect(2, 2, "branch|tag <name>")?;
    let [kind, name] = args.positionals[..] else {
        bail!("plan-delete needs branch|tag <name>\n\n{USAGE}");
    };
    let sort = match kind {
        "branch" => RefSort::Branch,
        "tag" => RefSort::Tag,
        other => {
            return Err(usage_error(format!(
                "plan-delete takes branch or tag, not '{other}'\n\n{USAGE}"
            )));
        }
    };
    let decision = plan_delete(repo_path, &rc_git_guard::Policy::default(), sort, name)?;
    match decision {
        rc_git_guard::Decision::Allow => writeln!(out, "allow\t-\t-\tno\t-")?,
        rc_git_guard::Decision::Confirm(challenge) => writeln!(
            out,
            "confirm\t{}\t{}\t{}\t{}",
            risk_name(challenge.risk()),
            challenge.typed_phrase().map_or("-".into(), field),
            if challenge.backup_ref() { "yes" } else { "no" },
            field(challenge.summary())
        )?,
        rc_git_guard::Decision::Deny { reason } => {
            writeln!(out, "deny\t-\t-\tno\t{}", field(&reason))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn risk_name(risk: rc_git_guard::Risk) -> &'static str {
    match risk {
        rc_git_guard::Risk::Safe => "safe",
        rc_git_guard::Risk::Recoverable => "recoverable",
        rc_git_guard::Risk::Destructive => "destructive",
    }
}

/// `upstream`: the checked-out branch's upstream and how far apart the two are, read from the
/// repository alone, with no network I/O.
fn upstream_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    split_args("upstream", args, &[], &[])?.expect(0, 0, "no arguments")?;
    let status = rc_git_guard::actions::upstream_status(repo_path)?;
    if status.has_upstream {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            status.branch,
            field(&status.remote),
            field(&status.url),
            status.ahead,
            status.behind
        )?;
    } else {
        writeln!(out, "{}\tnone", status.branch)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The most the app highlights or scans for import links: `Highlighter.byteLimit` in
/// `RcGitKit`. A larger file stays plain there, so it exits 4 here.
const HIGHLIGHT_LIMIT: u64 = 2 * 1024 * 1024;

/// What `highlight` and `links` work on: a text blob's bytes and the language its name implies,
/// or the exit code for a blob they decline.
enum HighlightInput {
    Text {
        data: Vec<u8>,
        language: rc_git_highlight::Language,
    },
    Declined(ExitCode),
}

/// Reads `<rev> <path>` for `label`, picking the language from the file name with
/// `language_of`, and declines a binary blob or one over [`HIGHLIGHT_LIMIT`] as `cat` does.
fn highlight_input(
    repo: &Repository,
    label: &'static str,
    args: &[String],
    language_of: fn(&str) -> Option<rc_git_highlight::Language>,
) -> Result<HighlightInput> {
    let args = split_args(label, args, &[], &[])?;
    args.expect(2, 2, "<rev> <path>")?;
    let [rev, path] = args.positionals[..] else {
        bail!("{label} needs <rev> <path>\n\n{USAGE}");
    };
    let path = &canonical_path(path)?;
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let language = language_of(file_name)
        .with_context(|| format!("{label}: no language is known for '{path}'"))?;
    let root = repo.resolve_tree(rev)?;
    let entry = repo
        .entry_at_path(root, path)?
        .with_context(|| format!("'{path}' does not exist at {rev}"))?;
    if entry.kind == EntryKind::Tree {
        bail!("'{path}' is a directory");
    }
    match repo.read_blob(entry.oid, HIGHLIGHT_LIMIT)? {
        BlobContent::Text { data, .. } => Ok(HighlightInput::Text { data, language }),
        BlobContent::Binary { size } => {
            writeln!(io::stderr().lock(), "'{path}' is binary ({size} bytes)")?;
            Ok(HighlightInput::Declined(ExitCode::from(EXIT_BINARY)))
        }
        BlobContent::TooLarge { size, limit } => {
            writeln!(
                io::stderr().lock(),
                "'{path}' is {size} bytes, over the {limit} byte limit"
            )?;
            Ok(HighlightInput::Declined(ExitCode::from(EXIT_TOO_LARGE)))
        }
    }
}

/// The language the app highlights a file as: by extension or file name, else by the name as
/// a fence label, the way `rcgit_highlight` picks it.
fn highlight_language(name: &str) -> Option<rc_git_highlight::Language> {
    rc_git_highlight::Language::from_file_name(name)
        .or_else(|| rc_git_highlight::Language::from_fence(name))
}

fn highlight_verb(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let (data, language) = match highlight_input(repo, "highlight", args, highlight_language)? {
        HighlightInput::Text { data, language } => (data, language),
        HighlightInput::Declined(code) => return Ok(code),
    };
    for span in rc_git_highlight::highlight(&data, language, None)? {
        writeln!(
            out,
            "{}\t{}\t{}",
            span.start,
            span.end,
            class_name(span.class)
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn links_verb(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let (data, language) = match highlight_input(
        repo,
        "links",
        args,
        rc_git_highlight::Language::from_file_name,
    )? {
        HighlightInput::Text { data, language } => (data, language),
        HighlightInput::Declined(code) => return Ok(code),
    };
    for link in rc_git_highlight::import_links(&data, language)? {
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            link.start, link.end, link.specifier_start, link.specifier_end
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn class_name(class: rc_git_highlight::Class) -> &'static str {
    match class {
        rc_git_highlight::Class::Attribute => "attribute",
        rc_git_highlight::Class::Comment => "comment",
        rc_git_highlight::Class::Constant => "constant",
        rc_git_highlight::Class::Emphasis => "emphasis",
        rc_git_highlight::Class::Function => "function",
        rc_git_highlight::Class::Heading => "heading",
        rc_git_highlight::Class::Keyword => "keyword",
        rc_git_highlight::Class::Link => "link",
        rc_git_highlight::Class::Number => "number",
        rc_git_highlight::Class::Operator => "operator",
        rc_git_highlight::Class::Property => "property",
        rc_git_highlight::Class::Punctuation => "punctuation",
        rc_git_highlight::Class::String => "string",
        rc_git_highlight::Class::Strong => "strong",
        rc_git_highlight::Class::Type => "type",
        rc_git_highlight::Class::Variable => "variable",
    }
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
    fn an_agent_that_refuses_to_sign_exits_as_authentication() {
        let err = anyhow::Error::new(crate::ActionError::AgentSignRefused {
            op: "fetch",
            host: "github.com".to_owned(),
            key: "'work@laptop' (SHA256:abc)".to_owned(),
            agent: "/tmp/agent.sock".to_owned(),
        });
        assert_eq!(crate::exit_code_for(&err), crate::EXIT_AUTH);
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
    #[test]
    fn confirm_hint_quotes_a_phrase_only_when_the_shell_would_split_it() {
        assert_eq!(crate::shell_word("feature/x-1.2"), "feature/x-1.2");
        assert_eq!(crate::shell_word("a$b"), "'a$b'");
        assert_eq!(crate::shell_word("it's"), r"'it'\''s'");
        assert_eq!(crate::shell_word(""), "''");
    }
}
