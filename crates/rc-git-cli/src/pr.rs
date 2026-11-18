//! `rc-git pr`: a repository's open pull requests, one in full, and its checks, read through
//! the app's accounts and keychain (the same stores `auth` uses) and kit's live service, so the
//! CLI picks the account exactly as the app does: the one account on the remote's host, else
//! the one chosen for the repository, else `--login`. Every text the server sent is printed as
//! the guard neutralized it: one-line fields carry no tab, line break, control or bidi
//! character, and the description's lines are printed one record each.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use rc_git_core::Repository;
use rc_git_kit::canonical_path;
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore};
use rc_git_kit::pull_requests::checks::{self, check_state, ordered, row_state};
use rc_git_kit::pull_requests::copy::{
    check_label, mergeable_label, problem, review_label, state_label,
};
use rc_git_kit::pull_requests::link::{Linked, RepositoryLink};
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::{
    PageCursor, PullRequestDetail, PullRequestService, PullRequestSummary, ReadError, ServerLink,
};

use crate::{EXIT_AUTH, EXIT_ERROR, EXIT_INTERRUPTED, UsageError, VerbArgs, split_args};

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
        return Err(UsageError("pr needs list, view or checks".to_owned()).into());
    };
    let started = Instant::now();
    let result = match verb.as_str() {
        "list" => list(rest, out),
        "view" => view(rest, out),
        "checks" => checks(rest, out),
        other => Err(UsageError(format!("pr has no '{other}'; use list, view or checks")).into()),
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
