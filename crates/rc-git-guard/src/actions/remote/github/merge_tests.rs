//! The merge against the testkit's fake GitHub host, which answers only the exact read and the
//! exact mutation a correct merge sends and counts mutation requests apart from reads.

use std::time::Duration;

use rc_git_testkit::github::{FakeToken, GitHubServer, MergeBehaviour, MergeScript};
use serde_json::{Value, json};

use super::api::{Mutated, Session};
use super::testing::{detail_request_body, merge_request_body};
use super::*;
use crate::actions::remote::Cancel;
use crate::{Action, Decision, Policy};

pub(super) const TOKEN: &str = "ghp_merge_token_never_printed_0123";
pub(super) const ID: &str = "PR_kwDOmerge7";
pub(super) const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
pub(super) const MOVED: &str = "89abcdef0123456789abcdef0123456789abcdef";
pub(super) const MERGE_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";

pub(super) fn server() -> GitHubServer {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    server
}

pub(super) fn host_of(server: &GitHubServer) -> GitHubHost {
    GitHubHost::parse(&server.host()).unwrap()
}

pub(super) fn token() -> ApiToken {
    ApiToken::new(TOKEN.to_owned()).unwrap()
}

pub(super) fn target(host: &GitHubHost) -> RepositoryTarget {
    RepositoryTarget::on_host(host, &format!("http://{host}/octo/rc-git.git")).unwrap()
}

pub(super) fn oid(hex: &str) -> rc_git_core::Oid {
    rc_git_core::Oid::from_hex(hex).unwrap()
}

/// What the user saw: pull request 7, open at `head` on `main`.
pub(super) fn stamp(head: &str) -> PullRequestStamp {
    PullRequestStamp {
        number: 7,
        head_oid: oid(head),
        base_ref: "main".to_owned(),
        state: PullRequestState::Open,
    }
}

/// The detail read's answer for pull request 7 at `head` in `state`, one passing check.
pub(super) fn pull(head: &str, state: &str) -> Value {
    let check = json!({ "__typename": "CheckRun", "name": "build", "status": "COMPLETED",
        "conclusion": "SUCCESS", "detailsUrl": null, "databaseId": 1, "isRequired": true,
        "checkSuite": { "workflowRun": null } });
    let rollup = json!({ "state": "SUCCESS",
        "contexts": { "totalCount": 1, "nodes": [check] } });
    let pull_request = json!({
        "id": ID, "number": 7, "title": "Ship \u{202e}it", "body": "", "url": null,
        "isDraft": false, "state": state, "updatedAt": "2026-11-18T09:00:00Z",
        "author": { "login": "mona" }, "headRefName": "feature", "headRefOid": head,
        "baseRefName": "main", "isCrossRepository": false,
        "headRepositoryOwner": { "login": "octo" }, "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "viewerCanMergeAsAdmin": true, "viewerCanEnableAutoMerge": false,
        "viewerCanDisableAutoMerge": false, "viewerCanDeleteHeadRef": true,
        "autoMergeRequest": null,
        "commits": { "nodes": [ { "commit": { "statusCheckRollup": rollup } } ] }
    });
    json!({ "data": { "repository": {
        "mergeCommitAllowed": true, "squashMergeAllowed": true, "rebaseMergeAllowed": false,
        "autoMergeAllowed": false, "pullRequest": pull_request } } })
}

/// [`pull`] at [`HEAD`], open, with `key` set to `value`.
fn open_with(key: &str, value: Value) -> String {
    let mut answer = pull(HEAD, "OPEN");
    answer["data"]["repository"]["pullRequest"][key] = value;
    answer.to_string()
}

pub(super) fn merged_reply() -> String {
    json!({ "data": { "mergePullRequest": { "pullRequest": {
        "number": 7, "state": "MERGED", "merged": true, "headRefOid": HEAD,
        "baseRefName": "main", "mergeCommit": { "oid": MERGE_COMMIT } } } } })
    .to_string()
}

/// Pull request 7 on `server`, open at [`HEAD`], merging with a squash as `behaviour` says.
fn script(server: &GitHubServer, behaviour: MergeBehaviour) {
    script_open(server, pull(HEAD, "OPEN").to_string(), behaviour);
}

