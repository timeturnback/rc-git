//! The pull request reads: one page of a repository's open pull requests, and one pull
//! request in full with its head commit's checks. Both are GraphQL queries with static text and
//! every value in `variables`, sent with the account's token to the account's host only (the
//! [`RepositoryTarget`] must be on that host, else nothing is sent). Every answer is bounded: a
//! page holds [`PAGE_SIZE`] pull requests, a detail at most [`MAX_CHECKS`] checks, and the body
//! is capped like every API answer.
//!
//! A GitHub Enterprise Server release older than the detail query lacks some of its fields,
//! and GraphQL refuses a whole query that names one. The detail read then asks once more with
//! [`DETAIL_BASIC`], which leaves those fields out, and says so in
//! [`PullRequestDetail::fields`].

use serde_json::{Value, json};

use super::api::{GraphqlReply, Session};
use super::host::GitHubHost;
use super::links::server_link;
use super::pull_types::{
    ActionsIds, AutoMerge, Check, CheckConclusion, CheckList, CheckProgress, CheckStatus,
    DetailFields, MergeMethod, MergeMethods, MergeStateStatus, Mergeable, PageCursor,
    PullRequestDetail, PullRequestId, PullRequestPage, PullRequestState, PullRequestSummary,
    ReviewDecision, StatusState, ViewerAbilities,
};
use super::secret::ApiToken;
use super::text::{multi_line, single_line};
use super::{GitHubError, GitHubResult, RepositoryTarget};
use crate::actions::remote::Cancel;

/// How many pull requests one page holds.
pub const PAGE_SIZE: u32 = 30;

/// The most checks a detail reads; a commit with more says so in [`CheckList::total`].
pub const MAX_CHECKS: u32 = 100;

/// The longest description kept, in characters; GitHub's own cap is 65,536.
const MAX_BODY_CHARS: usize = 65_536;

/// The longest title, branch name or check name kept, in characters.
pub(super) const MAX_LINE_CHARS: usize = 1024;

/// The longest login kept, in characters.
const MAX_LOGIN_CHARS: usize = 100;

/// One page of open pull requests, most recently updated first.
pub(super) const LIST: &str = "query RcGitPullRequests($owner: String!, $name: String!, \
$first: Int!, $after: String) { repository(owner: $owner, name: $name) { \
pullRequests(states: [OPEN], first: $first, after: $after, \
orderBy: {field: UPDATED_AT, direction: DESC}) { pageInfo { hasNextPage endCursor } nodes { \
number title isDraft state updatedAt author { login } headRefName headRefOid baseRefName \
isCrossRepository headRepositoryOwner { login } reviewDecision \
commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } } } } }";

/// One pull request with every field rc-git uses.
pub(super) const DETAIL_FULL: &str = "query RcGitPullRequest($owner: String!, $name: String!, \
$number: Int!, $checks: Int!) { repository(owner: $owner, name: $name) { \
mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed autoMergeAllowed \
pullRequest(number: $number) { id number title body url isDraft state updatedAt \
author { login } headRefName headRefOid baseRefName baseRefOid isCrossRepository \
headRepositoryOwner { login } reviewDecision mergeable mergeStateStatus viewerCanMergeAsAdmin viewerCanEnableAutoMerge \
viewerCanDisableAutoMerge viewerCanDeleteHeadRef autoMergeRequest { mergeMethod \
enabledBy { login } } commits(last: 1) { nodes { commit { statusCheckRollup { state \
contexts(first: $checks) { totalCount nodes { __typename ... on CheckRun { name status \
conclusion detailsUrl databaseId isRequired(pullRequestNumber: $number) \
checkSuite { workflowRun { databaseId } } } ... on StatusContext { context state targetUrl \
description isRequired(pullRequestNumber: $number) } } } } } } } } } }";

