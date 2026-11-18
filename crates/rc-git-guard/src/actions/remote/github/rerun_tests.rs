//! Re-running failed jobs against the testkit's fake GitHub host, which answers only the exact
//! read and the exact re-run request a correct client sends, per workflow run, and counts them.

use rc_git_testkit::github::{
    GitHubServer, MergeBehaviour, MergeScript, RERUN_BODY, RerunBehaviour,
};
use serde_json::{Value, json};

use super::merge_tests::{
    HEAD, MOVED, TOKEN, host_of, no_token, pull, server, stamp, target, token,
};
use super::testing::detail_request_body;
use super::*;
use crate::actions::remote::Cancel;
use crate::{Action, Decision, Policy};

const REPOSITORY: &str = "octo/rc-git";

/// A GitHub Actions job of workflow run `run` (any JSON) named `name` that ended as
/// `conclusion`.
fn job(name: &str, run: &Value, conclusion: &str) -> Value {
    json!({ "__typename": "CheckRun", "name": name, "status": "COMPLETED",
        "conclusion": conclusion, "detailsUrl": null, "databaseId": 9000, "isRequired": false,
        "checkSuite": { "workflowRun": { "databaseId": run } } })
}

/// [`pull`] at `head`, open, with `checks` as its head's checks.
fn with_checks(head: &str, checks: &[Value]) -> Value {
    let mut answer = pull(head, "OPEN");
    answer["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
        ["contexts"] = json!({ "totalCount": checks.len(), "nodes": checks });
    answer
}

/// Two failed jobs of run 42, one of run 41, a passing job of run 43, a cancelled job of run 44,
/// a failed check of another app and a failed commit status.
fn standard_checks() -> Vec<Value> {
    vec![
        job("test", &json!(42), "FAILURE"),
        job("lint", &json!(41), "FAILURE"),
        job("docs", &json!(43), "SUCCESS"),
        job("build", &json!(42), "TIMED_OUT"),
        job("deploy", &json!(44), "CANCELLED"),
        json!({ "__typename": "CheckRun", "name": "codecov", "status": "COMPLETED",
            "conclusion": "FAILURE", "detailsUrl": null, "databaseId": 77, "isRequired": false,
            "checkSuite": { "workflowRun": null } }),
        json!({ "__typename": "StatusContext", "context": "ci/legacy", "state": "FAILURE",
            "targetUrl": null, "description": null, "isRequired": false }),
    ]
}

/// The read of pull request 7 on `server`: `open`, then `moved` once `behaviour` says. No
/// mutation is scripted on it.
fn script_read(server: &GitHubServer, open: &Value, moved: &Value, behaviour: MergeBehaviour) {
    let target = target(&host_of(server));
    server.script_merge(MergeScript {
        read: detail_request_body(&target, 7, false),
        open: open.to_string(),
        moved: moved.to_string(),
        merged: open.to_string(),
        mutation: "no mutation is scripted".to_owned(),
        reply: String::new(),
        behaviour,
    });
}

/// The standard pull request on `server`, with runs 41 and 42 re-run as asked.
fn standard(server: &GitHubServer) {
    let open = with_checks(HEAD, &standard_checks());
    script_read(server, &open, &open, MergeBehaviour::Merge);
    for run in [41, 42] {
        server.script_rerun(REPOSITORY, run, RerunBehaviour::Rerun);
    }
}

/// The parts of one re-run.
struct Rerun {
    host: GitHubHost,
    token: ApiToken,
    target: RepositoryTarget,
    stamp: PullRequestStamp,
}

impl Rerun {
    fn on(server: &GitHubServer) -> Self {
        let host = host_of(server);
        Self {
            target: target(&host),
            host,
            token: token(),
            stamp: stamp(HEAD),
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
        plan_rerun_failed_jobs(&Policy::default(), &self.who(), &Cancel::new())
    }

    fn run(
        &self,
        answer: Option<&Decision>,
        cancel: &Cancel,
    ) -> Result<RerunReport, ActionRefusal> {
        let request = RerunRequest {
            who: self.who(),
            answer,
            on_send: &|| {},
        };
        let result = rerun_failed_jobs(&Policy::default(), request, cancel);
        no_token(&result);
        result
    }

    fn confirmed(&self) -> Result<RerunReport, ActionRefusal> {
        let plan = self.plan().unwrap();
        self.run(Some(&plan.decision), &Cancel::new())
    }
}

fn run(id: u64) -> RunId {
    RunId::new(id).unwrap()
}

/// The re-run paths the server was sent, in arrival order.
fn rerun_targets(server: &GitHubServer) -> Vec<String> {
    server
        .recorded()
        .into_iter()
        .filter(|request| request.target.ends_with("/rerun-failed-jobs"))
        .map(|request| request.target)
        .collect()
}

fn path(id: u64) -> String {
    format!("/api/v3/repos/{REPOSITORY}/actions/runs/{id}/rerun-failed-jobs")
}

#[test]
fn a_rerun_sends_one_request_per_failed_run_in_id_order() {
    let server = server();
    standard(&server);
    let report = Rerun::on(&server).confirmed().unwrap();
    assert_eq!(
        report.runs,
        [
            RunReport {
                run: run(41),
                outcome: RunOutcome::Requested
            },
            RunReport {
                run: run(42),
                outcome: RunOutcome::Requested
            }
        ]
    );
    assert_eq!(rerun_targets(&server), [path(41), path(42)]);
    for request in server
        .recorded()
        .iter()
        .filter(|request| request.target.ends_with("/rerun-failed-jobs"))
    {
        assert_eq!(request.method, "POST");
        assert_eq!(request.body, RERUN_BODY.as_bytes());
        assert_eq!(request.body, testing::rerun_request_body().as_bytes());
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(request.header("content-type"), Some("application/json"));
    }
    assert_eq!(
        (server.reruns_applied(41), server.reruns_applied(42)),
        (1, 1)
    );
    assert_eq!(server.mutation_requests(), 2);
    assert_eq!(server.mutations_named("RcGitMergePullRequest"), 0);
    for never in [43, 44, 77, 9000] {
        assert_eq!(server.rerun_requests(REPOSITORY, never), 0, "{never}");
    }
}

#[test]
fn the_challenge_is_built_from_the_fresh_read_and_counts_what_it_leaves_out() {
    let server = server();
    standard(&server);
    let plan = Rerun::on(&server).plan().unwrap();
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("{:?}", plan.decision);
    };
    let host = host_of(&server);
    assert_eq!(
        challenge.summary(),
        format!(
            "Re-run the failed jobs of these GitHub Actions runs. GitHub re-runs each run's \
             failed jobs, as it counts them, and the jobs that depend on them, at the commit and \
             ref of the original run and with the privileges of whoever first triggered it; \
             rc-git cannot stop them once they start.\n\
             Repository: octo/rc-git on {host}\n\
             Pull request: #7\n\
             Account: octocat@{host}\n\
             Base: \u{201c}main\u{201d}\n\
             Head: 0123456 on \u{201c}feature\u{201d}\n\
             Runs: 2 runs: run 41 (\u{201c}lint\u{201d}); run 42 (\u{201c}build\u{201d}, \
             \u{201c}test\u{201d})\n\
             Not included: 3 other failed check(s) that are not a failed job of a GitHub \
             Actions run rc-git can name\n\
             Title: \u{201c}Ship \\u{{202e}}it\u{201d}"
        )
    );
    assert_eq!(server.mutation_requests(), 0, "a plan only reads");
}

#[test]
fn no_answer_or_an_answer_to_another_challenge_sends_nothing() {
    let server = server();
    standard(&server);
    let rerun = Rerun::on(&server);
    let error = rerun.run(None, &Cancel::new()).unwrap_err();
    assert!(
        matches!(error, ActionRefusal::NotConfirmed { .. }),
        "{error}"
    );
    let detail =
        read_pull_request(&rerun.host, &rerun.token, &rerun.target, 7, &Cancel::new()).unwrap();
    let merge =
        PullRequestMerge::new(&rerun.target, "octocat", &detail, MergeMethod::Squash).unwrap();
    let mut blocked = detail.clone();
    blocked.merge_state = Some(MergeStateStatus::Blocked);
    blocked.auto_merge_allowed = Some(true);
    blocked.viewer = Some(ViewerAbilities {
        can_enable_auto_merge: true,
        ..ViewerAbilities::default()
    });
    let enable =
        AutoMergeEnable::new(&rerun.target, "octocat", &blocked, MergeMethod::Squash).unwrap();
    let mut fewer = detail.clone();
    fewer.checks.checks.retain(|check| check.name != "lint");
    let other_runs = PullRequestRerun::new(&rerun.target, "octocat", &fewer).unwrap();
    for other in [
        Action::DeleteTag {
            name: "v1".to_owned(),
        },
        Action::MergePullRequest(Box::new(merge)),
        Action::EnableAutoMerge(Box::new(enable)),
        Action::RerunFailedJobs(Box::new(other_runs)),
    ] {
        let decision = Policy::default().evaluate(&other);
        let error = rerun.run(Some(&decision), &Cancel::new()).unwrap_err();
        assert!(
            matches!(&error, ActionRefusal::Changed { what, .. } if what.contains("no longer describes it")),
            "{error}"
        );
    }
    assert_eq!(server.mutation_requests(), 0);
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
        standard(&server);
        let mut rerun = Rerun::on(&server);
        rerun.stamp = seen;
        let refused = rerun.plan().unwrap_err();
        assert!(
            matches!(&refused, ActionRefusal::Changed { what: w, .. } if w.contains(what)),
            "{refused}"
        );
        let error = rerun
            .run(Some(&Decision::Allow), &Cancel::new())
            .unwrap_err();
        assert!(matches!(error, ActionRefusal::Changed { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0);
    }
    let server = server();
    let mut closed = with_checks(HEAD, &standard_checks());
    closed["data"]["repository"]["pullRequest"]["state"] = json!("CLOSED");
    script_read(&server, &closed, &closed, MergeBehaviour::Merge);
    let mut rerun = Rerun::on(&server);
    rerun.stamp.state = PullRequestState::Closed;
    let error = rerun.plan().unwrap_err();
    assert!(error.to_string().contains("it is closed"), "{error}");
    let error = rerun
        .run(Some(&Decision::Allow), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, ActionRefusal::Blocked { .. }), "{error}");
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn a_head_without_a_failed_run_rc_git_can_name_sends_nothing() {
    for checks in [
        vec![job("docs", &json!(43), "SUCCESS")],
        vec![
            job("deploy", &json!(44), "CANCELLED"),
            job("wait", &json!(45), "ACTION_REQUIRED"),
            job("skip", &json!(46), "SKIPPED"),
        ],
        vec![
            job("zero", &json!(0), "FAILURE"),
            job("negative", &json!(-5), "FAILURE"),
            job("fraction", &json!(1.5), "FAILURE"),
            job("text", &json!("77"), "FAILURE"),
            job("huge", &json!(9_223_372_036_854_775_808_u64), "FAILURE"),
            job("null", &Value::Null, "FAILURE"),
        ],
    ] {
        let server = server();
        let open = with_checks(HEAD, &checks);
        script_read(&server, &open, &open, MergeBehaviour::Merge);
        for id in [0, 5, 43, 44, 45, 46, 77] {
            server.script_rerun(REPOSITORY, id, RerunBehaviour::Rerun);
        }
        let rerun = Rerun::on(&server);
        let error = rerun.plan().unwrap_err();
        assert!(
            matches!(
                error,
                ActionRefusal::Blocked {
                    blocker: ActionBlocker::NoFailedRuns,
                    ..
                }
            ),
            "{error}"
        );
        let error = rerun
            .run(Some(&Decision::Allow), &Cancel::new())
            .unwrap_err();
        assert!(matches!(error, ActionRefusal::Blocked { .. }), "{error}");
        assert_eq!(server.mutation_requests(), 0);
        assert!(rerun_targets(&server).is_empty());
    }
}

#[test]
fn a_run_id_rc_git_does_not_accept_never_becomes_a_request() {
    let server = server();
    let checks = [
        job("zero", &json!(0), "FAILURE"),
        job("negative", &json!(-5), "FAILURE"),
        job("huge", &json!(9_223_372_036_854_775_808_u64), "FAILURE"),
        job("real", &json!(9_223_372_036_854_775_807_u64), "FAILURE"),
    ];
    let open = with_checks(HEAD, &checks);
    script_read(&server, &open, &open, MergeBehaviour::Merge);
    server.script_rerun(REPOSITORY, 9_223_372_036_854_775_807, RerunBehaviour::Rerun);
    let report = Rerun::on(&server).confirmed().unwrap();
    assert_eq!(report.runs.len(), 1);
    assert_eq!(report.runs[0].run.get(), 9_223_372_036_854_775_807);
    assert_eq!(rerun_targets(&server), [path(9_223_372_036_854_775_807)]);
    assert_eq!(RunId::new(0), None);
    assert_eq!(RunId::new(1 << 63), None);
}

#[test]
fn a_check_that_moved_since_the_challenge_sends_nothing() {
    let cases = [
        // A failed job passed on its own.
        standard_checks()
            .into_iter()
            .filter(|check| check["name"] != "lint")
            .collect::<Vec<_>>(),
        // Another job failed.
        [standard_checks(), vec![job("e2e", &json!(42), "FAILURE")]].concat(),
    ];
    for moved in cases {
        let server = server();
        let open = with_checks(HEAD, &standard_checks());
        script_read(
            &server,
            &open,
            &with_checks(HEAD, &moved),
            MergeBehaviour::ChangeAfter { reads: 1 },
        );
        let rerun = Rerun::on(&server);
        let shown = rerun.plan().unwrap().decision;
        let error = rerun.run(Some(&shown), &Cancel::new()).unwrap_err();
        assert!(
            matches!(&error, ActionRefusal::Changed { what, .. } if what.contains("Runs: ")),
            "{error}"
        );
        assert_eq!(server.mutation_requests(), 0);
    }
}

#[test]
fn a_fork_s_challenge_says_github_decides_how_its_runs_run() {
    let server = server();
    let mut open = with_checks(HEAD, &standard_checks());
    open["data"]["repository"]["pullRequest"]["isCrossRepository"] = json!(true);
    open["data"]["repository"]["pullRequest"]["headRepositoryOwner"] =
        json!({ "login": "mallory" });
    script_read(&server, &open, &open, MergeBehaviour::Merge);
    let plan = Rerun::on(&server).plan().unwrap();
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("{:?}", plan.decision);
    };
    assert!(
        challenge.summary().contains(
            "\nFork: its head branch is in a repository owned by \u{201c}mallory\u{201d}. GitHub \
             decides which workflow files, secrets and permissions these runs use; rc-git \
             approves no run that waits for approval\n"
        ),
        "{}",
        challenge.summary()
    );
}

#[test]
fn one_confirmation_re_runs_at_most_ten_runs_and_says_how_many_it_leaves_out() {
    let server = server();
    let mut checks = Vec::new();
    for id in (101..=112).rev() {
        checks.push(job(&format!("job {id}"), &json!(id), "FAILURE"));
    }
    let open = with_checks(HEAD, &checks);
    script_read(&server, &open, &open, MergeBehaviour::Merge);
    for id in 101..=112 {
        server.script_rerun(REPOSITORY, id, RerunBehaviour::Rerun);
    }
    let rerun = Rerun::on(&server);
    let plan = rerun.plan().unwrap();
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("{:?}", plan.decision);
    };
    assert!(
        challenge.summary().contains("\nRuns: 10 runs: run 101 (") && challenge.summary().contains(
            "\nNot included: 2 more run(s) with failed jobs, past the 10 one confirmation re-runs \
             (the lowest ids go first)\n"
        ),
        "{}",
        challenge.summary()
    );
    let report = rerun.run(Some(&plan.decision), &Cancel::new()).unwrap();
    let sent: Vec<String> = (101..=110).map(path).collect();
    assert_eq!(rerun_targets(&server), sent);
    assert_eq!(report.runs.len(), 10);
    assert_eq!(server.rerun_requests(REPOSITORY, 111), 0);
    assert_eq!(server.rerun_requests(REPOSITORY, 112), 0);
}

