//! Turning auto-merge on and off against the testkit's fake GitHub host, which answers only the
//! exact read and the exact mutation a correct change sends and counts each mutation by name.

use std::time::Duration;

use rc_git_testkit::github::{GitHubServer, MergeBehaviour, MergeScript};
use serde_json::{Value, json};

use super::merge_tests::{HEAD, ID, MOVED, host_of, mutations, no_token, pull, server, stamp};
use super::merge_tests::{target, token};
use super::mutation::{DISABLE_AUTO_MERGE, ENABLE_AUTO_MERGE, Mutation, MutationRequest};
use super::testing::{
    detail_request_body, disable_auto_merge_request_body, enable_auto_merge_request_body,
};
use super::*;
use crate::actions::remote::Cancel;
use crate::{Action, Decision, Policy};

const ENABLE: &str = "RcGitEnableAutoMerge";
const DISABLE: &str = "RcGitDisableAutoMerge";
const MERGE: &str = "RcGitMergePullRequest";

/// [`pull`] at `head` in `state`, blocked by branch protection, in a repository that allows
/// auto-merge, read by an account GitHub says may turn it on and off.
fn blocked(head: &str, state: &str) -> Value {
    let mut answer = pull(head, state);
    answer["data"]["repository"]["autoMergeAllowed"] = json!(true);
    let pull_request = &mut answer["data"]["repository"]["pullRequest"];
    pull_request["mergeStateStatus"] = json!("BLOCKED");
    pull_request["viewerCanEnableAutoMerge"] = json!(true);
    pull_request["viewerCanDisableAutoMerge"] = json!(true);
    answer
}

/// `answer` with squash auto-merge on, turned on by `mona`.
fn on(mut answer: Value) -> Value {
    answer["data"]["repository"]["pullRequest"]["autoMergeRequest"] =
        json!({ "mergeMethod": "SQUASH", "enabledBy": { "login": "mona" } });
    answer
}

/// `answer` with the pull request's `key` set to `value`.
fn with(mut answer: Value, key: &str, value: Value) -> Value {
    answer["data"]["repository"]["pullRequest"][key] = value;
    answer
}

fn enabled_reply() -> Value {
    json!({ "data": { "enablePullRequestAutoMerge": { "pullRequest": {
        "number": 7, "state": "OPEN", "headRefOid": HEAD, "baseRefName": "main",
        "autoMergeRequest": { "mergeMethod": "SQUASH" } } } } })
}

fn disabled_reply() -> Value {
    json!({ "data": { "disablePullRequestAutoMerge": { "pullRequest": {
        "number": 7, "state": "OPEN", "headRefOid": HEAD, "autoMergeRequest": null } } } })
}

/// One pull request on `server` for `change`: `open` before it, `moved` once the behaviour says,
/// `applied` once the change went through, answering the change's exact mutation with `reply`.
struct Script {
    open: Value,
    moved: Value,
    applied: Value,
    reply: Value,
}

impl Script {
    /// Turning auto-merge on for a blocked pull request at [`HEAD`].
    fn enable() -> Self {
        Self {
            open: blocked(HEAD, "OPEN"),
            moved: blocked(MOVED, "OPEN"),
            applied: on(blocked(HEAD, "OPEN")),
            reply: enabled_reply(),
        }
    }

    /// Turning auto-merge off for a blocked pull request at [`HEAD`] with it on.
    fn disable() -> Self {
        Self {
            open: on(blocked(HEAD, "OPEN")),
            moved: on(blocked(MOVED, "OPEN")),
            applied: blocked(HEAD, "OPEN"),
            reply: disabled_reply(),
        }
    }

    fn serve(self, server: &GitHubServer, change: AutoMergeChange, behaviour: MergeBehaviour) {
        let target = target(&host_of(server));
        let mutation = match change {
            AutoMergeChange::Enable(method) => enable_auto_merge_request_body(ID, HEAD, method),
            AutoMergeChange::Disable => disable_auto_merge_request_body(ID),
        };
        server.script_merge(MergeScript {
            read: detail_request_body(&target, 7, false),
            open: self.open.to_string(),
            moved: self.moved.to_string(),
            merged: self.applied.to_string(),
            mutation,
            reply: self.reply.to_string(),
            behaviour,
        });
    }
}

