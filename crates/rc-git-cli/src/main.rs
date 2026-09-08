//! `rc-git`: the command-line face of `rc-git-core`.
//!
//! Output is tab-separated so scripts and agents can parse it. `--time` prints how long the
//! repository open and the command took, which is how the budgets in `docs/performance.md`
//! are checked without the GUI.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rc_git_core::{
    BlobContent, BlobDiff, ChangeKind, CommitFilter, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT,
    DiffLineKind, EntryKind, RefKind, Repository, WorktreeStatus,
};

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
  diff    <repo> <old> <new> [path]       changed files between two revisions, or one file's hunks
  diff    <repo> --worktree <path>        one working-tree file's hunks against HEAD
  blame   <repo> <rev> <path>             which commit introduced each line
  status  <repo> [dir]                    one working-tree directory against HEAD
  changes <repo> [--max-scan N]           every changed working-tree path against HEAD
  stash   <repo>                          stash entries, newest first
  resolve <repo> <rev>                    object id a revision names

  commit      <repo> -m <msg> [--author \"Name <email>\"] <path>...  commit files to the branch
  stash-push  <repo> [-m <msg>] [--author \"Name <email>\"] <path>... stash files, restore to HEAD
  stash-apply <repo> [n]                  write stash@{n} back to the working tree
  stash-pop   <repo> [n]                  apply stash@{n}, then drop it
  stash-drop  <repo> <n> --confirm drop   delete stash@{n} without applying
  discard     <repo> --confirm discard <path>...  restore files to HEAD, delete untracked

exit codes: 0 ok, 1 error, 2 usage, 3 binary file, 4 file over limit
";

const EXIT_ERROR: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_BINARY: u8 = 3;
const EXIT_TOO_LARGE: u8 = 4;

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
    Stash,
    Resolve,
    Commit,
    StashPush,
    StashApply,
    StashPop,
    StashDrop,
    Discard,
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
            "stash" => Some(Self::Stash),
            "resolve" => Some(Self::Resolve),
            "commit" => Some(Self::Commit),
            "stash-push" => Some(Self::StashPush),
            "stash-apply" => Some(Self::StashApply),
            "stash-pop" => Some(Self::StashPop),
            "stash-drop" => Some(Self::StashDrop),
            "discard" => Some(Self::Discard),
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
            Self::Stash => "stash",
            Self::Resolve => "resolve",
            Self::Commit => "commit",
            Self::StashPush => "stash-push",
            Self::StashApply => "stash-apply",
            Self::StashPop => "stash-pop",
            Self::StashDrop => "stash-drop",
            Self::Discard => "discard",
        }
    }
}

struct Invocation {
    time: bool,
    verb: Verb,
    repo: PathBuf,
    args: Vec<String>,
}

