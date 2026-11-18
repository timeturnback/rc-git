//! The admin merge against the testkit's fake GitHub host: the read decides whether it exists,
//! the typed number decides whether it is confirmed, and every refusal is checked by the count
//! of mutation requests that reached the host, which must stay 0 (and 1 when it is sent, never
//! 2).

use rc_git_testkit::github::{GitHubServer, MergeBehaviour, MergeScript};
use serde_json::{Value, json};

use super::merge_tests::{
    HEAD, ID, MOVED, TOKEN, host_of, merged_reply, mutations, no_token, oid, pull, server, stamp,
    target, token,
};
use super::testing::{detail_request_body, merge_request_body};
use super::*;
use crate::actions::remote::Cancel;
use crate::{Decision, Policy};

/// [`pull`] at `head`, open, blocked by branch protection, the account allowed past it, with
/// each `(key, value)` of `changes` set on the pull request.
fn blocked_at(head: &str, changes: &[(&str, Value)]) -> String {
    let mut answer = pull(head, "OPEN");
    let node = &mut answer["data"]["repository"]["pullRequest"];
    node["mergeStateStatus"] = json!("BLOCKED");
    node["viewerCanMergeAsAdmin"] = json!(true);
    for (key, value) in changes {
        node[*key] = value.clone();
    }
    answer.to_string()
}

/// [`blocked_at`] at [`HEAD`].
fn blocked(changes: &[(&str, Value)]) -> String {
    blocked_at(HEAD, changes)
}

/// The `commits` of a pull request whose head has `nodes` of `total` checks.
fn commits(nodes: &[Value], total: usize) -> Value {
    json!({ "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE",
        "contexts": { "totalCount": total, "nodes": nodes } } } } ] })
}

/// A check run named `name`, `status` with `conclusion`, `required` as given (`Null`: unsaid).
fn run(name: &str, status: &str, conclusion: &Value, required: &Value) -> Value {
    json!({ "__typename": "CheckRun", "name": name, "status": status,
        "conclusion": conclusion, "detailsUrl": null, "databaseId": 1, "isRequired": required,
        "checkSuite": { "workflowRun": null } })
}

/// Pull request 7 on `server`: its read answers `open`, then `moved` as `behaviour` says, and
/// its merge mutation is the squash of [`HEAD`].
fn script(server: &GitHubServer, open: String, moved: String, behaviour: MergeBehaviour) {
    let target = target(&host_of(server));
    server.script_merge(MergeScript {
        read: detail_request_body(&target, 7, false),
        open,
        moved,
        merged: pull(HEAD, "MERGED").to_string(),
        mutation: merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply: merged_reply(),
        behaviour,
    });
}

/// Pull request 7 on `server`, blocked, merging as `behaviour` says.
fn script_blocked(server: &GitHubServer, behaviour: MergeBehaviour) {
    script(server, blocked(&[]), blocked_at(MOVED, &[]), behaviour);
}

/// The parts of one merge of pull request 7 with a squash.
struct Admin {
    host: GitHubHost,
    token: ApiToken,
    target: RepositoryTarget,
    stamp: PullRequestStamp,
}

impl Admin {
    fn on(server: &GitHubServer) -> Self {
        let host = host_of(server);
        Self {
            target: target(&host),
            host,
            token: token(),
            stamp: stamp(HEAD),
        }
    }

    fn target(&self, kind: MergeKind) -> MergeTarget<'_> {
        MergeTarget {
            host: &self.host,
            token: &self.token,
            target: &self.target,
            stamp: &self.stamp,
            method: MergeMethod::Squash,
            kind,
        }
    }

    fn plan(&self, kind: MergeKind) -> Result<MergePlan, MergeRefusal> {
        let result =
            plan_merge_pull_request(&Policy::default(), &self.target(kind), &Cancel::new());
        no_token(&result);
        result
    }

    /// The merge of `kind` answering `answer` with `typed`.
    fn run(
        &self,
        kind: MergeKind,
        answer: Option<&Decision>,
        typed: &str,
    ) -> Result<MergeOutcome, MergeRefusal> {
        let request = MergeRequest {
            merge: self.target(kind),
            answer: answer.map(|decision| MergeAnswer { decision, typed }),
            on_send: &|| {},
        };
        let result = merge_pull_request(&Policy::default(), request, &Cancel::new());
        no_token(&result);
        result
    }

    /// Plans the admin merge, types `7` into what the plan shows, and merges.
    fn confirmed(&self) -> Result<MergeOutcome, MergeRefusal> {
        let plan = self.plan(MergeKind::Admin).unwrap();
        self.run(MergeKind::Admin, Some(&plan.decision), "7")
    }
}