fn script_open(server: &GitHubServer, open: String, behaviour: MergeBehaviour) {
    let target = target(&host_of(server));
    server.script_merge(MergeScript {
        read: detail_request_body(&target, 7, false),
        open,
        moved: pull(MOVED, "OPEN").to_string(),
        merged: pull(HEAD, "MERGED").to_string(),
        mutation: merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply: merged_reply(),
        behaviour,
    });
}

/// The parts of one merge.
struct Merge {
    host: GitHubHost,
    token: ApiToken,
    target: RepositoryTarget,
    stamp: PullRequestStamp,
    method: MergeMethod,
}

impl Merge {
    fn on(server: &GitHubServer) -> Self {
        let host = host_of(server);
        Self {
            target: target(&host),
            host,
            token: token(),
            stamp: stamp(HEAD),
            method: MergeMethod::Squash,
        }
    }

    fn target(&self) -> MergeTarget<'_> {
        MergeTarget {
            host: &self.host,
            token: &self.token,
            login: "octocat",
            target: &self.target,
            stamp: &self.stamp,
            method: self.method,
            kind: MergeKind::Plain,
        }
    }

    fn plan(&self) -> Result<MergePlan, MergeRefusal> {
        plan_merge_pull_request(&Policy::default(), &self.target(), &Cancel::new())
    }

    /// The merge with `answer`, under `cancel`.
    fn run(
        &self,
        answer: Option<&Decision>,
        cancel: &Cancel,
    ) -> Result<MergeOutcome, MergeRefusal> {
        let request = MergeRequest {
            merge: self.target(),
            answer: answer.map(|decision| MergeAnswer {
                decision,
                typed: "",
            }),
            on_send: &|| {},
        };
        let result = merge_pull_request(&Policy::default(), request, cancel);
        no_token(&result);
        result
    }

    /// Plans, confirms what the plan shows, and merges.
    fn confirmed(&self) -> Result<MergeOutcome, MergeRefusal> {
        let plan = self.plan().unwrap();
        self.run(Some(&plan.decision), &Cancel::new())
    }
}

pub(super) fn no_token<T: std::fmt::Debug, E: std::fmt::Debug + std::fmt::Display>(
    result: &Result<T, E>,
) {
    let printed = match result {
        Ok(value) => format!("{value:?}"),
        Err(error) => format!("{error} {error:?}"),
    };
    assert!(!printed.contains(TOKEN), "{printed}");
}

pub(super) fn mutations(server: &GitHubServer) -> Vec<rc_git_testkit::github::RecordedRequest> {
    server
        .recorded()
        .into_iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains("mutation"))
        .collect()
}

#[test]
fn a_merge_sends_one_mutation_carrying_the_confirmed_head_and_method() {
    let server = server();
    script(&server, MergeBehaviour::Merge);
    let merge = Merge::on(&server);
    let outcome = merge.confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: Some(oid(MERGE_COMMIT)),
            base: Some("main".to_owned()),
            settled: Settled::Answer
        }
    );
    assert_eq!(server.mutation_requests(), 1);
    let sent = &mutations(&server)[0];
    assert_eq!(
        String::from_utf8_lossy(&sent.body),
        merge_request_body(ID, HEAD, MergeMethod::Squash)
    );
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(
        body["variables"],
        json!({ "id": ID, "head": HEAD, "method": "SQUASH" }),
        "the confirmed pull request, head and method, spelled as GitHub's schema has them"
    );
    assert_eq!(body["query"], super::mutation::MERGE);
    assert_eq!(sent.target, "/api/graphql");
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert!(server.merged());
    assert_eq!(
        server.graphql_reads(),
        2,
        "the plan's read and the merge's own"
    );
}