const SQUASH: AutoMergeChange = AutoMergeChange::Enable(MergeMethod::Squash);

/// The parts of one auto-merge change: what is set, on which pull request, by whom.
struct Setting {
    host: GitHubHost,
    token: ApiToken,
    target: RepositoryTarget,
    stamp: PullRequestStamp,
    change: AutoMergeChange,
}

impl Setting {
    fn on(server: &GitHubServer, change: AutoMergeChange) -> Self {
        let host = host_of(server);
        Self {
            target: target(&host),
            host,
            token: token(),
            stamp: stamp(HEAD),
            change,
        }
    }

    fn who(&self) -> ActionTarget<'_> {
        ActionTarget {
            host: &self.host,
            token: &self.token,
            login: "octocat",
            target: &self.target,
            stamp: &self.stamp,
        }
    }

    fn plan(&self) -> Result<ActionPlan, ActionRefusal> {
        plan_auto_merge(&Policy::default(), &self.who(), self.change, &Cancel::new())
    }

    fn run(
        &self,
        answer: Option<&Decision>,
        cancel: &Cancel,
    ) -> Result<AutoMergeOutcome, ActionRefusal> {
        let request = AutoMergeRequest {
            who: self.who(),
            change: self.change,
            answer,
            on_send: &|| {},
        };
        let result = set_auto_merge(&Policy::default(), request, cancel);
        no_token(&result);
        result
    }

    fn confirmed(&self) -> Result<AutoMergeOutcome, ActionRefusal> {
        let plan = self.plan().unwrap();
        self.run(Some(&plan.decision), &Cancel::new())
    }
}

/// The server's mutation requests, each kind counted: `(enable, disable, merge, all)`.
fn counts(server: &GitHubServer) -> (usize, usize, usize, usize) {
    (
        server.mutations_named(ENABLE),
        server.mutations_named(DISABLE),
        server.mutations_named(MERGE),
        server.mutation_requests(),
    )
}

#[test]
fn turning_auto_merge_on_sends_one_mutation_with_the_confirmed_id_head_and_method() {
    let server = server();
    Script::enable().serve(&server, SQUASH, MergeBehaviour::Merge);
    let outcome = Setting::on(&server, SQUASH).confirmed().unwrap();
    assert_eq!(
        outcome,
        AutoMergeOutcome::Done {
            settled: Settled::Answer
        }
    );
    assert_eq!(counts(&server), (1, 0, 0, 1));
    let sent = &mutations(&server)[0];
    assert_eq!(
        String::from_utf8_lossy(&sent.body),
        enable_auto_merge_request_body(ID, HEAD, MergeMethod::Squash)
    );
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body["query"], ENABLE_AUTO_MERGE);
    assert_eq!(
        body["variables"],
        json!({ "id": ID, "head": HEAD, "method": "SQUASH" })
    );
    assert_eq!(sent.target, "/api/graphql");
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {}", super::merge_tests::TOKEN).as_str())
    );
    assert_eq!(
        server.graphql_reads(),
        2,
        "the plan's read and the change's own"
    );
}

#[test]
fn turning_auto_merge_off_sends_one_mutation_with_the_confirmed_id() {
    let server = server();
    Script::disable().serve(&server, AutoMergeChange::Disable, MergeBehaviour::Merge);
    let outcome = Setting::on(&server, AutoMergeChange::Disable)
        .confirmed()
        .unwrap();
    assert_eq!(
        outcome,
        AutoMergeOutcome::Done {
            settled: Settled::Answer
        }
    );
    assert_eq!(counts(&server), (0, 1, 0, 1));
    let body: Value = serde_json::from_slice(&mutations(&server)[0].body).unwrap();
    assert_eq!(body["query"], DISABLE_AUTO_MERGE);
    assert_eq!(body["variables"], json!({ "id": ID }));
}