/// [`DETAIL_FULL`] without the fields an older GitHub Enterprise Server lacks.
pub(super) const DETAIL_BASIC: &str = "query RcGitPullRequestBasic($owner: String!, \
$name: String!, $number: Int!, $checks: Int!) { repository(owner: $owner, name: $name) { \
mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed \
pullRequest(number: $number) { id number title body url isDraft state updatedAt \
author { login } headRefName headRefOid baseRefName isCrossRepository \
headRepositoryOwner { login } reviewDecision mergeable commits(last: 1) { nodes { commit { statusCheckRollup { state \
contexts(first: $checks) { totalCount nodes { __typename ... on CheckRun { name status \
conclusion detailsUrl databaseId } ... on StatusContext { context state targetUrl \
description } } } } } } } } } }";

/// The variables of the list query for `target`, from `after` on.
pub(super) fn list_variables(target: &RepositoryTarget, after: Option<&PageCursor>) -> Value {
    json!({
        "owner": target.owner(),
        "name": target.name(),
        "first": PAGE_SIZE,
        "after": after.map(|cursor| cursor.0.as_str()),
    })
}

/// The variables of a detail query for pull request `number` of `target`.
pub(super) fn detail_variables(target: &RepositoryTarget, number: u64) -> Value {
    json!({
        "owner": target.owner(),
        "name": target.name(),
        "number": number,
        "checks": MAX_CHECKS,
    })
}

/// One page of `target`'s open pull requests, most recently updated first, from `after` on.
/// `token` goes to `host` only, and only when `target` is on `host`.
///
/// # Errors
/// [`GitHubError::ForeignRemote`] before any request for a target on another host;
/// [`GitHubError::Unauthorized`], [`GitHubError::RateLimited`] and the transport errors as
/// every call; [`GitHubError::Status`] with GitHub's message when the repository cannot be
/// read (missing, or not visible to the token); [`GitHubError::Malformed`] for an answer that
/// is not a page of pull requests.
pub fn list_open_pull_requests(
    host: &GitHubHost,
    token: &ApiToken,
    target: &RepositoryTarget,
    after: Option<&PageCursor>,
    cancel: &Cancel,
) -> GitHubResult<PullRequestPage> {
    const OP: &str = "list pull requests";
    let session = Session::new(host, token);
    session.check_target(target)?;
    let reply = session.graphql(OP, LIST, &list_variables(target, after), cancel)?;
    let connection = repository(&reply)?
        .get("pullRequests")
        .ok_or_else(|| reply.malformed("the answer has no pullRequests"))?;
    let mut pull_requests = Vec::new();
    for node in nodes(connection) {
        let summary = summary(node).ok_or_else(|| {
            reply.malformed("a pull request lacks its number, title, branches or head commit")
        })?;
        pull_requests.push(summary);
    }
    let info = connection.get("pageInfo");
    let has_next = info
        .and_then(|i| i.get("hasNextPage"))
        .and_then(Value::as_bool);
    let next = match has_next {
        Some(true) => Some(cursor(info).ok_or_else(|| reply.malformed("a bad page cursor"))?),
        Some(false) | None => None,
    };
    Ok(PullRequestPage {
        pull_requests,
        next,
    })
}

/// Pull request `number` of `target` in full, with its head commit's checks. `token` goes to
/// `host` only, and only when `target` is on `host`. When the host's schema lacks a field of
/// the full query, the read is asked again without those fields ([`DetailFields::Basic`]).
///
/// # Errors
/// [`GitHubError::InvalidValue`] before any request for a number GraphQL cannot carry
/// (0, or above 2^31 − 1); otherwise as [`list_open_pull_requests`], with
/// [`GitHubError::Status`] for a pull request the repository does not have.
pub fn read_pull_request(
    host: &GitHubHost,
    token: &ApiToken,
    target: &RepositoryTarget,
    number: u64,
    cancel: &Cancel,
) -> GitHubResult<PullRequestDetail> {
    const OP: &str = "read the pull request";
    if number == 0 || i32::try_from(number).is_err() {
        return Err(GitHubError::InvalidValue {
            field: "pull request number",
            value: number.to_string(),
            reason: "a pull request number is 1 to 2147483647",
        });
    }
    let session = Session::new(host, token);
    session.check_target(target)?;
    let variables = detail_variables(target, number);
    let mut fields = DetailFields::Full;
    let mut reply = session.graphql(OP, DETAIL_FULL, &variables, cancel)?;
    if reply.data.is_none()
        && !reply.errors.is_empty()
        && reply
            .errors
            .iter()
            .all(super::api::GraphqlError::is_unknown_field)
    {
        fields = DetailFields::Basic;
        reply = session.graphql(OP, DETAIL_BASIC, &variables, cancel)?;
    }
    detail(&reply, host, fields)
}