#[test]
fn the_challenge_is_built_from_the_fresh_read() {
    let server = server();
    script(&server, MergeBehaviour::Merge);
    let plan = Merge::on(&server).plan().unwrap();
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("{:?}", plan.decision);
    };
    assert_eq!(challenge.typed_phrase(), None);
    let host = host_of(&server);
    assert_eq!(
        challenge.summary(),
        format!(
            "Merge this pull request on GitHub. GitHub merges it for everyone and rc-git cannot \
             undo it; the local repository is unchanged until you fetch.\n\
             Repository: octo/rc-git on {host}\n\
             Pull request: #7\n\
             Account: octocat@{host}\n\
             Base: \u{201c}main\u{201d}\n\
             Head: 0123456 on \u{201c}feature\u{201d}\n\
             Method: a squash merge\n\
             Checks: 1 of 1 checks passed\n\
             Protection: your account can merge past branch protection, so a requirement that \
             starts blocking this pull request before GitHub runs the merge will not stop it\n\
             Title: \u{201c}Ship \\u{{202e}}it\u{201d}"
        )
    );
    assert_eq!(server.mutation_requests(), 0, "a plan only reads");
}

#[test]
fn a_stale_head_a_changed_base_or_a_closed_pull_request_sends_no_mutation() {
    for (seen, what) in [
        (stamp(MOVED), "its head is now"),
        (
            PullRequestStamp {
                base_ref: "develop".to_owned(),
                ..stamp(HEAD)
            },
            "its base is now 'main', not 'develop'",
        ),
    ] {
        let server = server();
        script(&server, MergeBehaviour::Merge);
        let mut merge = Merge::on(&server);
        merge.stamp = seen;
        let refused = merge.plan().unwrap_err();
        assert!(
            matches!(&refused, MergeRefusal::Changed { what: w, .. } if w.contains(what)),
            "{refused}"
        );
        let answer = Decision::Allow;
        let error = merge.run(Some(&answer), &Cancel::new()).unwrap_err();
        assert!(matches!(error, MergeRefusal::Changed { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0);
    }
    let closed = server();
    script_open(
        &closed,
        pull(HEAD, "CLOSED").to_string(),
        MergeBehaviour::Merge,
    );
    let error = Merge::on(&closed).plan().unwrap_err();
    assert!(
        error.to_string().contains("it is now closed, not open"),
        "{error}"
    );
    assert_eq!(closed.mutation_requests(), 0);
}

#[test]
fn a_head_that_moves_between_the_plan_and_the_merge_sends_no_mutation() {
    let server = server();
    script(
        &server,
        MergeBehaviour::MoveHeadAfter {
            reads: 1,
            refusal: String::new(),
        },
    );
    let error = Merge::on(&server).confirmed().unwrap_err();
    assert!(
        matches!(&error, MergeRefusal::Changed { what, .. } if what.contains(MOVED)),
        "{error}"
    );
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn a_draft_a_disallowed_method_or_a_conflict_sends_no_mutation() {
    let cases = [
        (
            open_with("isDraft", json!(true)),
            MergeMethod::Squash,
            MergeBlocker::Draft,
        ),
        (
            pull(HEAD, "OPEN").to_string(),
            MergeMethod::Rebase,
            MergeBlocker::MethodNotAllowed(MergeMethod::Rebase),
        ),
        (
            open_with("mergeable", json!("CONFLICTING")),
            MergeMethod::Squash,
            MergeBlocker::Conflicting,
        ),
        (
            open_with("mergeable", json!("UNKNOWN")),
            MergeMethod::Squash,
            MergeBlocker::MergeabilityUnknown,
        ),
        (
            open_with("mergeStateStatus", json!("BLOCKED")),
            MergeMethod::Squash,
            MergeBlocker::Blocked,
        ),
        (
            open_with("mergeStateStatus", json!("BEHIND")),
            MergeMethod::Squash,
            MergeBlocker::Behind,
        ),
        (
            open_with("mergeStateStatus", json!("DIRTY")),
            MergeMethod::Squash,
            MergeBlocker::Conflicting,
        ),
        (
            open_with("mergeStateStatus", json!("UNKNOWN")),
            MergeMethod::Squash,
            MergeBlocker::MergeStateUnknown,
        ),
        (
            open_with("id", Value::Null),
            MergeMethod::Squash,
            MergeBlocker::NoId,
        ),
    ];
    for (open, method, blocker) in cases {
        let server = server();
        script_open(&server, open, MergeBehaviour::Merge);
        let mut merge = Merge::on(&server);
        merge.method = method;
        let error = merge.plan().unwrap_err();
        assert_eq!(
            error,
            MergeRefusal::Blocked {
                pull_request: "octo/rc-git#7".to_owned(),
                blocker
            }
        );
        let answer = Decision::Allow;
        let error = merge.run(Some(&answer), &Cancel::new()).unwrap_err();
        assert!(matches!(error, MergeRefusal::Blocked { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0, "{blocker}");
    }
}

#[test]
fn no_answer_or_an_answer_to_another_challenge_sends_no_mutation() {
    let server = server();
    script(&server, MergeBehaviour::Merge);
    let merge = Merge::on(&server);
    let error = merge.run(None, &Cancel::new()).unwrap_err();
    assert!(
        matches!(error, MergeRefusal::NotConfirmed { phrase: None, .. }),
        "{error}"
    );
    let delete = Policy::default().evaluate(&Action::DeleteTag {
        name: "v1".to_owned(),
    });
    let error = merge.run(Some(&delete), &Cancel::new()).unwrap_err();
    assert!(
        error.to_string().contains("no longer describes it"),
        "{error}"
    );
    let squash = merge.plan().unwrap().decision;
    let mut by_merge_commit = Merge::on(&server);
    by_merge_commit.method = MergeMethod::Merge;
    let error = by_merge_commit
        .run(Some(&squash), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, MergeRefusal::Changed { .. }), "{error}");
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn a_check_that_finished_since_the_challenge_was_shown_sends_no_mutation() {
    let server = server();
    let mut pending = pull(HEAD, "OPEN");
    pending["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
        ["contexts"]["nodes"][0]["status"] = json!("IN_PROGRESS");
    script_open(&server, pending.to_string(), MergeBehaviour::Merge);
    let merge = Merge::on(&server);
    let shown = merge.plan().unwrap().decision;
    script(&server, MergeBehaviour::Merge);
    let error = merge.run(Some(&shown), &Cancel::new()).unwrap_err();
    assert!(
        matches!(&error, MergeRefusal::Changed { what, .. } if what.contains("1 of 1 checks passed")),
        "{error}"
    );
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn the_mutation_primitive_sends_nothing_for_another_action_s_proof() {
    let server = server();
    let host = host_of(&server);
    let token = token();
    let fetch = Policy::default().authorize(&Action::Fetch, None).unwrap();
    let sent = Session::new(&host, &token).mutate(&fetch, &Cancel::new());
    assert!(
        matches!(sent, Mutated::NotSent(GitHubError::InvalidValue { .. })),
        "{sent:?}"
    );
    assert_eq!(server.requests(), 0);
}

#[test]
fn the_mutation_primitive_sends_nothing_to_a_repository_on_another_host() {
    let account = server();
    let elsewhere = server();
    script(&account, MergeBehaviour::Merge);
    script(&elsewhere, MergeBehaviour::Merge);
    let host = host_of(&account);
    let token = token();
    let detail = read_pull_request(&host, &token, &target(&host), 7, &Cancel::new()).unwrap();
    let foreign = target(&host_of(&elsewhere));
    let merge = PullRequestMerge::new(&foreign, "octocat", &detail, MergeMethod::Squash).unwrap();
    let action = Action::MergePullRequest(Box::new(merge));
    let Decision::Confirm(challenge) = Policy::default().evaluate(&action) else {
        panic!("a merge is confirmed");
    };
    let proof = challenge.verify("").unwrap();
    let sent = Session::new(&host, &token).mutate(&proof, &Cancel::new());
    assert!(
        matches!(sent, Mutated::NotSent(GitHubError::ForeignRemote { .. })),
        "{sent:?}"
    );
    assert_eq!(
        account.mutation_requests() + elsewhere.mutation_requests(),
        0
    );
    assert_eq!(elsewhere.requests(), 0);
}

#[test]
fn a_read_never_sends_a_mutation_document() {
    let server = server();
    let host = host_of(&server);
    let token = token();
    let error = Session::new(&host, &token)
        .graphql("read", super::mutation::MERGE, &json!({}), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, GitHubError::InvalidValue { .. }), "{error}");
    assert_eq!(server.requests(), 0);
}

#[test]
fn a_head_that_moves_after_the_merge_s_read_is_refused_by_the_host_once() {
    let server = server();
    let refusal = json!({ "data": { "mergePullRequest": null }, "errors": [ {
        "type": "UNPROCESSABLE", "path": ["mergePullRequest"],
        "message": "Head branch was modified. Review and try the merge again." } ] });
    script(
        &server,
        MergeBehaviour::MoveHeadAfter {
            reads: 2,
            refusal: refusal.to_string(),
        },
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Refused {
            reason: "Head branch was modified. Review and try the merge again.".to_owned()
        }
    );
    assert_eq!(server.mutation_requests(), 1);
    assert!(!server.merged());
}

#[test]
fn a_refusal_passes_the_host_s_reason_through_without_the_token() {
    let echoed = json!({ "data": { "mergePullRequest": null }, "errors": [ {
        "type": "FORBIDDEN", "message": format!("you sent {TOKEN} and may not merge") } ] });
    let cases = [
        ("200 OK", echoed.to_string(), "<redacted> and may not merge"),
        (
            "422 Unprocessable Entity",
            json!({ "message": format!("Base branch was modified {TOKEN}") }).to_string(),
            "HTTP 422: Base branch was modified <redacted>",
        ),
        (
            "401 Unauthorized",
            json!({ "message": "Bad credentials" }).to_string(),
            "rejected the token",
        ),
        (
            "200 OK",
            json!({ "errors": [ { "type": "RATE_LIMITED", "message": "API rate limit exceeded" } ] })
                .to_string(),
            "rate limited by the host",
        ),
    ];
    for (status, body, reason) in cases {
        let server = server();
        script(&server, MergeBehaviour::Refuse { status, body });
        let outcome = Merge::on(&server).confirmed().unwrap();
        let MergeOutcome::Refused { reason: given } = &outcome else {
            panic!("{status}: {outcome:?}");
        };
        assert!(given.contains(reason), "{status}: {given}");
        assert_eq!(server.mutation_requests(), 1, "{status}");
        assert_eq!(
            server.graphql_reads(),
            2,
            "{status}: a refusal needs no read after"
        );
    }
}

#[test]
fn a_server_error_or_an_unplaced_error_is_unknown_never_refused() {
    let timeout = json!({ "data": null, "errors": [ { "message":
        "Something went wrong while executing your query. This may be the result of a timeout" } ] });
    for (status, body) in [
        (
            "502 Bad Gateway",
            json!({ "message": "Server Error" }).to_string(),
        ),
        ("200 OK", timeout.to_string()),
        ("200 OK", "not json".to_owned()),
    ] {
        let server = server();
        script(&server, MergeBehaviour::Refuse { status, body });
        let outcome = Merge::on(&server).confirmed().unwrap();
        assert!(
            matches!(
                &outcome,
                MergeOutcome::Unknown { after: AfterLoss::StillOpen { head }, .. } if *head == oid(HEAD)
            ),
            "{status}: {outcome:?}"
        );
        assert_eq!(server.mutation_requests(), 1, "{status}");
        assert_eq!(
            server.graphql_reads(),
            3,
            "{status}: one read after the loss"
        );
    }
}

/// A cancel whose idle budget gives up on a stalled answer after a second.
fn impatient() -> Cancel {
    Cancel::new().with_idle_timeout(Duration::from_secs(1))
}

#[test]
fn an_applied_merge_whose_answer_stalls_is_settled_by_one_read() {
    let server = server();
    script(&server, MergeBehaviour::MergeThenStall);
    let merge = Merge::on(&server);
    let plan = merge.plan().unwrap();
    let outcome = merge.run(Some(&plan.decision), &impatient()).unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: None,
            base: Some("main".to_owned()),
            settled: Settled::Reread
        }
    );
    assert_eq!(server.mutation_requests(), 1);
    assert_eq!(server.graphql_reads(), 3);
}

#[test]
fn an_applied_merge_whose_connection_drops_is_settled_by_one_read() {
    let server = server();
    script(&server, MergeBehaviour::MergeThenDrop);
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: None,
            base: Some("main".to_owned()),
            settled: Settled::Reread
        }
    );
    assert_eq!(server.mutation_requests(), 1);
}

#[test]
fn a_dropped_merge_that_never_applied_says_the_state_is_unknown() {
    let server = server();
    script(&server, MergeBehaviour::Drop);
    let outcome = Merge::on(&server).confirmed().unwrap();
    let MergeOutcome::Unknown { problem, after } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(after, &AfterLoss::StillOpen { head: oid(HEAD) });
    assert!(problem.contains("merge the pull request"), "{problem}");
    assert_eq!(server.mutation_requests(), 1);
    assert!(!server.merged());
}

#[test]
fn a_cancel_up_to_the_send_sends_nothing() {
    let server = server();
    script(&server, MergeBehaviour::Merge);
    let merge = Merge::on(&server);
    let plan = merge.plan().unwrap();
    let cancelled = Cancel::new();
    cancelled.signal();
    let error = merge.run(Some(&plan.decision), &cancelled).unwrap_err();
    assert!(matches!(error, MergeRefusal::Cancelled { .. }), "{error}");
    let at_send = Cancel::new();
    let raise = at_send.clone();
    let signal = move || raise.signal();
    let request = MergeRequest {
        merge: merge.target(),
        answer: Some(MergeAnswer {
            decision: &plan.decision,
            typed: "",
        }),
        on_send: &signal,
    };
    let error = merge_pull_request(&Policy::default(), request, &at_send).unwrap_err();
    assert!(matches!(error, MergeRefusal::Cancelled { .. }), "{error}");
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn a_cancel_after_the_send_cannot_stop_the_merge_and_reads_its_outcome() {
    let server = server();
    script(&server, MergeBehaviour::MergeThenStall);
    let merge = Merge::on(&server);
    let plan = merge.plan().unwrap();
    let cancel = Cancel::new();
    let raise = cancel.clone();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while server.mutation_requests() == 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            raise.signal();
        });
        let outcome = merge.run(Some(&plan.decision), &cancel).unwrap();
        assert_eq!(
            outcome,
            MergeOutcome::Merged {
                merge_commit: None,
                base: Some("main".to_owned()),
                settled: Settled::Reread
            }
        );
    });
    assert_eq!(server.mutation_requests(), 1);
}

