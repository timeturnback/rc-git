//! `rc-git pr`: a repository's open pull requests, one in full, its checks, and the files it
//! changes, read through the app's accounts and keychain (the same stores `auth` uses) and
//! kit's live service, so the CLI picks the account exactly as the app does: the one account
//! on the remote's host, else the one chosen for the repository, else `--login`. Every text
//! the server sent is printed as the guard neutralized it: one-line fields carry no tab, line
//! break, control or bidi character, and the description's lines are printed one record each.
//!
//! `pr diff` reads the changes from local objects. It fetches the pull request's head only
//! with `--fetch`, over the remote's git transport with the remote options' credentials; the
//! GitHub token stays with the API reads.
//!
//! `pr merge` merges through the guard as the app does. `--head` is required: it is the stamp
//! of what the caller saw (`pr view` prints it), and a pull request whose head moved since
//! refuses before anything is sent. Without `--yes` it prints what would be merged and merges
//! nothing. It sends one request at most and never sends it again; an answer that was lost
//! ends with its own exit code and tells the caller to look at the pull request on GitHub.
//! `pr merge --admin` is the admin merge past branch protection: `--yes` does nothing for it,
//! and without `--confirm` it prints what would be skipped and sends nothing. The `--confirm`
//! text goes to the guard as typed; the guard alone checks it against the number.

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
use rc_git_kit::pull_requests::changes::{ChangesRead, FetchProblem, HeadFetched};
use rc_git_kit::pull_requests::checks::{self, check_state, ordered, row_state};
use rc_git_kit::pull_requests::copy::{
    check_label, merge_note, merge_stop_message, mergeable_label, problem, review_label,
    state_label,
};
use rc_git_kit::pull_requests::link::{Linked, RepositoryLink};
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::merge::{
    MergeAsk, MergeEnd, MergeFlow, MergePhase, MergeStop, PlanAsk,
};
use rc_git_kit::pull_requests::{
    Bypassed, Decision, MergeKind, MergeMethod, MergePlan, MergeRefusal, PageCursor,
    PullRequestDetail, PullRequestService, PullRequestStamp, PullRequestState, PullRequestSummary,
    ReadError, ReviewDecision, ServerLink,
};