fn challenge(plan: &MergePlan) -> &crate::Challenge {
    match &plan.decision {
        Decision::Confirm(challenge) => challenge,
        Decision::Allow | Decision::Deny { .. } => panic!("{:?}", plan.decision),
    }
}

#[test]
fn an_admin_merge_sends_one_mutation_with_the_confirmed_head_and_method_to_the_account_host() {
    let server = server();
    script_blocked(&server, MergeBehaviour::Merge);
    let admin = Admin::on(&server);
    let plan = admin.plan(MergeKind::Admin).unwrap();
    assert_eq!(challenge(&plan).typed_phrase(), Some("7"));
    assert_eq!(challenge(&plan).risk(), crate::Risk::Destructive);
    assert_eq!(server.mutation_requests(), 0, "a plan only reads");
    let outcome = admin
        .run(MergeKind::Admin, Some(&plan.decision), " 7\n")
        .unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: Some(oid(super::merge_tests::MERGE_COMMIT)),
            settled: Settled::Answer
        }
    );
    assert_eq!(server.mutation_requests(), 1);
    let sent = &mutations(&server)[0];
    assert_eq!(
        String::from_utf8_lossy(&sent.body),
        merge_request_body(ID, HEAD, MergeMethod::Squash),
        "the plain merge's own document: GitHub decides from the account"
    );
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(
        body["variables"],
        json!({ "id": ID, "head": HEAD, "method": "SQUASH" })
    );
    assert_eq!(sent.target, "/api/graphql");
    assert_eq!(sent.header("host"), Some(server.host().as_str()));
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(server.graphql_reads(), 2, "the plan's read and the merge's");
    assert!(server.merged());
}