#[test]
fn the_challenge_to_turn_it_on_is_built_from_the_fresh_read() {
    let server = server();
    Script::enable().serve(&server, SQUASH, MergeBehaviour::Merge);
    let plan = Setting::on(&server, SQUASH).plan().unwrap();
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("{:?}", plan.decision);
    };
    let host = host_of(&server);
    assert_eq!(
        challenge.summary(),
        format!(
            "Turn on auto-merge for this pull request on GitHub. GitHub merges it for everyone \
             and rc-git cannot undo it; the local repository is unchanged until you fetch.\n\
             Repository: octo/rc-git on {host}\n\
             Pull request: #7\n\
             Account: octocat@{host}\n\
             Base: \u{201c}main\u{201d}\n\
             Head: 0123456 on \u{201c}feature\u{201d}\n\
             Method: a squash merge\n\
             Checks: 1 of 1 checks passed\n\
             Merges by itself: GitHub will merge this pull request by itself when its \
             requirements are met, including commits pushed to its head branch after this \
             confirmation; rc-git will not ask again\n\
             Title: \u{201c}Ship \\u{{202e}}it\u{201d}"
        )
    );
    assert_eq!(counts(&server).3, 0, "a plan only reads");
}

#[test]
fn no_answer_or_an_answer_to_another_challenge_sends_nothing() {
    for (script, change) in [
        (Script::enable as fn() -> Script, SQUASH),
        (Script::disable, AutoMergeChange::Disable),
    ] {
        let server = server();
        script().serve(&server, change, MergeBehaviour::Merge);
        let ours = Setting::on(&server, change);
        let error = ours.run(None, &Cancel::new()).unwrap_err();
        assert!(
            matches!(error, ActionRefusal::NotConfirmed { .. }),
            "{error}"
        );
        let delete = Policy::default().evaluate(&Action::DeleteTag {
            name: "v1".to_owned(),
        });
        let mut others = vec![delete];
        let merge_detail =
            read_pull_request(&ours.host, &ours.token, &ours.target, 7, &Cancel::new()).unwrap();
        let mut mergeable = merge_detail.clone();
        mergeable.merge_state = Some(MergeStateStatus::Clean);
        let merge = PullRequestMerge::new(&ours.target, "octocat", &mergeable, MergeMethod::Squash)
            .unwrap();
        others.push(Policy::default().evaluate(&Action::MergePullRequest(Box::new(merge))));
        let other_change = match change {
            AutoMergeChange::Enable(_) => {
                let mut on = merge_detail.clone();
                on.auto_merge = Some(AutoMerge {
                    method: Some(MergeMethod::Squash),
                    enabled_by: None,
                });
                Action::DisableAutoMerge(Box::new(
                    AutoMergeDisable::new(&ours.target, "octocat", &on).unwrap(),
                ))
            }
            AutoMergeChange::Disable => {
                let mut off = merge_detail.clone();
                off.auto_merge = None;
                Action::EnableAutoMerge(Box::new(
                    AutoMergeEnable::new(&ours.target, "octocat", &off, MergeMethod::Squash)
                        .unwrap(),
                ))
            }
        };
        others.push(Policy::default().evaluate(&other_change));
        for other in &others {
            let error = ours.run(Some(other), &Cancel::new()).unwrap_err();
            assert!(
                matches!(&error, ActionRefusal::Changed { what, .. }
                    if what.contains("no longer describes it")),
                "{error}"
            );
        }
        let mut by_merge_commit = Setting::on(&server, change);
        if let AutoMergeChange::Enable(_) = change {
            let squash = ours.plan().unwrap().decision;
            by_merge_commit.change = AutoMergeChange::Enable(MergeMethod::Merge);
            let error = by_merge_commit
                .run(Some(&squash), &Cancel::new())
                .unwrap_err();
            assert!(matches!(error, ActionRefusal::Changed { .. }), "{error}");
        }
        assert_eq!(counts(&server), (0, 0, 0, 0), "{change:?}");
    }
}