#[test]
fn hostile_check_names_are_neutralized_and_capped_in_the_challenge() {
    let server = server();
    let long = "x".repeat(200);
    let mut checks = vec![job(
        "evil\u{202e}\nRuns: 0 runs \u{201d}",
        &json!(41),
        "FAILURE",
    )];
    for index in 0..6 {
        checks.push(job(&format!("{long}{index}"), &json!(41), "FAILURE"));
    }
    let open = with_checks(HEAD, &checks);
    script_read(&server, &open, &open, MergeBehaviour::Merge);
    let plan = Rerun::on(&server).plan().unwrap();
    let Decision::Confirm(challenge) = &plan.decision else {
        panic!("{:?}", plan.decision);
    };
    let summary = challenge.summary();
    assert_eq!(
        summary
            .lines()
            .filter(|line| line.starts_with("Runs: "))
            .count(),
        1,
        "{summary}"
    );
    let runs = summary
        .lines()
        .find(|line| line.starts_with("Runs: "))
        .unwrap();
    assert!(
        runs.contains("\u{201c}evil\\u{202e}\\u{a}Runs: 0 runs \\u{201d}\u{201d}"),
        "{runs}"
    );
    assert!(runs.ends_with(" and 2 more)"), "{runs}");
    assert!(!runs.contains(&"x".repeat(81)), "{runs}");
    assert!(!summary.contains('\u{202e}'));
}