#[test]
fn a_wrong_or_missing_number_sends_nothing() {
    let server = server();
    script_blocked(&server, MergeBehaviour::Merge);
    let admin = Admin::on(&server);
    let plan = admin.plan(MergeKind::Admin).unwrap();
    for typed in [
        "", "  ", "#7", "07", "8", "17", "7 7", "seven", "\u{ff17}", "PR 7",
    ] {
        let error = admin
            .run(MergeKind::Admin, Some(&plan.decision), typed)
            .unwrap_err();
        assert_eq!(
            error,
            MergeRefusal::NotConfirmed {
                pull_request: "octo/rc-git#7".to_owned(),
                phrase: Some("7".to_owned())
            },
            "{typed:?}"
        );
        assert!(error.to_string().contains("type '7' to confirm"), "{error}");
    }
    let error = admin.run(MergeKind::Admin, None, "7").unwrap_err();
    assert!(
        matches!(error, MergeRefusal::NotConfirmed { phrase: None, .. }),
        "no answer: {error}"
    );
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn the_plain_merge_s_answer_never_merges_a_blocked_pull_request() {
    let server = server();
    let clean = pull(HEAD, "OPEN").to_string();
    script(&server, clean.clone(), clean, MergeBehaviour::Merge);
    let admin = Admin::on(&server);
    let plain_plan = admin.plan(MergeKind::Plain).unwrap();
    script_blocked(&server, MergeBehaviour::Merge);
    let error = admin.plan(MergeKind::Plain).unwrap_err();
    assert!(
        matches!(
            error,
            MergeRefusal::Blocked {
                blocker: MergeBlocker::Blocked,
                ..
            }
        ),
        "{error}"
    );
    let admin_plan = admin.plan(MergeKind::Admin).unwrap();
    let error = admin
        .run(MergeKind::Plain, Some(&admin_plan.decision), "7")
        .unwrap_err();
    assert!(
        matches!(error, MergeRefusal::Blocked { .. }),
        "the admin answer does not make the plain merge go past protection: {error}"
    );
    for typed in ["", "7"] {
        let error = admin
            .run(MergeKind::Admin, Some(&plain_plan.decision), typed)
            .unwrap_err();
        assert!(
            error.to_string().contains("no longer describes it"),
            "a plain merge's answer confirms no admin merge: {error}"
        );
    }
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn an_account_github_does_not_allow_past_protection_has_no_admin_merge() {
    for may in [json!(false), Value::Null] {
        let server = server();
        let open = blocked(&[("viewerCanMergeAsAdmin", may.clone())]);
        script(&server, open.clone(), open, MergeBehaviour::Merge);
        let admin = Admin::on(&server);
        let error = admin.plan(MergeKind::Admin).unwrap_err();
        assert!(
            matches!(
                error,
                MergeRefusal::Blocked {
                    blocker: MergeBlocker::NoAdminRight,
                    ..
                }
            ),
            "{may}: {error}"
        );
        let anything = Decision::Allow;
        let error = admin
            .run(MergeKind::Admin, Some(&anything), "7")
            .unwrap_err();
        assert!(matches!(error, MergeRefusal::Blocked { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0, "{may}");
    }
}

#[test]
fn drafts_conflicts_and_states_github_has_not_settled_have_no_admin_merge() {
    let cases = [
        (vec![("isDraft", json!(true))], MergeBlocker::Draft),
        (
            vec![("mergeable", json!("CONFLICTING"))],
            MergeBlocker::Conflicting,
        ),
        (
            vec![("mergeable", json!("UNKNOWN"))],
            MergeBlocker::MergeabilityUnknown,
        ),
        (
            vec![("mergeStateStatus", json!("DIRTY"))],
            MergeBlocker::Conflicting,
        ),
        (
            vec![("mergeStateStatus", json!("DRAFT"))],
            MergeBlocker::Draft,
        ),
        (
            vec![("mergeStateStatus", json!("UNKNOWN"))],
            MergeBlocker::MergeStateUnknown,
        ),
        (
            vec![("mergeStateStatus", Value::Null)],
            MergeBlocker::MergeStateUnknown,
        ),
        (
            vec![("mergeStateStatus", json!("SOMETHING_NEW"))],
            MergeBlocker::MergeStateUnknown,
        ),
        (vec![("id", Value::Null)], MergeBlocker::NoId),
    ];
    for (changes, blocker) in cases {
        let server = server();
        let open = blocked(&changes);
        script(&server, open.clone(), open, MergeBehaviour::Merge);
        let admin = Admin::on(&server);
        let error = admin.plan(MergeKind::Admin).unwrap_err();
        assert_eq!(
            error,
            MergeRefusal::Blocked {
                pull_request: "octo/rc-git#7".to_owned(),
                blocker
            }
        );
        let anything = Decision::Allow;
        let error = admin
            .run(MergeKind::Admin, Some(&anything), "7")
            .unwrap_err();
        assert!(matches!(error, MergeRefusal::Blocked { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0, "{blocker}");
    }
}

#[test]
fn a_host_without_the_merge_state_has_no_admin_merge() {
    let server = server();
    let target = target(&host_of(&server));
    let unknown = json!({ "errors": [ { "message": "Field 'viewerCanMergeAsAdmin' doesn't exist",
        "extensions": { "code": "undefinedField" } } ] });
    server.script_graphql(
        &detail_request_body(&target, 7, false),
        &unknown.to_string(),
    );
    server.script_graphql(&detail_request_body(&target, 7, true), &blocked(&[]));
    let error = Admin::on(&server).plan(MergeKind::Admin).unwrap_err();
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

#[test]
fn a_pull_request_the_plain_merge_can_take_has_no_admin_merge() {
    for state in ["CLEAN", "UNSTABLE", "HAS_HOOKS"] {
        let server = server();
        let open = blocked(&[("mergeStateStatus", json!(state))]);
        script(&server, open.clone(), open, MergeBehaviour::Merge);
        let admin = Admin::on(&server);
        let error = admin.plan(MergeKind::Admin).unwrap_err();
        assert_eq!(
            error,
            MergeRefusal::Blocked {
                pull_request: "octo/rc-git#7".to_owned(),
                blocker: MergeBlocker::NothingToBypass
            },
            "{state}"
        );
        assert!(error.to_string().contains("use Merge"), "{error}");
        assert_eq!(server.mutation_requests(), 0, "{state}");
    }
}

#[test]
fn a_pull_request_that_clears_or_loses_the_ability_after_the_plan_sends_nothing() {
    let cases = [
        (
            blocked(&[("mergeStateStatus", json!("CLEAN"))]),
            MergeBlocker::NothingToBypass,
        ),
        (
            blocked(&[("viewerCanMergeAsAdmin", json!(false))]),
            MergeBlocker::NoAdminRight,
        ),
    ];
    for (later, blocker) in cases {
        let server = server();
        script(
            &server,
            blocked(&[]),
            later,
            MergeBehaviour::ChangeAfter { reads: 1 },
        );
        let error = Admin::on(&server).confirmed().unwrap_err();
        assert!(
            matches!(&error, MergeRefusal::Blocked { blocker: b, .. } if *b == blocker),
            "{error}"
        );
        assert_eq!(server.mutation_requests(), 0, "{blocker}");
        assert!(!server.merged());
    }
}

#[test]
fn a_stale_head_a_changed_base_or_a_closed_pull_request_sends_nothing() {
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
        script_blocked(&server, MergeBehaviour::Merge);
        let mut admin = Admin::on(&server);
        admin.stamp = seen;
        let error = admin.plan(MergeKind::Admin).unwrap_err();
        assert!(
            matches!(&error, MergeRefusal::Changed { what: w, .. } if w.contains(what)),
            "{error}"
        );
        let anything = Decision::Allow;
        let error = admin
            .run(MergeKind::Admin, Some(&anything), "7")
            .unwrap_err();
        assert!(matches!(error, MergeRefusal::Changed { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0);
    }
    let closed = server();
    let mut answer: Value = serde_json::from_str(&blocked(&[])).unwrap();
    answer["data"]["repository"]["pullRequest"]["state"] = json!("CLOSED");
    script(
        &closed,
        answer.to_string(),
        answer.to_string(),
        MergeBehaviour::Merge,
    );
    let error = Admin::on(&closed).plan(MergeKind::Admin).unwrap_err();
    assert!(
        error.to_string().contains("it is now closed, not open"),
        "{error}"
    );
    assert_eq!(closed.mutation_requests(), 0);
}

#[test]
fn a_required_check_that_moves_between_the_challenge_and_the_confirm_sends_nothing() {
    let build = run("build", "COMPLETED", &json!("SUCCESS"), &json!(true));
    let running = run("deploy", "IN_PROGRESS", &Value::Null, &json!(true));
    let failed = run("deploy", "COMPLETED", &json!("FAILURE"), &json!(true));
    let server = server();
    script(
        &server,
        blocked(&[("commits", commits(&[build.clone(), running], 2))]),
        blocked(&[("commits", commits(&[build, failed], 2))]),
        MergeBehaviour::ChangeAfter { reads: 1 },
    );
    let admin = Admin::on(&server);
    let plan = admin.plan(MergeKind::Admin).unwrap();
    let bypassed = plan.bypassed.clone().unwrap();
    assert_eq!(bypassed.pending, ["deploy"]);
    assert!(bypassed.failed.is_empty());
    let error = admin
        .run(MergeKind::Admin, Some(&plan.decision), "7")
        .unwrap_err();
    let MergeRefusal::Changed { what, .. } = &error else {
        panic!("{error}");
    };
    assert!(
        what.contains("required checks that failed: \u{201c}deploy\u{201d}"),
        "the refusal shows what it would skip now: {what}"
    );
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn the_challenge_lists_what_the_fresh_read_shows_it_skips() {
    let long = "x".repeat(200);
    let mut nodes = vec![
        run("build", "COMPLETED", &json!("SUCCESS"), &json!(true)),
        run(
            "lint\u{202e}gnp.exe\n",
            "COMPLETED",
            &json!("FAILURE"),
            &json!(true),
        ),
        run(&long, "COMPLETED", &json!("TIMED_OUT"), &json!(true)),
    ];
    for name in ["f2", "f3", "f4", "f5", "f6"] {
        nodes.push(run(name, "COMPLETED", &json!("FAILURE"), &json!(true)));
    }
    nodes.push(run("deploy", "QUEUED", &Value::Null, &json!(true)));
    nodes.push(run(
        "optional",
        "COMPLETED",
        &json!("FAILURE"),
        &json!(false),
    ));
    nodes.push(run("mystery", "COMPLETED", &json!("FAILURE"), &Value::Null));
    nodes.push(
        json!({ "__typename": "StatusContext", "context": "ci/legacy",
        "state": "PENDING", "targetUrl": null, "description": null, "isRequired": false }),
    );
    let server = server();
    let open = blocked(&[
        ("commits", commits(&nodes, 20)),
        ("reviewDecision", json!("CHANGES_REQUESTED")),
        ("mergeStateStatus", json!("BEHIND")),
    ]);
    script(&server, open.clone(), open, MergeBehaviour::Merge);
    let plan = Admin::on(&server).plan(MergeKind::Admin).unwrap();
    let mut failed = vec!["lint\\u{202e}gnp.exe\\u{a}".to_owned(), long.clone()];
    failed.extend(["f2", "f3", "f4", "f5", "f6"].map(str::to_owned));
    assert_eq!(
        plan.bypassed,
        Some(Bypassed {
            failed,
            pending: vec!["deploy".to_owned()],
            unreported: 1,
            unread: 8,
            review: Some(ReviewDecision::ChangesRequested),
            behind: true,
        })
    );
    let summary = challenge(&plan).summary();
    let cut = format!("{}\u{2026}", "x".repeat(80));
    for part in [
        "Merge octo/rc-git#7 \u{201c}Ship \\u{202e}it\u{201d} into main past its branch \
         protection, as an administrator, with a squash merge: feature at 0123456",
        &format!(
            "Known to be skipped: required checks that failed: \
             \u{201c}lint\\u{{202e}}gnp.exe\\u{{a}}\u{201d}, \u{201c}{cut}\u{201d}, \
             \u{201c}f2\u{201d}, \u{201c}f3\u{201d}, \
             \u{201c}f4\u{201d} and 2 more; required checks not finished: \u{201c}deploy\u{201d}; \
             a review that asked for changes; bringing its head branch up to date with main."
        ),
        "1 more check(s) that have not passed may be required; GitHub did not say",
        "The read did not hold 8 of the head's checks",
        "so this list may not be complete",
    ] {
        assert!(summary.contains(part), "{part}\n---\n{summary}");
    }
    for hidden in ['\u{202e}', '\n', '\t'] {
        assert!(!summary.contains(hidden), "{hidden:?} in {summary}");
    }
    assert!(!summary.contains("optional") && !summary.contains("ci/legacy"));
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn a_refused_admin_merge_passes_the_reason_through_without_the_token() {
    let echoed = json!({ "data": { "mergePullRequest": null }, "errors": [ {
        "type": "FORBIDDEN",
        "message": format!("{TOKEN} may not bypass branch protections") } ] });
    let server = server();
    script_blocked(
        &server,
        MergeBehaviour::Refuse {
            status: "200 OK",
            body: echoed.to_string(),
        },
    );
    let outcome = Admin::on(&server).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Refused {
            reason: "<redacted> may not bypass branch protections".to_owned()
        }
    );
    assert_eq!(server.mutation_requests(), 1);
    assert_eq!(server.graphql_reads(), 2, "a refusal needs no read after");
    assert!(!server.merged());
}

#[test]
fn a_lost_answer_is_settled_by_one_read_and_never_resent() {
    let applied = server();
    script_blocked(&applied, MergeBehaviour::MergeThenDrop);
    let outcome = Admin::on(&applied).confirmed().unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            merge_commit: None,
            settled: Settled::Reread
        }
    );
    assert_eq!(applied.mutation_requests(), 1);
    assert_eq!(
        applied.graphql_reads(),
        3,
        "the plan, the merge, one read after"
    );

    let dropped = server();
    script_blocked(&dropped, MergeBehaviour::Drop);
    let outcome = Admin::on(&dropped).confirmed().unwrap();
    let MergeOutcome::Unknown { problem, after } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(after, &AfterLoss::StillOpen { head: oid(HEAD) });
    assert!(
        problem.contains("merge the pull request past branch protection"),
        "{problem}"
    );
    assert_eq!(dropped.mutation_requests(), 1);
    assert!(!dropped.merged());
}