#[test]
fn a_proof_names_only_its_own_document() {
    let server = server();
    Script::enable().serve(&server, SQUASH, MergeBehaviour::Merge);
    let change = Setting::on(&server, SQUASH);
    let detail = read_pull_request(
        &change.host,
        &change.token,
        &change.target,
        7,
        &Cancel::new(),
    )
    .unwrap();
    let enable = Action::EnableAutoMerge(Box::new(
        AutoMergeEnable::new(&change.target, "octocat", &detail, MergeMethod::Squash).unwrap(),
    ));
    let mut on = detail.clone();
    on.auto_merge = Some(AutoMerge {
        method: Some(MergeMethod::Squash),
        enabled_by: None,
    });
    let disable = Action::DisableAutoMerge(Box::new(
        AutoMergeDisable::new(&change.target, "octocat", &on).unwrap(),
    ));
    for (action, document) in [(enable, ENABLE_AUTO_MERGE), (disable, DISABLE_AUTO_MERGE)] {
        let proof = Policy::default().authorize(&action, None).unwrap();
        let mutation = Mutation::of(&proof).unwrap();
        let Some(MutationRequest::Graphql { document: sent, .. }) = mutation.request(0) else {
            panic!("{action:?}");
        };
        assert_eq!(sent, document);
        assert_eq!(mutation.request(1), None, "one request, once");
    }
}

#[test]
fn a_stale_head_a_changed_base_or_a_closed_pull_request_sends_nothing() {
    for (script, change) in [
        (Script::enable as fn() -> Script, SQUASH),
        (Script::disable, AutoMergeChange::Disable),
    ] {
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
            script().serve(&server, change, MergeBehaviour::Merge);
            let mut ours = Setting::on(&server, change);
            ours.stamp = seen;
            let refused = ours.plan().unwrap_err();
            assert!(
                matches!(&refused, ActionRefusal::Changed { what: w, .. } if w.contains(what)),
                "{refused}"
            );
            let error = ours
                .run(Some(&Decision::Allow), &Cancel::new())
                .unwrap_err();
            assert!(matches!(error, ActionRefusal::Changed { .. }), "{error}");
            assert_eq!(counts(&server).3, 0);
        }
        let closed = server();
        let mut shut = script();
        shut.open = with(shut.open, "state", json!("CLOSED"));
        shut.serve(&closed, change, MergeBehaviour::Merge);
        let error = Setting::on(&closed, change).plan().unwrap_err();
        assert!(
            error.to_string().contains("it is now closed, not open"),
            "{error}"
        );
        assert_eq!(counts(&closed).3, 0);
    }
}

/// Plans and runs `change` on a server whose read answers `open`, expecting `blocker`.
fn refused_for(open: Value, change: AutoMergeChange, blocker: ActionBlocker) {
    let server = server();
    let script = match change {
        AutoMergeChange::Enable(_) => Script::enable(),
        AutoMergeChange::Disable => Script::disable(),
    };
    Script { open, ..script }.serve(&server, change, MergeBehaviour::Merge);
    let ours = Setting::on(&server, change);
    let error = ours.plan().unwrap_err();
    assert_eq!(
        error,
        ActionRefusal::Blocked {
            action: match change {
                AutoMergeChange::Enable(_) => PullRequestAction::EnableAutoMerge,
                AutoMergeChange::Disable => PullRequestAction::DisableAutoMerge,
            },
            pull_request: "octo/rc-git#7".to_owned(),
            blocker
        }
    );
    let error = ours
        .run(Some(&Decision::Allow), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, ActionRefusal::Blocked { .. }), "{error}");
    assert_eq!(counts(&server).3, 0, "{blocker}");
}