fn main() -> ExitCode {
    let invocation = match parse(std::env::args().skip(1).collect()) {
        Ok(invocation) => invocation,
        Err(message) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "error: {message}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match run(&invocation) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(io::stderr().lock(), "error: {err:#}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn parse(mut args: Vec<String>) -> std::result::Result<Invocation, String> {
    let time = args.first().is_some_and(|a| a == "--time");
    if time {
        args.remove(0);
    }
    if args.first().is_some_and(|a| a == "--help" || a == "-h") {
        return Err("help requested".to_owned());
    }
    if args.len() < 2 {
        return Err("expected <command> <repo>".to_owned());
    }
    let command = args.remove(0);
    let verb = Verb::parse(&command).ok_or_else(|| format!("unknown command '{command}'"))?;
    let repo = PathBuf::from(args.remove(0));
    Ok(Invocation {
        time,
        verb,
        repo,
        args,
    })
}

fn run(invocation: &Invocation) -> Result<ExitCode> {
    let opened_at = Instant::now();
    let repo = Repository::open(&invocation.repo)?;
    let open_elapsed = opened_at.elapsed();

    let started_at = Instant::now();
    let mut out = Vec::new();
    let code = match invocation.verb {
        Verb::Head => head(&repo, &mut out)?,
        Verb::Refs => refs(&repo, &mut out)?,
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
        Verb::Stash => stash(&repo, &mut out)?,
        Verb::Resolve => resolve(&repo, &invocation.args, &mut out)?,
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
        Verb::Discard => discard_verb(&invocation.repo, &invocation.args, &mut out)?,
    };
    let command_elapsed = started_at.elapsed();

    io::stdout()
        .lock()
        .write_all(&out)
        .context("write to stdout")?;
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

fn head(repo: &Repository, out: &mut Vec<u8>) -> Result<ExitCode> {
    let head = repo.head()?;
    let name = head.ref_name.as_deref().unwrap_or("detached");
    let target = head
        .target
        .map_or_else(|| "unborn".to_owned(), |oid| oid.to_hex());
    writeln!(out, "{name}\t{target}")?;
    Ok(ExitCode::SUCCESS)
}

fn refs(repo: &Repository, out: &mut Vec<u8>) -> Result<ExitCode> {
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
    let rev = args.first().map_or("HEAD", String::as_str);
    let path = args.get(1).map_or("", String::as_str);
    let root = repo.resolve_tree(rev)?;
    let tree = if path.trim_matches('/').is_empty() {
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
        let kind = match entry.kind {
            EntryKind::Tree => "tree",
            EntryKind::Blob => "blob",
            EntryKind::ExecutableBlob => "exec",
            EntryKind::Symlink => "link",
            EntryKind::Submodule => "submodule",
        };
        writeln!(out, "{kind}\t{}\t{}", entry.oid, entry.name_lossy())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn cat(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let [rev, path, rest @ ..] = args else {
        bail!("cat needs <rev> <path>\n\n{USAGE}");
    };
    let limit = match rest.first() {
        Some(raw) => raw
            .parse::<u64>()
            .with_context(|| format!("limit '{raw}' is not a number"))?,
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
    let mut rev = "HEAD";
    let mut limit = 20;
    let mut skip = 0;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-n" => limit = number(iter.next(), "-n")?,
            "--skip" => skip = number(iter.next(), "--skip")?,
            other => rev = other,
        }
    }
    let tip = repo.resolve(rev)?;
    for commit in repo.log(tip, skip, limit)? {
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            commit.oid, commit.author_time, commit.author_name, commit.title
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn filelog(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let [path, rest @ ..] = args else {
        bail!("filelog needs <path>\n\n{USAGE}");
    };
    let mut rev = "HEAD";
    let mut limit = 20;
    let mut max_scan = 5000;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-n" => limit = number(iter.next(), "-n")?,
            "--max-scan" => max_scan = number(iter.next(), "--max-scan")?,
            other => rev = other,
        }
    }
    let tip = repo.resolve(rev)?;
    let mut printed = 0;
    let mut scan_skip = 0;
    loop {
        let page = repo.path_history(tip, path, scan_skip, limit - printed, max_scan)?;
        for commit in &page.commits {
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                commit.oid, commit.author_time, commit.author_name, commit.title
            )?;
        }
        printed += page.commits.len();
        scan_skip += page.scanned;
        if page.complete || printed >= limit {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn search(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let mut rev = "HEAD";
    let mut limit = 20;
    let mut max_scan = 10_000;
    let mut filter = CommitFilter::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-n" => limit = number(iter.next(), "-n")?,
            "--max-scan" => max_scan = number(iter.next(), "--max-scan")?,
            "--message" => filter.message = Some(text_value(iter.next(), "--message")?),
            "--author" => filter.author = Some(text_value(iter.next(), "--author")?),
            "--since" => filter.after = Some(epoch(iter.next(), "--since")?),
            "--until" => filter.before = Some(epoch(iter.next(), "--until")?),
            other => rev = other,
        }
    }
    let tip = repo.resolve(rev)?;
    let mut printed = 0;
    let mut scan_skip = 0;
    loop {
        let page = repo.search_commits(tip, &filter, scan_skip, limit - printed, max_scan)?;
        for commit in &page.commits {
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                commit.oid, commit.author_time, commit.author_name, commit.title
            )?;
        }
        printed += page.commits.len();
        scan_skip += page.scanned;
        if page.complete || printed >= limit {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn text_value(value: Option<&String>, flag: &str) -> Result<String> {
    value
        .cloned()
        .with_context(|| format!("{flag} needs a value"))
}

fn epoch(value: Option<&String>, flag: &str) -> Result<i64> {
    let raw = value.with_context(|| format!("{flag} needs seconds since the Unix epoch"))?;
    raw.parse()
        .with_context(|| format!("{flag} value '{raw}' is not a number"))
}

fn show(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let Some(rev) = args.first() else {
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
        summary.author_name, summary.author_email, summary.author_time, summary.author_offset
    )?;
    writeln!(
        out,
        "committer\t{} <{}>\t{}\t{}",
        detail.committer_name,
        detail.committer_email,
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
    if args.first().is_some_and(|arg| arg == "--worktree") {
        let [_, path] = args else {
            bail!("diff --worktree needs <path>\n\n{USAGE}");
        };
        let tree = repo.resolve_tree("HEAD")?;
        let old = repo.entry_at_path(tree, path)?.map(|entry| entry.oid);
        let diff = repo.diff_worktree_file(old, path, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)?;
        return render_blob_diff(diff, path, out);
    }
    let [old_rev, new_rev, rest @ ..] = args else {
        bail!("diff needs <old> <new>\n\n{USAGE}");
    };
    let old = repo.resolve(old_rev)?;
    let new = repo.resolve(new_rev)?;
    let Some(path) = rest.first() else {
        for change in repo.diff_trees(Some(old), new)? {
            let letter = match change.kind {
                ChangeKind::Added => "A",
                ChangeKind::Deleted => "D",
                ChangeKind::Modified => "M",
                ChangeKind::Renamed => "R",
                ChangeKind::Copied => "C",
            };
            write!(out, "{letter}\t{}", change.path)?;
            if let Some(source) = &change.source_path {
                write!(out, "\t{source}")?;
            }
            writeln!(out)?;
        }
        return Ok(ExitCode::SUCCESS);
    };
    let old_tree = repo.tree_of(old)?;
    let new_tree = repo.tree_of(new)?;
    let old_blob = repo.entry_at_path(old_tree, path)?.map(|entry| entry.oid);
    let new_blob = repo.entry_at_path(new_tree, path)?.map(|entry| entry.oid);
    if old_blob.is_none() && new_blob.is_none() {
        bail!("'{path}' does not exist at {old_rev} or {new_rev}");
    }
    let diff = repo.diff_blobs(old_blob, new_blob, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)?;
    render_blob_diff(diff, path, out)
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
    let [rev, path] = args else {
        bail!("blame needs <rev> <path>\n\n{USAGE}");
    };
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
            range.commit, range.start_line, range.line_count, commit.author_name, commit.title
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn changes(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let mut max_scan = 200_000;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--max-scan" {
            max_scan = number(iter.next(), "--max-scan")?;
        } else {
            bail!("changes does not take '{arg}'\n\n{USAGE}");
        }
    }
    let tree = repo.resolve_tree("HEAD")?;
    let report = repo.worktree_status(tree, max_scan)?;
    for entry in &report.entries {
        let letter = match entry.status {
            WorktreeStatus::Unchanged => " ",
            WorktreeStatus::Modified => "M",
            WorktreeStatus::Untracked => "?",
            WorktreeStatus::Ignored => "!",
            WorktreeStatus::Deleted => "D",
        };
        writeln!(out, "{letter}\t{}", String::from_utf8_lossy(&entry.path))?;
    }
    if report.truncated {
        writeln!(
            io::stderr().lock(),
            "truncated after scanning {} entries",
            report.scanned
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn stash(repo: &Repository, out: &mut Vec<u8>) -> Result<ExitCode> {
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
    let dir = args.first().map_or("", String::as_str);
    let tree = repo.resolve_tree("HEAD")?;
    let listing = repo.worktree_entries(tree, dir)?;
    for entry in &listing.entries {
        let letter = match entry.status {
            WorktreeStatus::Unchanged => " ",
            WorktreeStatus::Modified => "M",
            WorktreeStatus::Untracked => "?",
            WorktreeStatus::Ignored => "!",
            WorktreeStatus::Deleted => "D",
        };
        let kind = match entry.kind {
            EntryKind::Tree => "tree",
            EntryKind::Blob => "blob",
            EntryKind::ExecutableBlob => "exec",
            EntryKind::Symlink => "link",
            EntryKind::Submodule => "submodule",
        };
        writeln!(
            out,
            "{letter}\t{kind}\t{}",
            String::from_utf8_lossy(&entry.name)
        )?;
    }
    for missing in &listing.missing {
        writeln!(out, "D\t-\t{}", missing.name_lossy())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Flags shared by the mutating verbs: `-m`, `--author`, `--confirm`, and positional paths.
#[derive(Default)]
struct MutationArgs {
    message: Option<String>,
    author: Option<rc_git_guard::actions::Identity>,
    confirm: Option<String>,
    paths: Vec<String>,
}

fn parse_mutation(args: &[String]) -> Result<MutationArgs> {
    let mut parsed = MutationArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-m" | "--message" => {
                parsed.message = Some(text_value(iter.next(), "-m")?);
            }
            "--author" => {
                let raw = text_value(iter.next(), "--author")?;
                parsed.author = Some(parse_author(&raw)?);
            }
            "--confirm" => {
                parsed.confirm = Some(text_value(iter.next(), "--confirm")?);
            }
            other => parsed.paths.push(other.to_owned()),
        }
    }
    Ok(parsed)
}

/// `"Name <email>"`, as `git --author` takes it.
fn parse_author(raw: &str) -> Result<rc_git_guard::actions::Identity> {
    let Some((name, rest)) = raw.split_once('<') else {
        bail!("--author needs \"Name <email>\", got '{raw}'");
    };
    let Some(email) = rest.strip_suffix('>') else {
        bail!("--author needs \"Name <email>\", got '{raw}'");
    };
    Ok(rc_git_guard::actions::Identity {
        name: name.trim().to_owned(),
        email: email.trim().to_owned(),
    })
}

fn commit_verb(
    repo: &Repository,
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let parsed = parse_mutation(args)?;
    let message = parsed
        .message
        .with_context(|| format!("commit needs -m <msg>\n\n{USAGE}"))?;
    if parsed.paths.is_empty() {
        bail!("commit needs at least one <path>\n\n{USAGE}");
    }
    let expected = repo.resolve("HEAD").ok().map(|oid| oid.to_hex());
    let selections: Vec<rc_git_guard::actions::FileSelection> = parsed
        .paths
        .iter()
        .map(|path| rc_git_guard::actions::FileSelection::Whole { path: path.clone() })
        .collect();
    let done = rc_git_guard::actions::commit(
        repo_path,
        &rc_git_guard::Policy::default(),
        &message,
        parsed.author,
        expected.as_deref(),
        &selections,
        parsed.confirm.as_deref(),
    )?;
    writeln!(out, "{}\t{}", done.oid, done.branch)?;
    Ok(ExitCode::SUCCESS)
}

fn stash_push_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let parsed = parse_mutation(args)?;
    if parsed.paths.is_empty() {
        bail!("stash-push needs at least one <path>\n\n{USAGE}");
    }
    let saved = rc_git_guard::actions::stash_push(
        repo_path,
        &rc_git_guard::Policy::default(),
        &parsed.paths,
        parsed.message.as_deref(),
        parsed.author,
        parsed.confirm.as_deref(),
    )?;
    writeln!(out, "{}\t{}", saved.oid, saved.message)?;
    Ok(ExitCode::SUCCESS)
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
    let parsed = parse_mutation(args)?;
    let index = match parsed.paths.first() {
        Some(raw) => raw
            .parse()
            .with_context(|| format!("'{raw}' is not a stash index"))?,
        None => 0,
    };
    let policy = rc_git_guard::Policy::default();
    let input = parsed.confirm.as_deref();
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

fn discard_verb(
    repo_path: &std::path::Path,
    args: &[String],
    out: &mut Vec<u8>,
) -> Result<ExitCode> {
    let parsed = parse_mutation(args)?;
    if parsed.paths.is_empty() {
        bail!("discard needs at least one <path>\n\n{USAGE}");
    }
    rc_git_guard::actions::discard_files(
        repo_path,
        &rc_git_guard::Policy::default(),
        &parsed.paths,
        parsed.confirm.as_deref(),
    )?;
    let _ = out;
    Ok(ExitCode::SUCCESS)
}

fn number(value: Option<&String>, flag: &str) -> Result<usize> {
    let raw = value.with_context(|| format!("{flag} needs a number"))?;
    raw.parse()
        .with_context(|| format!("{flag} value '{raw}' is not a number"))
}

fn resolve(repo: &Repository, args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let Some(rev) = args.first() else {
        bail!("resolve needs <rev>\n\n{USAGE}");
    };
    writeln!(out, "{}", repo.resolve(rev)?)?;
    Ok(ExitCode::SUCCESS)
}
