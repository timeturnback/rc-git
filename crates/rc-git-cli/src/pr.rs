//! `rc-git pr`: a repository's open pull requests, one in full, its checks, and the files it
//! changes, read through the app's accounts and keychain (the same stores `auth` uses) and
//! kit's live service, so the CLI picks the account exactly as the app does: the one account
//! on the remote's host, else the one chosen for the repository, else `--login`. Every text
//! the server sent is printed as the guard neutralized it: one-line fields carry no tab, line
//! break, control or bidi character, and the description's lines are printed one record each,
//! with a tab in them shown as `\u{9}` so a line stays one field.
//!
//! `pr diff` reads the changes from local objects. It fetches the pull request's head only
//! with `--fetch`, over the remote's git transport with the remote options' credentials; the
//! GitHub token stays with the API reads.
//!
//! `pr merge` merges through the guard as the app does. `--head` and `--base` are required: they
//! are the stamp of what the caller saw (`pr view` prints both), and a pull request whose head
//! moved or whose base changed since refuses before anything is sent. Without `--yes` it prints
//! what would be merged and merges nothing; with it, the challenge goes to stderr before the
//! request leaves. It sends one request at most and never sends it again; an answer that was
//! lost ends with its own exit code and tells the caller to look at the pull request on GitHub,
//! and a merge GitHub says landed on another base than `--base` ends with its own too. Once the
//! request has left, a second Ctrl+C no longer ends the command, which reads the outcome first.
//! `pr merge --admin` is the admin merge past branch protection: `--yes` does nothing for it,
//! and without `--confirm` it prints what would be skipped and the plan's fingerprint and sends
//! nothing. The real run needs `--confirm` with the number and `--plan` with that fingerprint,
//! which the guard's fresh decision must still have; the `--confirm` text goes to the guard as
//! typed, and the guard alone checks it against the number.
//!
//! `pr auto-merge` (`--enable --method m`, or `--disable`) and `pr rerun` follow the plain
//! merge: `--head` and `--base` are the stamp, `--yes` the confirm, the challenge goes to stderr
//! before anything leaves, and once a request has left a second Ctrl+C no longer ends the
//! command. A re-run sends one request per failed run and prints how each ended.
//!
//! `pr logs` prints one GitHub Actions job's log (`--check <name>` or `--job <id>`): the guard
//! matches the job to the head just read (or to `--head`, which must still be the head) before
//! anything is downloaded, and stdout carries the neutralized log alone, with what was left out
//! on stderr. Neither the token nor the download address is ever printed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use rc_git_core::{ChangeKind, Repository};
use rc_git_guard::actions::github::pull_request_remote;
use rc_git_guard::actions::pull_head_ref;
use rc_git_kit::canonical_path;
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore};
use rc_git_kit::platform::credentials::{CredentialKind, CredentialStore, MemoryCredentialStore};
use rc_git_kit::platform::host_keys::{HostKeyStore, MemoryHostKeyStore};
use rc_git_kit::pull_requests::auto_merge::{AutoMergeFlow, AutoMergeIntent};
use rc_git_kit::pull_requests::changes::{ChangesRead, ComparedWith, FetchProblem, HeadFetched};
use rc_git_kit::pull_requests::checks::{self, check_state, ordered, row_state};
use rc_git_kit::pull_requests::copy::{
    auto_merge_note, check_label, log_cut_notes, log_stop_note, merge_method_word, merge_note,
    merge_stop_message, mergeable_label, problem, rerun_note, review_label, state_label,
    stop_message,
};
use rc_git_kit::pull_requests::flow::{ActionAsk, ActionStop, Ended, Phase};
use rc_git_kit::pull_requests::link::{Linked, RepositoryLink};
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::logs::{LogAsk, LogCheck, LogStop, log_check_of};
use rc_git_kit::pull_requests::merge::{
    MergeAsk, MergeEnd, MergeFlow, MergePhase, MergeStop, PlanAsk,
};
use rc_git_kit::pull_requests::rerun::{RerunFlow, RerunIntent};
use rc_git_kit::pull_requests::{
    ActionRefusal, AutoMergeChange, AutoMergeOutcome, Bypassed, Check, Decision, JobId, MergeKind,
    MergeMethod, MergePlan, MergeRefusal, PageCursor, PullRequestDetail, PullRequestService,
    PullRequestStamp, PullRequestState, PullRequestSummary, ReadError, ReviewDecision, RunOutcome,
    ServerLink,
};

use crate::{
    EXIT_AUTH, EXIT_ERROR, EXIT_HOSTKEY, EXIT_INTERRUPTED, EXIT_MERGE_OTHER_BASE,
    EXIT_MERGE_REFUSED, EXIT_MERGE_UNKNOWN, EXIT_NOT_CONFIRMED, Records, Transport, UsageError,
    VerbArgs, credentials_for, split_args, write_path,
};