#[test]
fn turning_it_on_sends_nothing_unless_every_precondition_holds() {
    let open = || blocked(HEAD, "OPEN");
    let repository = |key: &str, value: Value| {
        let mut answer = open();
        answer["data"]["repository"][key] = value;
        answer
    };
    let cases = [
        (
            with(open(), "mergeStateStatus", json!("CLEAN")),
            ActionBlocker::CanMergeNow,
        ),
        (
            with(open(), "mergeStateStatus", json!("UNSTABLE")),
            ActionBlocker::CanMergeNow,
        ),
        (
            with(open(), "isDraft", json!(true)),
            ActionBlocker::Merge(MergeBlocker::Draft),
        ),
        (
            with(open(), "mergeable", json!("CONFLICTING")),
            ActionBlocker::Merge(MergeBlocker::Conflicting),
        ),
        (
            with(open(), "mergeStateStatus", json!("UNKNOWN")),
            ActionBlocker::Merge(MergeBlocker::MergeStateUnknown),
        ),
        (on(open()), ActionBlocker::AutoMergeOn),
        (
            repository("autoMergeAllowed", json!(false)),
            ActionBlocker::AutoMergeNotAllowed,
        ),
        (
            repository("autoMergeAllowed", Value::Null),
            ActionBlocker::AutoMergeUnreported,
        ),
        (
            repository("squashMergeAllowed", json!(false)),
            ActionBlocker::Merge(MergeBlocker::MethodNotAllowed(MergeMethod::Squash)),
        ),
        (
            with(open(), "viewerCanEnableAutoMerge", json!(false)),
            ActionBlocker::NoEnableRight,
        ),
        (
            with(open(), "id", Value::Null),
            ActionBlocker::Merge(MergeBlocker::NoId),
        ),
    ];
    for (open, blocker) in cases {
        refused_for(open, SQUASH, blocker);
    }
}

#[test]
fn turning_it_off_sends_nothing_unless_it_is_on_and_the_account_may() {
    let open = || on(blocked(HEAD, "OPEN"));
    refused_for(
        blocked(HEAD, "OPEN"),
        AutoMergeChange::Disable,
        ActionBlocker::AutoMergeOff,
    );
    refused_for(
        with(open(), "viewerCanDisableAutoMerge", json!(false)),
        AutoMergeChange::Disable,
        ActionBlocker::NoDisableRight,
    );
    refused_for(
        with(open(), "id", Value::Null),
        AutoMergeChange::Disable,
        ActionBlocker::Merge(MergeBlocker::NoId),
    );
}

/// Plans `change` on a read that becomes `moved` after the plan's read (the same head), confirms
/// what the plan showed, and expects `refusal` and no mutation.
fn changed_after_the_plan(script: Script, change: AutoMergeChange, refusal: &str) {
    let server = server();
    script.serve(&server, change, MergeBehaviour::ChangeAfter { reads: 1 });
    let ours = Setting::on(&server, change);
    let shown = ours.plan().unwrap().decision;
    let error = ours.run(Some(&shown), &Cancel::new()).unwrap_err();
    assert!(error.to_string().contains(refusal), "{error}");
    assert_eq!(counts(&server), (0, 0, 0, 0));
}