#[test]
fn a_repository_on_another_host_gets_nothing() {
    let account = server();
    let elsewhere = server();
    script(&elsewhere, MergeBehaviour::Merge);
    let mut merge = Merge::on(&account);
    merge.target = target(&host_of(&elsewhere));
    let error = merge.plan().unwrap_err();
    assert!(
        matches!(
            &error,
            MergeRefusal::Read {
                source: GitHubError::ForeignRemote { .. },
                ..
            }
        ),
        "{error}"
    );
    let answer = Decision::Allow;
    let error = merge.run(Some(&answer), &Cancel::new()).unwrap_err();
    assert!(matches!(error, MergeRefusal::Read { .. }), "{error}");
    assert_eq!(account.requests() + elsewhere.requests(), 0);
}

#[test]
fn a_host_without_the_merge_state_cannot_merge_from_rc_git() {
    let server = server();
    let target = target(&host_of(&server));
    let unknown = json!({ "errors": [ { "message": "Field 'viewerCanMergeAsAdmin' doesn't exist",
        "extensions": { "code": "undefinedField" } } ] });
    server.script_graphql(
        &detail_request_body(&target, 7, false),
        &unknown.to_string(),
    );
    server.script_graphql(
        &detail_request_body(&target, 7, true),
        &pull(HEAD, "OPEN").to_string(),
    );
    let error = Merge::on(&server).plan().unwrap_err();
    assert!(
        matches!(
            error,
            MergeRefusal::Blocked {
                blocker: MergeBlocker::MergeStateUnreported,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(server.mutation_requests(), 0);
}

/// Pull request 7 on `server` as [`script`] scripts it, with the read once it merged and the
/// mutation's answer set by the test.
fn script_with(server: &GitHubServer, merged: &Value, reply: &Value, behaviour: MergeBehaviour) {
    let target = target(&host_of(server));
    server.script_merge(MergeScript {
        read: detail_request_body(&target, 7, false),
        open: pull(HEAD, "OPEN").to_string(),
        moved: pull(MOVED, "OPEN").to_string(),
        merged: merged.to_string(),
        mutation: merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply: reply.to_string(),
        behaviour,
    });
}

/// [`merged_reply`] with `baseRefName` set to `base`, or left out for `None`.
fn merged_into_reply(base: Option<&str>) -> Value {
    let mut reply: Value = serde_json::from_str(&merged_reply()).unwrap();
    let pull_request = &mut reply["data"]["mergePullRequest"]["pullRequest"];
    match base {
        Some(base) => pull_request["baseRefName"] = json!(base),
        None => drop(pull_request.as_object_mut().unwrap().remove("baseRefName")),
    }
    reply
}

/// [`pull`] at [`HEAD`], merged, into `base`.
fn merged_read_into(base: &str) -> Value {
    let mut read = pull(HEAD, "MERGED");
    read["data"]["repository"]["pullRequest"]["baseRefName"] = json!(base);
    read
}

#[test]
fn a_merge_github_says_landed_on_another_base_is_never_reported_as_merged() {
    let server = server();
    script_with(
        &server,
        &merged_read_into("release"),
        &merged_into_reply(Some("release")),
        MergeBehaviour::Merge,
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::MergedIntoOtherBase {
            confirmed: "main".to_owned(),
            actual: "release".to_owned(),
            merge_commit: Some(oid(MERGE_COMMIT)),
            settled: Settled::Answer
        }
    );
    assert_eq!(server.mutation_requests(), 1);
}

#[test]
fn a_merge_whose_answer_names_no_base_is_merged_without_a_base() {
    let server = server();
    script_with(
        &server,
        &pull(HEAD, "MERGED"),
        &merged_into_reply(None),
        MergeBehaviour::Merge,
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: Some(oid(MERGE_COMMIT)),
            base: None,
            settled: Settled::Answer
        }
    );
}

#[test]
fn a_lost_answer_settled_by_a_read_on_another_base_is_never_reported_as_merged() {
    let server = server();
    script_with(
        &server,
        &merged_read_into("release"),
        &merged_into_reply(Some("release")),
        MergeBehaviour::MergeThenDrop,
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::MergedIntoOtherBase {
            confirmed: "main".to_owned(),
            actual: "release".to_owned(),
            merge_commit: None,
            settled: Settled::Reread
        }
    );
    assert_eq!(server.mutation_requests(), 1);
}

/// The headers of an answer whose rate limit is spent.
fn spent() -> Vec<(String, String)> {
    vec![
        ("x-ratelimit-remaining".to_owned(), "0".to_owned()),
        ("x-ratelimit-reset".to_owned(), "1900000000".to_owned()),
    ]
}

#[test]
fn a_merge_the_answer_reports_is_merged_whatever_errors_and_rate_limit_came_with_it() {
    let server = server();
    let mut reply = merged_into_reply(Some("main"));
    reply["errors"] = json!([ { "type": "RATE_LIMITED", "message": "API rate limit exceeded" } ]);
    script_with(
        &server,
        &pull(HEAD, "MERGED"),
        &reply,
        MergeBehaviour::Answer {
            merges: true,
            body: reply.to_string(),
            headers: spent(),
        },
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: Some(oid(MERGE_COMMIT)),
            base: Some("main".to_owned()),
            settled: Settled::Answer
        }
    );
    assert_eq!(
        server.graphql_reads(),
        2,
        "no read after an answer that says"
    );
}

#[test]
fn a_spent_rate_limit_refuses_only_an_answer_without_a_merge_whose_error_says_so() {
    let limited = json!({ "data": { "mergePullRequest": null }, "errors": [ {
        "type": "RATE_LIMITED", "message": "API rate limit exceeded" } ] });
    let unplaced = json!({ "data": { "mergePullRequest": null }, "errors": [ {
        "message": "Something went wrong" } ] });
    let server = server();
    script_with(
        &server,
        &pull(HEAD, "MERGED"),
        &limited,
        MergeBehaviour::Answer {
            merges: false,
            body: limited.to_string(),
            headers: spent(),
        },
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert!(
        matches!(&outcome, MergeOutcome::Refused { reason } if reason.contains("rate limited")),
        "{outcome:?}"
    );
    assert_eq!(server.graphql_reads(), 2);

    let server = super::merge_tests::server();
    script_with(
        &server,
        &pull(HEAD, "MERGED"),
        &unplaced,
        MergeBehaviour::Answer {
            merges: true,
            body: unplaced.to_string(),
            headers: spent(),
        },
    );
    let outcome = Merge::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: None,
            base: Some("main".to_owned()),
            settled: Settled::Reread
        },
        "a spent limit with an error that does not say is settled by the read"
    );
    assert_eq!(server.graphql_reads(), 3);
}