use crate::{
    EXIT_AUTH, EXIT_ERROR, EXIT_HOSTKEY, EXIT_INTERRUPTED, EXIT_MERGE_REFUSED, EXIT_MERGE_UNKNOWN,
    EXIT_NOT_CONFIRMED, Records, Transport, UsageError, VerbArgs, credentials_for, split_args,
    write_path,
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
        return Err(UsageError("pr needs list, view, checks, diff or merge".to_owned()).into());
    };
    let started = Instant::now();
    let result = match verb.as_str() {
        "list" => list(rest, out),
        "view" => view(rest, out),
        "checks" => checks(rest, out),
        "diff" => diff(rest, out),
        "merge" => merge(rest, out),
        other => Err(UsageError(format!(
            "pr has no '{other}'; use list, view, checks, diff or merge"
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
        writeln!(out, "body\t{line}")?;
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
    let stamp = service
        .detail(&linked, number, &cancel)
        .map_err(|error| failure(&error))?
        .stamp();
    let mut read = service
        .changes(&path, &linked, &stamp)
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
            .changes(&path, &linked, &stamp)
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
const MERGE_VALUED: [&str; 5] = ["--login", "--ssh-dir", "--method", "--head", "--confirm"];

/// The merge `--admin` asks for; `--confirm` belongs to it alone.
fn merge_kind(args: &VerbArgs<'_>) -> Result<MergeKind> {
    if args.has("--admin") {
        return Ok(MergeKind::Admin);
    }
    if args.value(&["--confirm"]).is_some() {
        return Err(UsageError(
            "--confirm goes with --admin; a plain pr merge is confirmed with --yes".to_owned(),
        )
        .into());
    }
    Ok(MergeKind::Plain)
}

/// The method `--method` names.
fn merge_method(args: &VerbArgs<'_>) -> Result<MergeMethod> {
    match args.value(&["--method"]) {
        Some("merge") => Ok(MergeMethod::Merge),
        Some("squash") => Ok(MergeMethod::Squash),
        Some("rebase") => Ok(MergeMethod::Rebase),
        Some(other) => {
            Err(UsageError(format!("--method '{other}' is not merge, squash or rebase")).into())
        }
        None => Err(UsageError("pr merge needs --method merge|squash|rebase".to_owned()).into()),
    }
}

/// The head `--head` names: the full commit id the caller saw.
fn merge_head(args: &VerbArgs<'_>) -> Result<rc_git_core::Oid> {
    let Some(text) = args.value(&["--head"]) else {
        return Err(UsageError(
            "pr merge needs --head <oid>, the full head commit id `pr view` printed: the head \
             you looked at, which the merge refuses to go past"
                .to_owned(),
        )
        .into());
    };
    rc_git_core::Oid::from_hex(text)
        .map_err(|_| UsageError(format!("--head '{text}' is not a full commit id")).into())
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

/// Merges pull request `<n>` as `--head` and `--method` say, once `--yes` confirms it, or, with
/// `--admin`, past branch protection once `--confirm` carries the number. The head the caller
/// saw and the base and state read now are the stamp the guard compares a fresh read with; the
/// guard's plan words what is merged, and its decision is the answer.
fn merge(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("pr merge", raw, &MERGE_VALUED, &["--yes", "--admin"])?;
    args.expect(1, 2, "<number> [repo]")?;
    let number = number(&args)?;
    let method = merge_method(&args)?;
    let head = merge_head(&args)?;
    let kind = merge_kind(&args)?;
    let (service, linked) = linked(&args, 1)?;
    let cancel = crate::interruptible_cancel()?;
    let detail = service
        .detail(&linked, number, &cancel)
        .map_err(|error| failure(&error))?;
    let stamp = PullRequestStamp {
        number,
        head_oid: head,
        base_ref: detail.summary.base_ref,
        state: PullRequestState::Open,
    };
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
    let (mut flow, load) = MergeFlow::start(1, stamp.clone(), method, kind);
    flow.planned(load.generation, Ok(plan.clone()));
    flow.confirm(typed.clone());
    let ask = MergeAsk {
        stamp: &stamp,
        method,
        kind,
        decision: Some(&plan.decision),
        typed: &typed,
        on_send: &|| {},
    };
    let answer = service.merge(&linked, ask, &cancel);
    if let Err(stop) = &answer {
        return Err(merge_stopped(stop));
    }
    flow.finished(load.generation, answer);
    merge_report(out, &linked, &flow)
}

/// The challenge's summary of `plan`.
fn plan_summary(plan: &MergePlan) -> String {
    match &plan.decision {
        Decision::Confirm(challenge) => challenge.summary().to_owned(),
        Decision::Allow | Decision::Deny { .. } => format!("merge #{}", plan.detail.summary.number),
    }
}

/// The plain merge's answer: `--yes`, or what would be merged and a usage error.
fn plain_answer(args: &VerbArgs<'_>, plan: &MergePlan, out: &mut Vec<u8>) -> Result<String> {
    use std::io::Write as _;
    if !args.has("--yes") {
        writeln!(out, "would-merge\t{}", plan_summary(plan))?;
        return Err(
            UsageError("pr merge needs --yes to merge; nothing was sent".to_owned()).into(),
        );
    }
    Ok(String::new())
}

/// The admin merge's answer: the `--confirm` text exactly as given, which the guard checks.
/// Without it (`--yes` counts for nothing here), what would be merged and skipped, and exit 10.
fn admin_answer(args: &VerbArgs<'_>, plan: &MergePlan, out: &mut Vec<u8>) -> Result<String> {
    use std::io::Write as _;
    if let Some(typed) = args.value(&["--confirm"]) {
        return Ok(typed.to_owned());
    }
    writeln!(out, "would-merge-admin\t{}", plan_summary(plan))?;
    if let Some(bypassed) = &plan.bypassed {
        write_bypassed(out, bypassed, &plan.detail.summary.base_ref)?;
    }
    let number = plan.detail.summary.number;
    Err(ReadFailure {
        message: format!(
            "pr merge --admin needs the pull request's number typed: --confirm {number} (--yes \
             is not enough); nothing was sent"
        ),
        code: EXIT_NOT_CONFIRMED,
    }
    .into())
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
    let code = match end {
        MergeEnd::Merged { merge_commit, .. } => {
            let commit = merge_commit.map_or_else(|| "-".to_owned(), |oid| oid.to_hex());
            writeln!(
                out,
                "merged\t{}/{}#{}\t{commit}\t{}",
                linked.target.owner(),
                linked.target.name(),
                flow.number(),
                flow.stamp().head_oid
            )?;
            writeln!(out, "note\t{note}")?;
            return Ok(ExitCode::SUCCESS);
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

/// What `pr diff` prints for `read`, or why there is nothing to print.
fn write_changes(
    out: &mut Vec<u8>,
    stamp: &PullRequestStamp,
    read: ChangesRead,
) -> Result<ExitCode> {
    use std::io::Write as _;
    let head = stamp.head_oid;
    let (base, tracking, files) = match read {
        ChangesRead::Files {
            base,
            tracking,
            files,
        } => (base, tracking, files),
        ChangesRead::HeadMissing => bail!("commit {head} is still not in this repository"),
        ChangesRead::NoBaseCopy {
            branch,
            tracking: Some(tracking),
        } => bail!(
            "there is no {tracking}, the copy of {branch} the changes are compared with; \
             fetch the remote, then run the command again"
        ),
        ChangesRead::NoBaseCopy {
            branch,
            tracking: None,
        } => bail!("the remote's fetch refspecs keep no copy of {branch}; fetch it with git"),
        ChangesRead::NoMergeBase {
            tracking,
            incomplete: true,
            scanned,
        } => bail!(
            "no merge base of {head} and {tracking} within {scanned} commits; the history \
             may be shallow, so fetch more of it with git"
        ),
        ChangesRead::NoMergeBase { tracking, .. } => {
            bail!("{head} and {tracking} share no history")
        }
    };
    writeln!(out, "base\t{base}\t{tracking}")?;
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
}