/// `data.repository` of `reply`, or the reply's failure.
fn repository(reply: &GraphqlReply) -> GitHubResult<&Value> {
    reply
        .data
        .as_ref()
        .and_then(|data| data.get("repository"))
        .filter(|repository| !repository.is_null())
        .ok_or_else(|| reply.failure("the answer names no repository"))
}

/// The non-null `nodes` of a connection.
fn nodes(connection: &Value) -> impl Iterator<Item = &Value> {
    connection
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|node| !node.is_null())
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn flag(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn login(value: &Value, key: &str) -> Option<String> {
    let login = text(value.get(key)?, "login")?;
    Some(single_line(login, MAX_LOGIN_CHARS))
}

fn cursor(info: Option<&Value>) -> Option<PageCursor> {
    PageCursor::from_text(text(info?, "endCursor")?)
}

/// The combined state of the head commit's checks, and the commit node itself.
fn head_commit(node: &Value) -> Option<&Value> {
    let commits = node.get("commits")?;
    nodes(commits).next()?.get("commit")
}

fn summary(node: &Value) -> Option<PullRequestSummary> {
    let number = node.get("number").and_then(Value::as_u64)?;
    let head_oid = rc_git_core::Oid::from_hex(text(node, "headRefOid")?).ok()?;
    let rollup = head_commit(node)
        .and_then(|commit| commit.get("statusCheckRollup"))
        .filter(|rollup| !rollup.is_null());
    Some(PullRequestSummary {
        number,
        title: single_line(text(node, "title")?, MAX_LINE_CHARS),
        author: login(node, "author"),
        head_ref: single_line(text(node, "headRefName")?, MAX_LINE_CHARS),
        head_oid,
        base_ref: single_line(text(node, "baseRefName")?, MAX_LINE_CHARS),
        head_owner: login(node, "headRepositoryOwner"),
        cross_repository: flag(node, "isCrossRepository").unwrap_or(false),
        draft: flag(node, "isDraft").unwrap_or(false),
        state: pull_request_state(text(node, "state")),
        updated_at: text(node, "updatedAt").and_then(iso_seconds),
        checks: rollup.map(|rollup| status_state(text(rollup, "state"))),
        review: text(node, "reviewDecision").map(review_decision),
    })
}

fn detail(
    reply: &GraphqlReply,
    host: &GitHubHost,
    fields: DetailFields,
) -> GitHubResult<PullRequestDetail> {
    let repository = repository(reply)?;
    let node = repository
        .get("pullRequest")
        .filter(|node| !node.is_null())
        .ok_or_else(|| reply.failure("the repository has no such pull request"))?;
    let summary = summary(node).ok_or_else(|| {
        reply.malformed("the pull request lacks its number, title, branches or head commit")
    })?;
    let full = fields == DetailFields::Full;
    Ok(PullRequestDetail {
        summary,
        id: text(node, "id").and_then(PullRequestId::parse),
        base_oid: if full {
            text(node, "baseRefOid").and_then(|oid| rc_git_core::Oid::from_hex(oid).ok())
        } else {
            None
        },
        body: text(node, "body")
            .map(|body| multi_line(body, MAX_BODY_CHARS))
            .unwrap_or_default(),
        url: text(node, "url").and_then(|url| server_link(url, host)),
        mergeable: mergeable(text(node, "mergeable")),
        merge_state: full.then(|| merge_state(text(node, "mergeStateStatus"))),
        merge_methods: MergeMethods {
            merge: flag(repository, "mergeCommitAllowed").unwrap_or(false),
            squash: flag(repository, "squashMergeAllowed").unwrap_or(false),
            rebase: flag(repository, "rebaseMergeAllowed").unwrap_or(false),
        },
        auto_merge_allowed: if full {
            flag(repository, "autoMergeAllowed")
        } else {
            None
        },
        viewer: full.then(|| ViewerAbilities {
            can_merge_as_admin: flag(node, "viewerCanMergeAsAdmin").unwrap_or(false),
            can_enable_auto_merge: flag(node, "viewerCanEnableAutoMerge").unwrap_or(false),
            can_disable_auto_merge: flag(node, "viewerCanDisableAutoMerge").unwrap_or(false),
            can_delete_head_ref: flag(node, "viewerCanDeleteHeadRef").unwrap_or(false),
        }),
        auto_merge: node
            .get("autoMergeRequest")
            .filter(|auto| !auto.is_null())
            .map(|auto| AutoMerge {
                method: text(auto, "mergeMethod").and_then(merge_method),
                enabled_by: login(auto, "enabledBy"),
            }),
        checks: check_list(node, host),
        fields,
    })
}

fn check_list(node: &Value, host: &GitHubHost) -> CheckList {
    let Some(contexts) = head_commit(node)
        .and_then(|commit| commit.get("statusCheckRollup"))
        .and_then(|rollup| rollup.get("contexts"))
    else {
        return CheckList::default();
    };
    let checks: Vec<Check> = nodes(contexts)
        .filter_map(|item| check(item, host))
        .collect();
    let total = contexts
        .get("totalCount")
        .and_then(Value::as_u64)
        .and_then(|total| usize::try_from(total).ok())
        .unwrap_or(checks.len())
        .max(checks.len());
    CheckList { checks, total }
}

fn check(item: &Value, host: &GitHubHost) -> Option<Check> {
    let link = |key| text(item, key).and_then(|url| server_link(url, host));
    let required = flag(item, "isRequired");
    match text(item, "__typename")? {
        "CheckRun" => {
            let status = check_status(text(item, "status"));
            let workflow_run = item
                .get("checkSuite")
                .and_then(|suite| suite.get("workflowRun"))
                .and_then(|run| run.get("databaseId"))
                .and_then(Value::as_u64);
            let job = item.get("databaseId").and_then(Value::as_u64);
            Some(Check {
                name: single_line(text(item, "name")?, MAX_LINE_CHARS),
                progress: CheckProgress::Run {
                    status,
                    conclusion: text(item, "conclusion").map(check_conclusion),
                },
                description: None,
                required,
                details: link("detailsUrl"),
                actions: workflow_run
                    .zip(job)
                    .map(|(workflow_run, job)| ActionsIds { workflow_run, job }),
            })
        }
        "StatusContext" => Some(Check {
            name: single_line(text(item, "context")?, MAX_LINE_CHARS),
            progress: CheckProgress::Status(status_state(text(item, "state"))),
            description: text(item, "description").map(|d| single_line(d, MAX_LINE_CHARS)),
            required,
            details: link("targetUrl"),
            actions: None,
        }),
        _ => None,
    }
}

fn pull_request_state(word: Option<&str>) -> PullRequestState {
    match word {
        Some("OPEN") => PullRequestState::Open,
        Some("CLOSED") => PullRequestState::Closed,
        Some("MERGED") => PullRequestState::Merged,
        Some(_) | None => PullRequestState::Unknown,
    }
}

fn review_decision(word: &str) -> ReviewDecision {
    match word {
        "APPROVED" => ReviewDecision::Approved,
        "CHANGES_REQUESTED" => ReviewDecision::ChangesRequested,
        "REVIEW_REQUIRED" => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::Unknown,
    }
}

fn status_state(word: Option<&str>) -> StatusState {
    match word {
        Some("SUCCESS") => StatusState::Success,
        Some("FAILURE") => StatusState::Failure,
        Some("ERROR") => StatusState::Error,
        Some("PENDING") => StatusState::Pending,
        Some("EXPECTED") => StatusState::Expected,
        Some(_) | None => StatusState::Unknown,
    }
}

fn mergeable(word: Option<&str>) -> Mergeable {
    match word {
        Some("MERGEABLE") => Mergeable::Mergeable,
        Some("CONFLICTING") => Mergeable::Conflicting,
        Some(_) | None => Mergeable::Unknown,
    }
}

fn merge_state(word: Option<&str>) -> MergeStateStatus {
    match word {
        Some("BEHIND") => MergeStateStatus::Behind,
        Some("BLOCKED") => MergeStateStatus::Blocked,
        Some("CLEAN") => MergeStateStatus::Clean,
        Some("DIRTY") => MergeStateStatus::Dirty,
        Some("DRAFT") => MergeStateStatus::Draft,
        Some("HAS_HOOKS") => MergeStateStatus::HasHooks,
        Some("UNSTABLE") => MergeStateStatus::Unstable,
        Some(_) | None => MergeStateStatus::Unknown,
    }
}

fn merge_method(word: &str) -> Option<MergeMethod> {
    match word {
        "MERGE" => Some(MergeMethod::Merge),
        "SQUASH" => Some(MergeMethod::Squash),
        "REBASE" => Some(MergeMethod::Rebase),
        _ => None,
    }
}

fn check_status(word: Option<&str>) -> CheckStatus {
    match word {
        Some("QUEUED") => CheckStatus::Queued,
        Some("IN_PROGRESS") => CheckStatus::InProgress,
        Some("PENDING") => CheckStatus::Pending,
        Some("REQUESTED") => CheckStatus::Requested,
        Some("WAITING") => CheckStatus::Waiting,
        Some("COMPLETED") => CheckStatus::Completed,
        Some(_) | None => CheckStatus::Unknown,
    }
}

fn check_conclusion(word: &str) -> CheckConclusion {
    match word {
        "ACTION_REQUIRED" => CheckConclusion::ActionRequired,
        "CANCELLED" => CheckConclusion::Cancelled,
        "FAILURE" => CheckConclusion::Failure,
        "NEUTRAL" => CheckConclusion::Neutral,
        "SKIPPED" => CheckConclusion::Skipped,
        "STALE" => CheckConclusion::Stale,
        "STARTUP_FAILURE" => CheckConclusion::StartupFailure,
        "SUCCESS" => CheckConclusion::Success,
        "TIMED_OUT" => CheckConclusion::TimedOut,
        _ => CheckConclusion::Unknown,
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` (GraphQL `DateTime` in UTC) as seconds since the Unix epoch; `None`
/// for any other shape.
fn iso_seconds(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_read_as_unix_seconds() {
        assert_eq!(iso_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_seconds("2024-02-29T12:34:56Z"), Some(1_709_210_096));
        for bad in [
            "2024-02-29 12:34:56Z",
            "2024-13-01T00:00:00Z",
            "2024-02-29T12:34:56+01:00",
        ] {
            assert_eq!(iso_seconds(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_queries_are_static_and_name_their_values_as_variables() {
        for query in [LIST, DETAIL_FULL, DETAIL_BASIC] {
            assert!(query.starts_with("query RcGit"), "{query}");
            for variable in ["$owner", "$name"] {
                assert!(query.contains(variable), "{query}");
            }
            assert!(!query.contains("mutation"), "{query}");
        }
        assert!(!DETAIL_BASIC.contains("viewerCan") && !DETAIL_BASIC.contains("isRequired"));
        assert!(DETAIL_FULL.contains(" baseRefOid ") && !DETAIL_BASIC.contains("baseRefOid"));
    }
}