#[test]
fn only_a_schema_validation_code_shows_the_merge_never_ran() {
    for code in ["someCodeGitHubAddsLater", "INTERNAL", "timeout"] {
        let answer = json!({ "data": { "mergePullRequest": null }, "errors": [ {
            "message": "it went wrong", "extensions": { "code": code } } ] });
        let server = server();
        script_with(
            &server,
            &pull(HEAD, "MERGED"),
            &answer,
            MergeBehaviour::Answer {
                merges: true,
                body: answer.to_string(),
                headers: Vec::new(),
            },
        );
        let outcome = Merge::on(&server).confirmed().unwrap();
        assert_eq!(
            outcome,
            MergeOutcome::Merged {
                merge_commit: None,
                base: Some("main".to_owned()),
                settled: Settled::Reread
            },
            "{code}: an applied merge behind an unknown code is settled by the read"
        );
    }
    for code in super::merge::VALIDATION_CODES {
        let answer = json!({ "errors": [ {
            "message": "Field 'x' doesn't exist", "extensions": { "code": code } } ] });
        let server = server();
        script_with(
            &server,
            &pull(HEAD, "MERGED"),
            &answer,
            MergeBehaviour::Answer {
                merges: false,
                body: answer.to_string(),
                headers: Vec::new(),
            },
        );
        let outcome = Merge::on(&server).confirmed().unwrap();
        assert_eq!(
            outcome,
            MergeOutcome::Refused {
                reason: "Field 'x' doesn't exist".to_owned()
            },
            "{code}"
        );
        assert_eq!(server.graphql_reads(), 2, "{code}");
    }
}