#[test]
fn a_refusal_of_one_run_is_shown_and_the_next_run_is_still_sent_once() {
    let server = server();
    let open = with_checks(HEAD, &standard_checks());
    script_read(&server, &open, &open, MergeBehaviour::Merge);
    server.script_rerun(
        REPOSITORY,
        41,
        RerunBehaviour::Answer {
            status: "403 Forbidden",
            body: json!({ "message": format!("Must have admin rights {TOKEN}") }).to_string(),
            headers: Vec::new(),
        },
    );
    server.script_rerun(REPOSITORY, 42, RerunBehaviour::Rerun);
    let report = Rerun::on(&server).confirmed().unwrap();
    let RunOutcome::Refused { reason } = &report.runs[0].outcome else {
        panic!("{report:?}");
    };
    assert!(
        reason.contains("HTTP 403: Must have admin rights <redacted>"),
        "{reason}"
    );
    assert!(!reason.contains(TOKEN));
    assert_eq!(report.runs[1].outcome, RunOutcome::Requested);
    assert_eq!(rerun_targets(&server), [path(41), path(42)]);
    assert_eq!(server.reruns_applied(41), 0);
}

#[test]
fn a_run_github_does_not_know_is_refused_as_not_found() {
    let server = server();
    let open = with_checks(HEAD, &standard_checks());
    script_read(&server, &open, &open, MergeBehaviour::Merge);
    server.script_rerun(REPOSITORY, 42, RerunBehaviour::Rerun);
    let report = Rerun::on(&server).confirmed().unwrap();
    assert!(
        matches!(&report.runs[0].outcome, RunOutcome::Refused { reason } if reason.contains("HTTP 404")),
        "{report:?}"
    );
    assert_eq!(report.runs[1].outcome, RunOutcome::Requested);
}