#[test]
fn a_fact_that_changes_between_the_challenge_and_the_confirm_sends_nothing() {
    let mut failing = blocked(HEAD, "OPEN");
    failing["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
        ["contexts"]["nodes"][0]["conclusion"] = json!("FAILURE");
    changed_after_the_plan(
        Script {
            moved: failing,
            ..Script::enable()
        },
        SQUASH,
        "Checks: 0 of 1 checks passed, 1 failed",
    );
    changed_after_the_plan(
        Script {
            moved: on(blocked(HEAD, "OPEN")),
            ..Script::enable()
        },
        SQUASH,
        "auto-merge is already on",
    );
    let fork = with(
        with(blocked(HEAD, "OPEN"), "isCrossRepository", json!(true)),
        "headRepositoryOwner",
        json!({ "login": "mallory" }),
    );
    changed_after_the_plan(
        Script {
            moved: fork,
            ..Script::enable()
        },
        SQUASH,
        "Fork: its head branch is in a repository owned by \u{201c}mallory\u{201d}",
    );
    let mut by_merge_commit = on(blocked(HEAD, "OPEN"));
    by_merge_commit["data"]["repository"]["pullRequest"]["autoMergeRequest"]["mergeMethod"] =
        json!("MERGE");
    changed_after_the_plan(
        Script {
            moved: by_merge_commit,
            ..Script::disable()
        },
        AutoMergeChange::Disable,
        "Auto-merge: on, as a merge commit",
    );
}

#[test]
fn a_head_that_moves_after_the_change_s_read_is_refused_by_the_host_once() {
    let server = server();
    let refusal = json!({ "data": { "enablePullRequestAutoMerge": null }, "errors": [ {
        "type": "UNPROCESSABLE", "message": "Head sha didn't match expected head sha" } ] });
    Script::enable().serve(
        &server,
        SQUASH,
        MergeBehaviour::MoveHeadAfter {
            reads: 2,
            refusal: refusal.to_string(),
        },
    );
    let outcome = Setting::on(&server, SQUASH).confirmed().unwrap();
    assert_eq!(
        outcome,
        AutoMergeOutcome::Refused {
            reason: "Head sha didn't match expected head sha".to_owned()
        }
    );
    assert_eq!(counts(&server), (1, 0, 0, 1));
}

#[test]
fn a_refusal_passes_the_host_s_reason_through_without_the_token() {
    let token = super::merge_tests::TOKEN;
    let echoed = json!({ "data": { "enablePullRequestAutoMerge": null }, "errors": [ {
        "type": "FORBIDDEN", "message": format!("you sent {token} and may not") } ] });
    let cases = [
        ("200 OK", echoed.to_string(), "<redacted> and may not"),
        (
            "422 Unprocessable Entity",
            json!({ "message": format!("Pull request is in clean status {token}") }).to_string(),
            "HTTP 422: Pull request is in clean status <redacted>",
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
        Script::enable().serve(&server, SQUASH, MergeBehaviour::Refuse { status, body });
        let outcome = Setting::on(&server, SQUASH).confirmed().unwrap();
        let AutoMergeOutcome::Refused { reason: given } = &outcome else {
            panic!("{status}: {outcome:?}");
        };
        assert!(given.contains(reason), "{status}: {given}");
        assert!(!given.contains(token), "{status}: {given}");
        assert_eq!(counts(&server), (1, 0, 0, 1), "{status}");
        assert_eq!(
            server.graphql_reads(),
            2,
            "{status}: no read after a refusal"
        );
    }
}

#[test]
fn a_lost_answer_is_settled_by_one_read_and_never_resent() {
    let cases = [
        (SQUASH, MergeBehaviour::MergeThenDrop, Some(Settled::Reread)),
        (
            AutoMergeChange::Disable,
            MergeBehaviour::MergeThenDrop,
            Some(Settled::Reread),
        ),
        (SQUASH, MergeBehaviour::Drop, None),
        (AutoMergeChange::Disable, MergeBehaviour::Drop, None),
    ];
    for (change, behaviour, settled) in cases {
        let server = server();
        let script = match change {
            AutoMergeChange::Enable(_) => Script::enable(),
            AutoMergeChange::Disable => Script::disable(),
        };
        script.serve(&server, change, behaviour.clone());
        let outcome = Setting::on(&server, change).confirmed().unwrap();
        if let Some(settled) = settled {
            assert_eq!(
                outcome,
                AutoMergeOutcome::Done { settled },
                "{change:?} {behaviour:?}"
            );
        } else {
            let after = match change {
                AutoMergeChange::Enable(_) => AutoMergeAfter::Off,
                AutoMergeChange::Disable => AutoMergeAfter::On {
                    method: Some(MergeMethod::Squash),
                },
            };
            assert!(
                matches!(&outcome, AutoMergeOutcome::Unknown { after: a, .. } if *a == after),
                "{change:?}: {outcome:?}"
            );
        }
        assert_eq!(counts(&server).3, 1, "{change:?} {behaviour:?}");
        assert_eq!(server.graphql_reads(), 3, "one read after the loss");
    }
}

#[test]
fn a_stalled_answer_is_settled_by_one_read() {
    let server = server();
    Script::enable().serve(&server, SQUASH, MergeBehaviour::MergeThenStall);
    let ours = Setting::on(&server, SQUASH);
    let plan = ours.plan().unwrap();
    let impatient = Cancel::new().with_idle_timeout(Duration::from_secs(1));
    let outcome = ours.run(Some(&plan.decision), &impatient).unwrap();
    assert_eq!(
        outcome,
        AutoMergeOutcome::Done {
            settled: Settled::Reread
        }
    );
    assert_eq!(counts(&server), (1, 0, 0, 1));
}

#[test]
fn an_answer_that_does_not_show_the_change_is_unknown_until_a_read_says() {
    for (reply, read_after) in [
        (
            json!({ "data": { "enablePullRequestAutoMerge": { "pullRequest": {
                "number": 7, "state": "OPEN", "headRefOid": HEAD, "baseRefName": "main",
                "autoMergeRequest": null } } } }),
            AutoMergeAfter::Off,
        ),
        (
            json!({ "data": null, "errors": [ { "message": "Something went wrong" } ] }),
            AutoMergeAfter::Off,
        ),
    ] {
        let server = server();
        Script::enable().serve(
            &server,
            SQUASH,
            MergeBehaviour::Answer {
                merges: false,
                body: reply.to_string(),
                headers: Vec::new(),
            },
        );
        let outcome = Setting::on(&server, SQUASH).confirmed().unwrap();
        assert!(
            matches!(&outcome, AutoMergeOutcome::Unknown { after, .. } if *after == read_after),
            "{outcome:?}"
        );
        assert_eq!(counts(&server).3, 1);
    }
}

#[test]
fn turned_on_for_a_pull_request_retargeted_since_the_read_is_never_plain_success() {
    let server = server();
    let mut reply = enabled_reply();
    reply["data"]["enablePullRequestAutoMerge"]["pullRequest"]["baseRefName"] = json!("release");
    Script {
        reply,
        ..Script::enable()
    }
    .serve(&server, SQUASH, MergeBehaviour::Merge);
    let outcome = Setting::on(&server, SQUASH).confirmed().unwrap();
    assert_eq!(
        outcome,
        AutoMergeOutcome::OnOtherBase {
            confirmed: "main".to_owned(),
            actual: "release".to_owned(),
            settled: Settled::Answer
        }
    );
}

#[test]
fn a_repository_on_another_host_gets_nothing() {
    let account = server();
    let elsewhere = server();
    Script::enable().serve(&elsewhere, SQUASH, MergeBehaviour::Merge);
    let mut ours = Setting::on(&account, SQUASH);
    ours.target = target(&host_of(&elsewhere));
    let error = ours.plan().unwrap_err();
    assert!(
        matches!(
            &error,
            ActionRefusal::Read {
                source: GitHubError::ForeignRemote { .. },
                ..
            }
        ),
        "{error}"
    );
    let error = ours
        .run(Some(&Decision::Allow), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, ActionRefusal::Read { .. }), "{error}");
    assert_eq!(account.requests() + elsewhere.requests(), 0);
}

#[test]
fn a_cancel_up_to_the_send_sends_nothing() {
    let server = server();
    Script::enable().serve(&server, SQUASH, MergeBehaviour::Merge);
    let ours = Setting::on(&server, SQUASH);
    let plan = ours.plan().unwrap();
    let cancelled = Cancel::new();
    cancelled.signal();
    let error = ours.run(Some(&plan.decision), &cancelled).unwrap_err();
    assert!(matches!(error, ActionRefusal::Cancelled { .. }), "{error}");
    let at_send = Cancel::new();
    let raise = at_send.clone();
    let signal = move || raise.signal();
    let request = AutoMergeRequest {
        who: ours.who(),
        change: SQUASH,
        answer: Some(&plan.decision),
        on_send: &signal,
    };
    let error = set_auto_merge(&Policy::default(), request, &at_send).unwrap_err();
    assert!(matches!(error, ActionRefusal::Cancelled { .. }), "{error}");
    assert_eq!(counts(&server).3, 0);
}