/// A failed read, with the exit code it ends with.
#[derive(Debug)]
pub(crate) struct ReadFailure {
    message: String,
    code: u8,
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReadFailure {}

impl ReadFailure {
    /// The exit code.
    pub(crate) fn code(&self) -> u8 {
        self.code
    }
}

fn failure(error: &ReadError) -> anyhow::Error {
    let code = match error {
        ReadError::SignedOut { .. } => EXIT_AUTH,
        ReadError::Cancelled => EXIT_INTERRUPTED,
        ReadError::RateLimited { .. } | ReadError::Failed(_) => EXIT_ERROR,
    };
    ReadFailure {
        message: problem(error, rc_git_kit::github::unix_now()),
        code,
    }
    .into()
}

/// Runs `rc-git pr <args>`, timing it when `time` is set.
pub(crate) fn run(args: &[String], time: bool, out: &mut Vec<u8>) -> Result<ExitCode> {
    let Some((verb, rest)) = args.split_first() else {
        return Err(UsageError(
            "pr needs list, view, checks, logs, diff, merge, auto-merge or rerun".to_owned(),
        )
        .into());
    };
    let started = Instant::now();
    let result = match verb.as_str() {
        "list" => list(rest, out),
        "view" => view(rest, out),
        "checks" => checks(rest, out),
        "logs" => logs(rest, out),
        "diff" => diff(rest, out),
        "merge" => merge(rest, out),
        "auto-merge" => auto_merge(rest, out),
        "rerun" => rerun(rest, out),
        other => Err(UsageError(format!(
            "pr has no '{other}'; use list, view, checks, logs, diff, merge, auto-merge or \
             rerun"
        ))
        .into()),
    };
    if time {
        use std::io::Write as _;
        writeln!(
            std::io::stderr().lock(),
            "time: pr {verb} {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        )?;
    }
    result
}

const VALUED: [&str; 3] = ["--login", "--ssh-dir", "--after"];

/// The repository's tab path (its working directory, canonical), as the app keys its account
/// choice.
fn repo_path(args: &VerbArgs<'_>, index: usize) -> Result<String> {
    let given = PathBuf::from(args.positional(index).unwrap_or("."));
    let repository = Repository::open(&given)?;
    let root = repository
        .work_dir()
        .map_or_else(|| given.clone(), std::path::Path::to_path_buf);
    Ok(canonical_path::resolve(&root)
        .to_string_lossy()
        .into_owned())
}

/// The service and the link of the repository the arguments name.
fn linked(args: &VerbArgs<'_>, index: usize) -> Result<(LivePullRequests, Linked)> {
    let path = repo_path(args, index)?;
    let stores = crate::auth::stores()?;
    let tokens: Arc<dyn ApiTokenStore> = Arc::new(stores.tokens);
    let ssh_dir = args.value(&["--ssh-dir"]).map(PathBuf::from);
    let service = LivePullRequests::new(stores.settings, tokens).with_ssh_dir(ssh_dir);
    let login = args.value(&["--login"]);
    let link = match login {
        Some(login) => service.link_choosing(&path, login),
        None => service.link(&path),
    };
    match link {
        RepositoryLink::Linked(linked) => {
            if let Some(login) = login.filter(|login| *login != linked.account.key.login()) {
                bail!(
                    "--login {login}: the repository's host has {} signed in, not {login}",
                    linked.account.key
                );
            }
            Ok((service, linked))
        }
        RepositoryLink::Hidden => bail!(
            "{path}: its remote names no GitHub repository on a host with a signed-in account"
        ),
        RepositoryLink::SignIn { host } => Err(ReadFailure {
            message: format!(
                "no GitHub account is signed in on {host}; run rc-git auth login --host {host}"
            ),
            code: EXIT_AUTH,
        }
        .into()),
        RepositoryLink::ChooseAccount { host, accounts } => {
            let logins: Vec<&str> = accounts.iter().map(AccountKey::login).collect();
            Err(anyhow!(
                "several accounts are signed in on {host} ({}); pass --login with one of them, \
                 or choose one for this repository in RCGit",
                logins.join(", ")
            ))
        }
    }
}

fn number(args: &VerbArgs<'_>) -> Result<u64> {
    let text = args.positional(0).unwrap_or_default();
    text.parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| UsageError(format!("'{text}' is not a pull request number")).into())
}

/// `-` for nothing.
fn or_dash(text: Option<&str>) -> &str {
    match text {
        Some(text) if !text.is_empty() => text,
        Some(_) | None => "-",
    }
}

fn row(summary: &PullRequestSummary) -> String {
    let checks = row_state(summary.checks).map_or("-", check_label);
    format!(
        "pr\t{}\t{}\t{checks}\t{}\t{}\t{}\t{}\t{}",
        summary.number,
        state_label(summary.state, summary.draft),
        review_label(summary.review),
        or_dash(summary.author.as_deref()),
        summary.head_ref,
        summary.base_ref,
        summary.title
    )
}

fn list(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    use std::io::Write as _;
    let args = split_args("pr list", raw, &VALUED, &[])?;
    args.expect(0, 1, "[repo]")?;
    let after = match args.value(&["--after"]) {
        Some(text) => Some(
            PageCursor::from_text(text)
                .ok_or_else(|| UsageError(format!("--after '{text}' is not a page cursor")))?,
        ),
        None => None,
    };
    let (service, linked) = linked(&args, 0)?;
    let cancel = crate::interruptible_cancel()?;
    let page = service
        .list(&linked, after.as_ref(), &cancel)
        .map_err(|error| failure(&error))?;
    for summary in &page.pull_requests {
        writeln!(out, "{}", row(summary))?;
    }
    if let Some(next) = &page.next {
        writeln!(out, "more\t{}", next.as_str())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn detail(raw: &[String], label: &'static str) -> Result<PullRequestDetail> {
    let args = split_args(label, raw, &VALUED[..2], &[])?;
    args.expect(1, 2, "<number> [repo]")?;
    let number = number(&args)?;
    let (service, linked) = linked(&args, 1)?;
    let cancel = crate::interruptible_cancel()?;
    service
        .detail(&linked, number, &cancel)
        .map_err(|error| failure(&error))
}

fn link_text(link: Option<&ServerLink>) -> &str {
    link.map_or("-", ServerLink::url)
}

fn view(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    use std::io::Write as _;
    let detail = detail(raw, "pr view")?;
    let summary = &detail.summary;
    writeln!(out, "number\t{}", summary.number)?;
    writeln!(out, "title\t{}", summary.title)?;
    writeln!(out, "state\t{}", state_label(summary.state, summary.draft))?;
    writeln!(out, "author\t{}", or_dash(summary.author.as_deref()))?;
    let owner = or_dash(summary.head_owner.as_deref());
    writeln!(
        out,
        "head\t{owner}\t{}\t{}",
        summary.head_ref, summary.head_oid
    )?;
    writeln!(out, "base\t{}", summary.base_ref)?;
    writeln!(out, "review\t{}", review_label(summary.review))?;
    writeln!(out, "mergeable\t{}", mergeable_label(detail.mergeable))?;
    writeln!(out, "url\t{}", link_text(detail.url.as_ref()))?;
    writeln!(out, "checks\t{}", checks::summary(&detail.checks))?;
    for line in detail.body.lines() {
        writeln!(out, "{}", body_record(line))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn checks(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    use std::io::Write as _;
    let detail = detail(raw, "pr checks")?;
    for check in ordered(&detail.checks.checks) {
        let required = match check.required {
            Some(true) => "required",
            Some(false) => "optional",
            None => "-",
        };
        writeln!(
            out,
            "check\t{}\t{required}\t{}\t{}\t{}",
            check_label(check_state(&check.progress)),
            check.name,
            link_text(check.details.as_ref()),
            or_dash(check.description.as_deref())
        )?;
    }
    let unread = detail
        .checks
        .total
        .saturating_sub(detail.checks.checks.len());
    if unread > 0 {
        writeln!(out, "unread\t{unread}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `pr logs`'s valued flags.
const LOGS_VALUED: [&str; 5] = ["--login", "--ssh-dir", "--check", "--job", "--head"];

/// Which check `pr logs` reads: by name, or by job id.
enum LogPick<'a> {
    Name(&'a str),
    Job(JobId),
}

/// The check `--check` or `--job` names: exactly one of them.
fn log_pick<'a>(args: &'a VerbArgs<'_>) -> Result<LogPick<'a>> {
    match (args.value(&["--check"]), args.value(&["--job"])) {
        (Some(name), None) => Ok(LogPick::Name(name)),
        (None, Some(id)) => id
            .parse::<u64>()
            .ok()
            .and_then(JobId::new)
            .map(LogPick::Job)
            .ok_or_else(|| UsageError(format!("--job '{id}' is not a job id")).into()),
        (Some(_), Some(_)) | (None, None) => Err(UsageError(
            "pr logs needs exactly one of --check <name> or --job <id>".to_owned(),
        )
        .into()),
    }
}

/// The check of `detail` that `pick` names, when kit offers its log: one GitHub Actions job of
/// the head, completed.
fn log_target(detail: &PullRequestDetail, pick: &LogPick<'_>) -> Result<LogCheck> {
    let number = detail.summary.number;
    let named: Vec<&Check> = detail
        .checks
        .checks
        .iter()
        .filter(|check| match pick {
            LogPick::Name(name) => check.name == *name && check.actions.is_some(),
            LogPick::Job(job) => check.actions.is_some_and(|ids| ids.job == job.get()),
        })
        .collect();
    let [check] = named.as_slice() else {
        let jobs: Vec<String> = named
            .iter()
            .filter_map(|check| check.actions.map(|ids| ids.job.to_string()))
            .collect();
        let what = match pick {
            LogPick::Name(name) => format!("named '{name}'"),
            LogPick::Job(job) => format!("with job id {job}"),
        };
        if jobs.is_empty() {
            bail!("#{number}'s head has no GitHub Actions check {what}; pr checks lists them");
        }
        bail!(
            "#{number}'s head has {} GitHub Actions checks {what} (jobs {}); pass --job",
            jobs.len(),
            jobs.join(", ")
        );
    };
    log_check_of(detail, check).ok_or_else(|| {
        anyhow!(
            "check '{}' of #{number} has not finished, or GitHub named it with ids rc-git does not \
             read; its log is not offered",
            check.name
        )
    })
}

/// A log read that showed no log, with the exit code it ends with: 11 for a job rc-git would not
/// read, as the other verbs' refusals.
fn log_stopped(stop: &LogStop, check: &LogCheck) -> anyhow::Error {
    let code = match stop {
        LogStop::NotMatched(_) | LogStop::NotFinished => EXIT_MERGE_REFUSED,
        LogStop::Read(ReadError::SignedOut { .. }) => EXIT_AUTH,
        LogStop::Read(ReadError::Cancelled) => EXIT_INTERRUPTED,
        LogStop::Read(ReadError::RateLimited { .. } | ReadError::Failed(_)) => EXIT_ERROR,
    };
    ReadFailure {
        message: log_stop_note(stop, &check.name, rc_git_kit::github::unix_now()),
        code,
    }
    .into()
}

/// Prints the log of one GitHub Actions job of pull request `<n>`'s head, read through the guard
/// as the app reads it: stdout carries the neutralized log alone, one line per line, and stderr
/// one `truncated<TAB>sentence` record per thing left out.
fn logs(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    use std::io::Write as _;
    let args = split_args("pr logs", raw, &LOGS_VALUED, &[])?;
    args.expect(1, 2, "<number> [repo]")?;
    let number = number(&args)?;
    let pick = log_pick(&args)?;
    let pinned = match args.value(&["--head"]) {
        Some(text) => Some(
            rc_git_core::Oid::from_hex(text)
                .map_err(|_| UsageError(format!("--head '{text}' is not a full commit id")))?,
        ),
        None => None,
    };
    let (service, linked) = linked(&args, 1)?;
    let cancel = crate::interruptible_cancel()?;
    let detail = service
        .detail(&linked, number, &cancel)
        .map_err(|error| failure(&error))?;
    let head = detail.summary.head_oid;
    if let Some(pinned) = pinned.filter(|pinned| *pinned != head) {
        return Err(ReadFailure {
            message: format!(
                "#{number}'s head is now {head}, not --head {pinned}; no log was read"
            ),
            code: EXIT_MERGE_REFUSED,
        }
        .into());
    }
    let check = log_target(&detail, &pick)?;
    let ask = LogAsk {
        head,
        check: &check,
        on_matched: &|| {},
    };
    let log = service
        .read_log(&linked, ask, &cancel)
        .map_err(|stop| log_stopped(&stop, &check))?;
    if !log.text.is_empty() {
        writeln!(out, "{}", log.text)?;
    }
    let mut notes = Vec::new();
    for note in log_cut_notes(log.cut, log.lines_left_out, log.lines_shortened) {
        writeln!(notes, "truncated\t{note}")?;
    }
    std::io::stderr().lock().write_all(&notes)?;
    Ok(ExitCode::SUCCESS)
}

/// `pr diff`'s valued flags: the pr options and the remote options of `fetch`.
const DIFF_VALUED: [&str; 5] = [
    "--login",
    "--ssh-dir",
    "--username",
    "--token",
    "--accept-hostkey",
];

fn diff(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    use std::io::Write as _;
    let args = split_args("pr diff", raw, &DIFF_VALUED, &["--fetch"])?;
    args.expect(1, 2, "<number> [repo]")?;
    let number = number(&args)?;
    let (service, linked) = linked(&args, 1)?;
    let path = repo_path(&args, 1)?;
    let (store, host_keys) = git_secrets(&path, &args)?;
    let service = service.with_git_credentials(store, host_keys);
    let cancel = crate::interruptible_cancel()?;
    let detail = service
        .detail(&linked, number, &cancel)
        .map_err(|error| failure(&error))?;
    let (stamp, base) = (detail.stamp(), detail.base_oid);
    let mut read = service
        .changes(&path, &linked, &stamp, base)
        .map_err(|message| anyhow!(message))?;
    if read == ChangesRead::HeadMissing {
        if !args.has("--fetch") {
            bail!(
                "commit {}, the head of #{number}, is not in this repository; rerun with \
                 --fetch to download it into {}",
                stamp.head_oid,
                pull_head_ref(number)
            );
        }
        fetch_head(&service, (&path, &linked), &stamp, &cancel)?;
        writeln!(
            out,
            "fetched\t{}\t{}",
            pull_head_ref(number),
            stamp.head_oid
        )?;
        read = service
            .changes(&path, &linked, &stamp, base)
            .map_err(|message| anyhow!(message))?;
    }
    write_changes(out, &stamp, read)
}

/// Fetches `stamp`'s head; a head that moved since the read, or a failure, ends the command.
fn fetch_head(
    service: &LivePullRequests,
    (path, linked): (&str, &Linked),
    stamp: &PullRequestStamp,
    cancel: &crate::Cancel,
) -> Result<()> {
    let problem = match service.fetch_head(path, linked, stamp, cancel) {
        Ok(HeadFetched::Fetched) => return Ok(()),
        Ok(HeadFetched::Moved { found }) => bail!(
            "#{} moved after it was read: the remote's head is {found}, not {}; nothing was \
             fetched, so run the command again",
            stamp.number,
            stamp.head_oid
        ),
        Err(problem) => problem,
    };
    let code = match &problem {
        FetchProblem::Cancelled => EXIT_INTERRUPTED,
        FetchProblem::NeedsSecret(_) => EXIT_AUTH,
        FetchProblem::UnknownHostKey(_) => EXIT_HOSTKEY,
        FetchProblem::Failed(_) => EXIT_ERROR,
    };
    Err(ReadFailure {
        message: problem.message().unwrap_or("interrupted").to_owned(),
        code,
    }
    .into())
}

/// The git secrets the remote options give, keyed by the pull request remote's host as the
/// fetch offers them; nothing for a repository without that remote.
fn git_secrets(
    path: &str,
    args: &VerbArgs<'_>,
) -> Result<(Arc<dyn CredentialStore>, Arc<dyn HostKeyStore>)> {
    let store = MemoryCredentialStore::new();
    let host_keys = MemoryHostKeyStore::new();
    if let Some(remote) = pull_request_remote(Path::new(path))? {
        let transport = if remote.uses_ssh() {
            Transport::Ssh
        } else if remote.url().starts_with("https://") || remote.url().starts_with("http://") {
            Transport::Https
        } else {
            Transport::Other
        };
        let username = args.value(&["--username"]).map(str::to_owned);
        let token = args.value(&["--token"]).map(str::to_owned);
        if let Some(creds) = credentials_for(transport, username, token)? {
            let kind = if remote.uses_ssh() {
                CredentialKind::SshPassphrase
            } else {
                CredentialKind::HttpsToken
            };
            store.store(&creds, remote.host(), kind)?;
        }
        if let Some(fingerprint) = args.value(&["--accept-hostkey"]) {
            host_keys.store(fingerprint, remote.host())?;
        }
    }
    Ok((Arc::new(store), Arc::new(host_keys)))
}

/// `pr merge`'s valued flags.
const MERGE_VALUED: [&str; 7] = [
    "--login",
    "--ssh-dir",
    "--method",
    "--head",
    "--base",
    "--confirm",
    "--plan",
];

/// The merge `--admin` asks for; `--confirm` and `--plan` belong to it alone.
fn merge_kind(args: &VerbArgs<'_>) -> Result<MergeKind> {
    if args.has("--admin") {
        return Ok(MergeKind::Admin);
    }
    for flag in ["--confirm", "--plan"] {
        if args.value(&[flag]).is_some() {
            return Err(UsageError(format!(
                "{flag} goes with --admin; a plain pr merge is confirmed with --yes"
            ))
            .into());
        }
    }
    Ok(MergeKind::Plain)
}

/// The method `--method` names for `verb`.
fn merge_method(args: &VerbArgs<'_>, verb: &str) -> Result<MergeMethod> {
    match args.value(&["--method"]) {
        Some("merge") => Ok(MergeMethod::Merge),
        Some("squash") => Ok(MergeMethod::Squash),
        Some("rebase") => Ok(MergeMethod::Rebase),
        Some(other) => {
            Err(UsageError(format!("--method '{other}' is not merge, squash or rebase")).into())
        }
        None => Err(UsageError(format!("{verb} needs --method merge|squash|rebase")).into()),
    }
}

/// The head `--head` names for `verb`: the full commit id the caller saw.
fn stamp_head(args: &VerbArgs<'_>, verb: &str) -> Result<rc_git_core::Oid> {
    let Some(text) = args.value(&["--head"]) else {
        return Err(UsageError(format!(
            "{verb} needs --head <oid>, the full head commit id `pr view` printed: the head you \
             looked at; a pull request at any other head refuses before anything is sent"
        ))
        .into());
    };
    rc_git_core::Oid::from_hex(text)
        .map_err(|_| UsageError(format!("--head '{text}' is not a full commit id")).into())
}

/// The base `--base` names for `verb`: the base branch the caller saw, as `pr view` printed it.
fn stamp_base(args: &VerbArgs<'_>, verb: &str) -> Result<String> {
    let Some(text) = args.value(&["--base"]) else {
        return Err(UsageError(format!(
            "{verb} needs --base <branch>, the base `pr view` printed: the branch you looked at; \
             a pull request on any other base refuses before anything is sent"
        ))
        .into());
    };
    if text.is_empty() {
        return Err(UsageError("--base needs a branch name".to_owned()).into());
    }
    Ok(text.to_owned())
}

/// The stamp of what the caller saw of pull request `number`: `--head`, `--base`, open.
fn stamp(args: &VerbArgs<'_>, number: u64, verb: &str) -> Result<PullRequestStamp> {
    Ok(PullRequestStamp {
        number,
        head_oid: stamp_head(args, verb)?,
        base_ref: stamp_base(args, verb)?,
        state: PullRequestState::Open,
    })
}

/// A merge that sent nothing, with the exit code it ends with.
fn merge_stopped(stop: &MergeStop) -> anyhow::Error {
    let code = match stop {
        MergeStop::Cancelled => EXIT_INTERRUPTED,
        MergeStop::SignedOut { .. } => EXIT_AUTH,
        MergeStop::Refused(MergeRefusal::NotConfirmed { .. }) => EXIT_NOT_CONFIRMED,
        MergeStop::Refused(_) => EXIT_MERGE_REFUSED,
        MergeStop::Failed(_) => EXIT_ERROR,
    };
    ReadFailure {
        message: merge_stop_message(stop),
        code,
    }
    .into()
}

/// Merges pull request `<n>` as `--head`, `--base` and `--method` say, once `--yes` confirms it,
/// or, with `--admin`, past branch protection once `--confirm` carries the number and `--plan`
/// the fingerprint of the plan the caller was shown. The head, the base and the state the caller
/// saw are the stamp the guard compares a fresh read with; the guard's plan words what is
/// merged, its decision is the answer, and its text goes to stderr before the request leaves.
fn merge(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("pr merge", raw, &MERGE_VALUED, &["--yes", "--admin"])?;
    args.expect(1, 2, "<number> [repo]")?;
    let number = number(&args)?;
    let method = merge_method(&args, "pr merge")?;
    let stamp = stamp(&args, number, "pr merge")?;
    let kind = merge_kind(&args)?;
    let (service, linked) = linked(&args, 1)?;
    let (cancel, exits) = crate::interruptible(&crate::STOP_SIGNALS)?;
    let plan = service
        .plan_merge(
            &linked,
            PlanAsk {
                stamp: &stamp,
                method,
                kind,
            },
            &cancel,
        )
        .map_err(|stop| merge_stopped(&stop))?;
    let typed = match kind {
        MergeKind::Plain => plain_answer(&args, &plan, out)?,
        MergeKind::Admin => admin_answer(&args, &plan, out)?,
    };
    let repository = format!(
        "{}/{}@{}",
        linked.target.owner(),
        linked.target.name(),
        linked.target.host()
    );
    let (mut flow, load) = MergeFlow::start(1, repository, stamp.clone(), method, kind);
    flow.planned(load.generation, Ok(plan.clone()));
    flow.confirm(typed.clone());
    write_sending(&plan)?;
    let ask = MergeAsk {
        stamp: &stamp,
        method,
        kind,
        decision: Some(&plan.decision),
        typed: &typed,
        on_send: &|| exits.disarm(),
    };
    let answer = service.merge(&linked, ask, &cancel);
    if let Err(stop) = &answer {
        return Err(merge_stopped(stop));
    }
    flow.finished(load.generation, answer);
    merge_report(out, &linked, &flow)
}

/// The challenge's text of `plan`, one line per fact.
fn plan_summary(plan: &MergePlan) -> String {
    match &plan.decision {
        Decision::Confirm(challenge) => challenge.summary().to_owned(),
        Decision::Allow | Decision::Deny { .. } => format!("merge #{}", plan.detail.summary.number),
    }
}

/// One `label<TAB>line` record per line of `plan`'s challenge text.
fn write_plan(out: &mut Vec<u8>, label: &str, plan: &MergePlan) -> Result<()> {
    use std::io::Write as _;
    for line in plan_summary(plan).lines() {
        writeln!(out, "{label}\t{line}")?;
    }
    Ok(())
}

/// Writes what is about to be merged to stderr, just before the request may leave: the
/// challenge the guard worded from its fresh read, which `--yes` or `--confirm` answered.
fn write_sending(plan: &MergePlan) -> Result<()> {
    use std::io::Write as _;
    let mut text = Vec::new();
    write_plan(&mut text, "sending", plan)?;
    std::io::stderr().lock().write_all(&text)?;
    Ok(())
}

/// The plain merge's answer: `--yes`, or what would be merged and a usage error.
fn plain_answer(args: &VerbArgs<'_>, plan: &MergePlan, out: &mut Vec<u8>) -> Result<String> {
    if !args.has("--yes") {
        write_plan(out, "would-merge", plan)?;
        return Err(
            UsageError("pr merge needs --yes to merge; nothing was sent".to_owned()).into(),
        );
    }
    Ok(String::new())
}

/// The admin merge's answer: the `--confirm` text exactly as given, which the guard checks,
/// once `--plan` holds the fingerprint of the decision the guard gives now, so a script that
/// knows the number still merges only what a dry run showed. Without `--confirm` (`--yes`
/// counts for nothing here), what would be merged and skipped, the plan's fingerprint, and
/// exit 10; a `--plan` that is not the fingerprint now, exit 11. Neither sends anything.
fn admin_answer(args: &VerbArgs<'_>, plan: &MergePlan, out: &mut Vec<u8>) -> Result<String> {
    use std::io::Write as _;
    let fingerprint = plan.decision.fingerprint();
    let number = plan.detail.summary.number;
    let Some(typed) = args.value(&["--confirm"]) else {
        write_plan(out, "would-merge-admin", plan)?;
        if let Some(bypassed) = &plan.bypassed {
            write_bypassed(out, bypassed, &plan.detail.summary.base_ref)?;
        }
        writeln!(out, "plan\t{fingerprint}")?;
        return Err(ReadFailure {
            message: format!(
                "pr merge --admin needs the pull request's number typed and the plan it was \
                 shown: --confirm {number} --plan {fingerprint} (--yes is not enough); nothing \
                 was sent"
            ),
            code: EXIT_NOT_CONFIRMED,
        }
        .into());
    };
    match args.value(&["--plan"]) {
        Some(given) if given == fingerprint => Ok(typed.to_owned()),
        Some(given) => {
            write_plan(out, "would-merge-admin", plan)?;
            writeln!(out, "plan\t{fingerprint}")?;
            Err(ReadFailure {
                message: format!(
                    "merge #{number}: it changed since the plan you were shown (--plan {given}, \
                     now {fingerprint}); nothing was sent. Read the plan again with pr merge \
                     --admin without --confirm"
                ),
                code: EXIT_MERGE_REFUSED,
            }
            .into())
        }
        None => Err(UsageError(format!(
            "pr merge --admin --confirm needs --plan, the fingerprint the run without --confirm \
             printed (it is {fingerprint} now); nothing was sent"
        ))
        .into()),
    }
}

/// One `bypass` record per thing an admin merge is known to skip.
fn write_bypassed(out: &mut Vec<u8>, bypassed: &Bypassed, base: &str) -> Result<()> {
    use std::io::Write as _;
    for name in &bypassed.failed {
        writeln!(out, "bypass\tfailed-required-check\t{name}")?;
    }
    for name in &bypassed.pending {
        writeln!(out, "bypass\tpending-required-check\t{name}")?;
    }
    match bypassed.review {
        Some(ReviewDecision::ReviewRequired) => writeln!(out, "bypass\treview\treview-required")?,
        Some(ReviewDecision::ChangesRequested) => {
            writeln!(out, "bypass\treview\tchanges-requested")?;
        }
        Some(ReviewDecision::Approved | ReviewDecision::Unknown) | None => {}
    }
    if bypassed.behind {
        writeln!(out, "bypass\tbehind\t{base}")?;
    }
    if bypassed.unreported > 0 {
        writeln!(
            out,
            "bypass\tmaybe-required-checks\t{}",
            bypassed.unreported
        )?;
    }
    if bypassed.unread > 0 {
        writeln!(out, "bypass\tunread-checks\t{}", bypassed.unread)?;
    }
    writeln!(out, "bypass\tunreported-rules\tany the API does not report")?;
    Ok(())
}

/// What `pr merge` prints once its request may have left, and the code it ends with.
fn merge_report(out: &mut Vec<u8>, linked: &Linked, flow: &MergeFlow) -> Result<ExitCode> {
    use std::io::Write as _;
    let note = merge_note(flow).unwrap_or_default();
    let MergePhase::Done(end) = flow.phase() else {
        bail!("the merge has no outcome: {note}");
    };
    let label = format!(
        "{}/{}#{}",
        linked.target.owner(),
        linked.target.name(),
        flow.number()
    );
    let head = flow.stamp().head_oid;
    let commit = |merge_commit: &Option<rc_git_core::Oid>| {
        merge_commit.map_or_else(|| "-".to_owned(), |oid| oid.to_hex())
    };
    let code = match end {
        MergeEnd::Merged {
            merge_commit, base, ..
        } => {
            let base = base.as_deref().unwrap_or("-");
            writeln!(
                out,
                "merged\t{label}\t{}\t{head}\t{base}",
                commit(merge_commit)
            )?;
            writeln!(out, "note\t{note}")?;
            return Ok(ExitCode::SUCCESS);
        }
        MergeEnd::MergedIntoOtherBase {
            confirmed,
            actual,
            merge_commit,
            ..
        } => {
            writeln!(
                out,
                "merged-into-other-base\t{label}\t{}\t{head}\t{actual}\t{confirmed}",
                commit(merge_commit)
            )?;
            EXIT_MERGE_OTHER_BASE
        }
        MergeEnd::Refused { .. } | MergeEnd::NotSent(_) => EXIT_MERGE_REFUSED,
        MergeEnd::Unknown { .. } => EXIT_MERGE_UNKNOWN,
    };
    Err(ReadFailure {
        message: note,
        code,
    }
    .into())
}

/// `pr auto-merge`'s and `pr rerun`'s valued flags.
const ACTION_VALUED: [&str; 5] = ["--login", "--ssh-dir", "--method", "--head", "--base"];

/// An auto-merge change or a re-run that sent nothing, with the exit code it ends with.
fn action_stopped(stop: &ActionStop) -> anyhow::Error {
    let code = match stop {
        ActionStop::Cancelled => EXIT_INTERRUPTED,
        ActionStop::SignedOut { .. } => EXIT_AUTH,
        ActionStop::Refused(ActionRefusal::NotConfirmed { .. }) => EXIT_NOT_CONFIRMED,
        ActionStop::Refused(_) => EXIT_MERGE_REFUSED,
        ActionStop::Failed(_) => EXIT_ERROR,
    };
    ReadFailure {
        message: stop_message(stop),
        code,
    }
    .into()
}

/// The change `--enable` (with `--method`) or `--disable` asks for: exactly one of them.
fn auto_merge_change(args: &VerbArgs<'_>) -> Result<AutoMergeChange> {
    match (args.has("--enable"), args.has("--disable")) {
        (true, false) => Ok(AutoMergeChange::Enable(merge_method(
            args,
            "pr auto-merge --enable",
        )?)),
        (false, true) if args.value(&["--method"]).is_some() => Err(UsageError(
            "--method goes with --enable; pr auto-merge --disable turns off whatever is on"
                .to_owned(),
        )
        .into()),
        (false, true) => Ok(AutoMergeChange::Disable),
        (true, true) | (false, false) => Err(UsageError(
            "pr auto-merge needs exactly one of --enable or --disable".to_owned(),
        )
        .into()),
    }
}

/// One `label<TAB>line` record per line of `decision`'s challenge text.
fn write_decision(out: &mut Vec<u8>, label: &str, decision: &Decision) -> Result<()> {
    use std::io::Write as _;
    let Decision::Confirm(challenge) = decision else {
        bail!("the guard asked for no confirmation, which a GitHub change always has");
    };
    for line in challenge.summary().lines() {
        writeln!(out, "{label}\t{line}")?;
    }
    Ok(())
}

/// `decision`'s challenge to stderr as `sending<TAB>line` records, just before the request may
/// leave.
fn write_sending_decision(decision: &Decision) -> Result<()> {
    use std::io::Write as _;
    let mut text = Vec::new();
    write_decision(&mut text, "sending", decision)?;
    std::io::stderr().lock().write_all(&text)?;
    Ok(())
}

/// `owner/name@host` of `linked`'s repository.
fn repository_label(linked: &Linked) -> String {
    format!(
        "{}/{}@{}",
        linked.target.owner(),
        linked.target.name(),
        linked.target.host()
    )
}

/// Turns auto-merge of pull request `<n>` on (`--enable --method m`) or off (`--disable`) once
/// `--yes` confirms it, for the head and base the caller saw (`--head`, `--base`). Without
/// `--yes` it prints the challenge and sends nothing; with it, the challenge goes to stderr
/// before the one request leaves.
fn auto_merge(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args(
        "pr auto-merge",
        raw,
        &ACTION_VALUED,
        &["--yes", "--enable", "--disable"],
    )?;
    args.expect(1, 2, "<number> [repo]")?;
    let number = number(&args)?;
    let change = auto_merge_change(&args)?;
    let stamp = stamp(&args, number, "pr auto-merge")?;
    let (service, linked) = linked(&args, 1)?;
    let (cancel, exits) = crate::interruptible(&crate::STOP_SIGNALS)?;
    let plan = service
        .plan_auto_merge(&linked, &stamp, change, &cancel)
        .map_err(|stop| action_stopped(&stop))?;
    if !args.has("--yes") {
        let label = match change {
            AutoMergeChange::Enable(_) => "would-enable-auto-merge",
            AutoMergeChange::Disable => "would-disable-auto-merge",
        };
        write_decision(out, label, &plan.decision)?;
        return Err(UsageError(
            "pr auto-merge needs --yes to change auto-merge; nothing was sent".to_owned(),
        )
        .into());
    }
    let intent = AutoMergeIntent(change);
    let (mut flow, load) =
        AutoMergeFlow::begin(1, repository_label(&linked), stamp.clone(), intent);
    flow.planned(load.generation, Ok(plan.clone()));
    flow.confirm(String::new());
    write_sending_decision(&plan.decision)?;
    let ask = ActionAsk {
        stamp: &stamp,
        decision: Some(&plan.decision),
        on_send: &|| exits.disarm(),
    };
    let answer = service.set_auto_merge(&linked, change, ask, &cancel);
    if let Err(stop) = &answer {
        return Err(action_stopped(stop));
    }
    flow.finished(load.generation, answer);
    auto_merge_report(out, &linked, &flow)
}

/// What `pr auto-merge` prints once its request may have left, and the code it ends with.
fn auto_merge_report(out: &mut Vec<u8>, linked: &Linked, flow: &AutoMergeFlow) -> Result<ExitCode> {
    use std::io::Write as _;
    let note = auto_merge_note(flow).unwrap_or_default();
    let Phase::Done(Ended::Sent(outcome)) = flow.phase() else {
        bail!("the auto-merge change has no outcome: {note}");
    };
    let label = format!(
        "{}/{}#{}",
        linked.target.owner(),
        linked.target.name(),
        flow.number()
    );
    let head = flow.stamp().head_oid;
    let code = match outcome {
        AutoMergeOutcome::Done { .. } => {
            match flow.ask().0 {
                AutoMergeChange::Enable(method) => writeln!(
                    out,
                    "auto-merge\ton\t{label}\t{}\t{head}",
                    merge_method_word(method)
                )?,
                AutoMergeChange::Disable => writeln!(out, "auto-merge\toff\t{label}")?,
            }
            writeln!(out, "note\t{note}")?;
            return Ok(ExitCode::SUCCESS);
        }
        AutoMergeOutcome::OnOtherBase {
            confirmed, actual, ..
        } => {
            writeln!(
                out,
                "auto-merge-other-base\t{label}\t{head}\t{actual}\t{confirmed}"
            )?;
            EXIT_MERGE_OTHER_BASE
        }
        AutoMergeOutcome::Refused { .. } => EXIT_MERGE_REFUSED,
        AutoMergeOutcome::Unknown { .. } => EXIT_MERGE_UNKNOWN,
    };
    Err(ReadFailure {
        message: note,
        code,
    }
    .into())
}

/// Re-runs the failed jobs of the GitHub Actions runs of pull request `<n>`'s head once `--yes`
/// confirms it, for the head and base the caller saw. Without `--yes` it prints the challenge
/// and sends nothing; with it, the challenge goes to stderr before the first request leaves.
fn rerun(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("pr rerun", raw, &ACTION_VALUED, &["--yes"])?;
    args.expect(1, 2, "<number> [repo]")?;
    if args.value(&["--method"]).is_some() {
        return Err(UsageError("pr rerun takes no --method".to_owned()).into());
    }
    let number = number(&args)?;
    let stamp = stamp(&args, number, "pr rerun")?;
    let (service, linked) = linked(&args, 1)?;
    let (cancel, exits) = crate::interruptible(&crate::STOP_SIGNALS)?;
    let plan = service
        .plan_rerun(&linked, &stamp, &cancel)
        .map_err(|stop| action_stopped(&stop))?;
    if !args.has("--yes") {
        write_decision(out, "would-rerun", &plan.decision)?;
        return Err(UsageError(
            "pr rerun needs --yes to re-run failed jobs; nothing was sent".to_owned(),
        )
        .into());
    }
    let (mut flow, load) =
        RerunFlow::begin(1, repository_label(&linked), stamp.clone(), RerunIntent);
    flow.planned(load.generation, Ok(plan.clone()));
    flow.confirm(String::new());
    write_sending_decision(&plan.decision)?;
    let ask = ActionAsk {
        stamp: &stamp,
        decision: Some(&plan.decision),
        on_send: &|| exits.disarm(),
    };
    let answer = service.rerun(&linked, ask, &cancel);
    if let Err(stop) = &answer {
        return Err(action_stopped(stop));
    }
    flow.finished(load.generation, answer);
    rerun_report(out, &flow)
}

/// What `pr rerun` prints once a request may have left: one `rerun` record per run, the note,
/// and the code it ends with (0 only when GitHub took every run).
fn rerun_report(out: &mut Vec<u8>, flow: &RerunFlow) -> Result<ExitCode> {
    use std::io::Write as _;
    let note = rerun_note(flow).unwrap_or_default();
    let Phase::Done(Ended::Sent(report)) = flow.phase() else {
        bail!("the re-run has no outcome: {note}");
    };
    let mut unknown = false;
    let mut refused = false;
    for run in &report.runs {
        let (word, why) = match &run.outcome {
            RunOutcome::Requested => ("requested", "-"),
            RunOutcome::Refused { reason } => {
                refused = true;
                ("refused", reason.as_str())
            }
            RunOutcome::Unknown { problem } => {
                unknown = true;
                ("unknown", problem.as_str())
            }
            RunOutcome::NotSent { reason } => {
                refused = true;
                ("not-sent", reason.as_str())
            }
            RunOutcome::NotMatched { reason } => {
                refused = true;
                ("not-matched", reason.as_str())
            }
        };
        writeln!(out, "rerun\t{}\t{word}\t{why}", run.run)?;
    }
    let code = match (unknown, refused) {
        (true, _) => EXIT_MERGE_UNKNOWN,
        (false, true) => EXIT_MERGE_REFUSED,
        (false, false) => {
            writeln!(out, "note\t{note}")?;
            return Ok(ExitCode::SUCCESS);
        }
    };
    Err(ReadFailure {
        message: note,
        code,
    }
    .into())
}

/// What `pr diff` prints for `read`, or why there is nothing to print.
fn write_changes(
    out: &mut Vec<u8>,
    stamp: &PullRequestStamp,
    read: ChangesRead,
) -> Result<ExitCode> {
    use std::io::Write as _;
    let head = stamp.head_oid;
    let (base, compared, files) = match read {
        ChangesRead::Files {
            base,
            compared,
            files,
        } => (base, compared, files),
        ChangesRead::HeadMissing => bail!("commit {head} is still not in this repository"),
        ChangesRead::NoBaseCopy {
            branch,
            tracking,
            commit: Some(commit),
        } => bail!(
            "commit {commit}, where {branch} is on GitHub and the changes are compared with, is \
             not in this repository; {}",
            match tracking {
                Some(_) => "fetch the remote, then run the command again",
                None => "the remote's fetch refspecs keep no copy of it, so fetch it with git",
            }
        ),
        ChangesRead::NoBaseCopy {
            branch,
            tracking: Some(tracking),
            commit: None,
        } => bail!(
            "there is no {tracking}, the copy of {branch} the changes are compared with; \
             fetch the remote, then run the command again"
        ),
        ChangesRead::NoBaseCopy {
            branch,
            tracking: None,
            commit: None,
        } => bail!("the remote's fetch refspecs keep no copy of {branch}; fetch it with git"),
        ChangesRead::NoMergeBase {
            compared,
            incomplete: true,
            scanned,
        } => bail!(
            "no merge base of {head} and {} within {scanned} commits; the history may be \
             shallow, so fetch more of it with git",
            compared_text(&compared)
        ),
        ChangesRead::NoMergeBase { compared, .. } => {
            bail!("{head} and {} share no history", compared_text(&compared))
        }
    };
    let (source, against) = match &compared {
        ComparedWith::PullRequestBase(commit) => ("pull-request-base", commit.to_hex()),
        ComparedWith::LocalCopy(tracking) => ("local-copy", tracking.clone()),
    };
    writeln!(out, "base\t{base}\t{source}\t{against}")?;
    writeln!(out, "head\t{head}")?;
    for change in files {
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

/// One `body` record: a line of the description, its tabs shown as `\u{9}` so the line stays
/// one field.
fn body_record(line: &str) -> String {
    format!("body\t{}", line.replace('\t', "\\u{9}"))
}

/// What a pull request's head was compared with, for an error.
fn compared_text(compared: &ComparedWith) -> String {
    match compared {
        ComparedWith::PullRequestBase(commit) => format!("{commit}, the base GitHub reports"),
        ComparedWith::LocalCopy(tracking) => tracking.clone(),
    }
}

#[cfg(test)]
mod tests {
    use rc_git_kit::pull_requests::{PullRequestState, single_line};

    use super::*;

    #[test]
    fn a_row_is_one_record_whatever_the_title_holds() {
        let summary = PullRequestSummary {
            number: 7,
            title: single_line("a\tb\nc\u{202e}", 100),
            author: None,
            head_ref: "feature".to_owned(),
            head_oid: rc_git_core::Oid::from_hex(&"ab".repeat(20)).unwrap(),
            base_ref: "main".to_owned(),
            head_owner: None,
            cross_repository: false,
            draft: false,
            state: PullRequestState::Open,
            updated_at: None,
            checks: None,
            review: None,
        };
        let printed = row(&summary);
        assert_eq!(printed.lines().count(), 1);
        assert_eq!(printed.matches('\t').count(), 8, "{printed}");
        assert!(printed.ends_with("a\\u{9}b\\u{a}c\\u{202e}"), "{printed}");
    }

    #[test]
    fn a_description_line_keeps_its_tabs_out_of_the_record() {
        let record = body_record("a\tb\t\u{a0}c");
        assert_eq!(record, "body\ta\\u{9}b\\u{9}\u{a0}c");
        assert_eq!(record.matches('\t').count(), 1);
    }
}