#[test]
fn a_refused_token_or_a_spent_rate_limit_stops_the_runs_after_it() {
    let spent = vec![
        ("x-ratelimit-remaining".to_owned(), "0".to_owned()),
        ("x-ratelimit-reset".to_owned(), "1900000000".to_owned()),
    ];
    for (status, headers, words) in [
        ("401 Unauthorized", Vec::new(), "rejected the token"),
        ("403 Forbidden", spent, "rate limited by the host"),
    ] {
        let server = server();
        let open = with_checks(HEAD, &standard_checks());
        script_read(&server, &open, &open, MergeBehaviour::Merge);
        server.script_rerun(
            REPOSITORY,
            41,
            RerunBehaviour::Answer {
                status,
                body: json!({ "message": "no" }).to_string(),
                headers,
            },
        );
        server.script_rerun(REPOSITORY, 42, RerunBehaviour::Rerun);
        let report = Rerun::on(&server).confirmed().unwrap();
        assert!(
            matches!(&report.runs[0].outcome, RunOutcome::Refused { reason } if reason.contains(words)),
            "{status}: {report:?}"
        );
        assert!(
            matches!(&report.runs[1].outcome, RunOutcome::NotSent { .. }),
            "{status}: {report:?}"
        );
        assert_eq!(rerun_targets(&server), [path(41)], "{status}");
    }
}