#[test]
fn an_account_that_gains_the_bypass_right_after_the_challenge_sends_no_mutation() {
    let server = server();
    let mut plain = pull(HEAD, "OPEN");
    plain["data"]["repository"]["pullRequest"]["viewerCanMergeAsAdmin"] = json!(false);
    let target = target(&host_of(&server));
    server.script_merge(MergeScript {
        read: detail_request_body(&target, 7, false),
        open: plain.to_string(),
        moved: pull(HEAD, "OPEN").to_string(),
        merged: pull(HEAD, "MERGED").to_string(),
        mutation: merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply: merged_reply(),
        behaviour: MergeBehaviour::ChangeAfter { reads: 1 },
    });
    let merge = Merge::on(&server);
    let shown = merge.plan().unwrap().decision;
    let Decision::Confirm(challenge) = &shown else {
        panic!("{shown:?}");
    };
    assert!(
        !challenge.summary().contains("Protection:"),
        "{}",
        challenge.summary()
    );
    let error = merge.run(Some(&shown), &Cancel::new()).unwrap_err();
    assert!(
        matches!(&error, MergeRefusal::Changed { what, .. }
            if what.contains("Protection: your account can merge past branch protection")),
        "{error}"
    );
    assert_eq!(server.mutation_requests(), 0);
}