#[test]
fn a_lost_or_unexpected_answer_is_unknown_never_resent_and_stops_the_rest() {
    for (behaviour, applied) in [
        (RerunBehaviour::Drop, 0),
        (RerunBehaviour::RerunThenDrop, 1),
        (
            RerunBehaviour::Answer {
                status: "502 Bad Gateway",
                body: json!({ "message": "Server Error" }).to_string(),
                headers: Vec::new(),
            },
            0,
        ),
        (
            RerunBehaviour::Answer {
                status: "200 OK",
                body: "{}".to_owned(),
                headers: Vec::new(),
            },
            0,
        ),
    ] {
        let server = server();
        let open = with_checks(HEAD, &standard_checks());
        script_read(&server, &open, &open, MergeBehaviour::Merge);
        server.script_rerun(REPOSITORY, 41, behaviour.clone());
        server.script_rerun(REPOSITORY, 42, RerunBehaviour::Rerun);
        let report = Rerun::on(&server).confirmed().unwrap();
        assert!(
            matches!(&report.runs[0].outcome, RunOutcome::Unknown { .. }),
            "{behaviour:?}: {report:?}"
        );
        assert!(
            matches!(&report.runs[1].outcome, RunOutcome::NotSent { .. }),
            "{behaviour:?}: {report:?}"
        );
        assert_eq!(rerun_targets(&server), [path(41)], "{behaviour:?}");
        assert_eq!(server.reruns_applied(41), applied, "{behaviour:?}");
        assert_eq!(
            server.graphql_reads(),
            2,
            "{behaviour:?}: no read settles a re-run"
        );
    }
}

#[test]
fn a_repository_on_another_host_gets_nothing() {
    let account = server();
    let elsewhere = server();
    standard(&elsewhere);
    let mut rerun = Rerun::on(&account);
    rerun.target = target(&host_of(&elsewhere));
    let error = rerun.plan().unwrap_err();
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
    let error = rerun
        .run(Some(&Decision::Allow), &Cancel::new())
        .unwrap_err();
    assert!(matches!(error, ActionRefusal::Read { .. }), "{error}");
    assert_eq!(account.requests() + elsewhere.requests(), 0);
}

#[test]
fn a_cancel_up_to_the_first_send_sends_nothing() {
    let server = server();
    standard(&server);
    let rerun = Rerun::on(&server);
    let plan = rerun.plan().unwrap();
    let cancelled = Cancel::new();
    cancelled.signal();
    let error = rerun.run(Some(&plan.decision), &cancelled).unwrap_err();
    assert!(matches!(error, ActionRefusal::Cancelled { .. }), "{error}");
    let at_send = Cancel::new();
    let raise = at_send.clone();
    let signal = move || raise.signal();
    let request = RerunRequest {
        who: rerun.who(),
        answer: Some(&plan.decision),
        on_send: &signal,
    };
    let error = rerun_failed_jobs(&Policy::default(), request, &at_send).unwrap_err();
    assert!(matches!(error, ActionRefusal::Cancelled { .. }), "{error}");
    assert_eq!(server.mutation_requests(), 0);
}
