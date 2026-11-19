//! Deleting a merged pull request's head branch against the testkit's fake GitHub host, which
//! answers only the exact reads and the exact delete a correct client sends and counts the
//! deletes, in a fixture repository whose backup refs each test reads. Every test that sends
//! nothing asserts both: no delete request arrived, and no backup ref was written.

use std::sync::{Arc, Mutex};

use rc_git_testkit::Fixture;
use rc_git_testkit::github::{BranchScript, DeleteBehaviour, GitHubServer};
use serde_json::{Value, json};

use super::api::Session;
use super::merge_tests::{TOKEN, host_of, no_token, server, target, token};
use super::testing::{based_on_request_body, delete_branch_request_body, head_branch_request_body};
use super::*;
use crate::actions::remote::Cancel;
use crate::{Action, Decision, Policy};

const REPO_ID: &str = "R_kgDOrepo7";
const PUSHED: &str = "89abcdef0123456789abcdef0123456789abcdef";
const BACKUP: &str = "refs/rc-git/backup/remote-branch/shipped";

/// A repository whose `HEAD` commit is the head the pull request merged.
fn repository() -> (Fixture, String) {
    let fx = Fixture::standard();
    let head = fx.rev_parse("HEAD");
    (fx, head)
}

/// Every backup ref of `fx` with its commit, one `name oid` line each.
fn backups(fx: &Fixture) -> String {
    fx.git_output(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        "refs/rc-git/backup/",
    ])
}

/// The head branch read's answer: pull request 7, merged at `head`, its head branch `name` at
/// `at` (gone when `None`), the default branch `trunk`, no other open pull request.
fn read_answer(name: &str, head: &str, at: Option<&str>) -> Value {
    let head_ref = at.map(|at| {
        json!({ "name": name, "prefix": "refs/heads/", "target": { "oid": at },
            "repository": { "name": "rc-git", "owner": { "login": "octo" } },
            "associatedPullRequests": { "totalCount": 0, "nodes": [] } })
    });
    json!({ "data": { "repository": {
        "id": REPO_ID,
        "defaultBranchRef": { "name": "trunk" },
        "pullRequest": {
            "number": 7, "title": "Ship \u{202e}it", "state": "MERGED", "merged": true,
            "headRefName": name, "headRefOid": head, "baseRefName": "main",
            "isCrossRepository": false, "viewerCanDeleteHeadRef": true,
            "headRepository": { "name": "rc-git", "owner": { "login": "octo" } },
            "headRef": head_ref } } } })
}

/// `answer` with the value at `path` (keys under `data.repository`) set to `value`.
fn with(mut answer: Value, path: &[&str], value: Value) -> Value {
    let mut node = &mut answer["data"]["repository"];
    for key in path {
        node = &mut node[*key];
    }
    *node = value;
    answer
}

/// No open pull request based on the branch.
fn none_based_on() -> Value {
    json!({ "data": { "repository": { "pullRequests": { "totalCount": 0, "nodes": [] } } } })
}

/// The head branch `name` of pull request 7 on `server`, read as `present` (and `changed` once
/// `behaviour` says), deleted as `behaviour` says.
fn script(
    server: &GitHubServer,
    name: &str,
    (present, changed): (&Value, &Value),
    behaviour: DeleteBehaviour,
) {
    script_based_on(
        server,
        name,
        (present, changed),
        &none_based_on(),
        behaviour,
    );
}

fn script_based_on(
    server: &GitHubServer,
    name: &str,
    (present, changed): (&Value, &Value),
    based_on: &Value,
    behaviour: DeleteBehaviour,
) {
    let target = target(&host_of(server));
    let head = present["data"]["repository"]["pullRequest"]["headRefOid"]
        .as_str()
        .unwrap()
        .to_owned();
    server.script_branch(BranchScript {
        read: head_branch_request_body(&target, 7),
        present: present.to_string(),
        present_at: branch_at(present),
        changed: changed.to_string(),
        changed_at: branch_at(changed),
        deleted: read_answer(name, &head, None).to_string(),
        based_on: based_on_request_body(&target, name),
        based_on_answer: based_on.to_string(),
        mutation: delete_branch_request_body(REPO_ID, name, &head),
        behaviour,
    });
}

/// The commit the head branch `answer` reads is at; empty when it names none.
fn branch_at(answer: &Value) -> String {
    answer["data"]["repository"]["pullRequest"]["headRef"]["target"]["oid"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The standard branch `shipped` at `head`, deleted as `behaviour` says.
fn standard(server: &GitHubServer, head: &str, behaviour: DeleteBehaviour) {
    let present = read_answer("shipped", head, Some(head));
    script(server, "shipped", (&present, &present), behaviour);
}

/// The parts of one delete.
struct Delete {
    host: GitHubHost,
    token: ApiToken,
    target: RepositoryTarget,
    stamp: PullRequestStamp,
    fx: Fixture,
}

impl Delete {
    fn on(server: &GitHubServer, fx: Fixture, head: &str) -> Self {
        let host = host_of(server);
        Self {
            target: target(&host),
            host,
            token: token(),
            stamp: stamp(head, PullRequestState::Merged),
            fx,
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

    fn plan(&self) -> Result<BranchDeletePlan, ActionRefusal> {
        let plan = plan_delete_head_branch(
            &Policy::default(),
            &self.who(),
            self.fx.path(),
            &Cancel::new(),
        );
        no_token(&plan);
        plan
    }

    fn run_with(
        &self,
        (answer, typed): (Option<&Decision>, &str),
        on_send: &(dyn Fn() + Sync),
        cancel: &Cancel,
    ) -> Result<BranchDeleteReport, ActionRefusal> {
        let request = BranchDeleteRequest {
            who: self.who(),
            repo_path: self.fx.path(),
            answer,
            typed,
            on_send,
        };
        let result = delete_head_branch(&Policy::default(), request, cancel);
        no_token(&result);
        result
    }

    fn run(
        &self,
        answer: Option<&Decision>,
        typed: &str,
    ) -> Result<BranchDeleteReport, ActionRefusal> {
        self.run_with((answer, typed), &|| {}, &Cancel::new())
    }

    /// Plans, then confirms what the plan shows by typing its branch.
    fn confirmed(&self) -> Result<BranchDeleteReport, ActionRefusal> {
        let plan = self.plan().unwrap();
        self.run(Some(&plan.decision), &plan.branch)
    }
}

/// What the user saw: pull request 7, `state` at `head` on `main`.
fn stamp(head: &str, state: PullRequestState) -> PullRequestStamp {
    PullRequestStamp {
        number: 7,
        head_oid: rc_git_core::Oid::from_hex(head).unwrap(),
        base_ref: "main".to_owned(),
        state,
    }
}

/// Asserts that nothing was sent and nothing written: no delete request, no backup ref.
fn nothing_done(server: &GitHubServer, delete: &Delete, what: &str) {
    assert_eq!(server.delete_requests(), 0, "{what}: a delete was sent");
    assert_eq!(server.mutation_requests(), 0, "{what}: a mutation was sent");
    assert_eq!(backups(&delete.fx), "", "{what}: a backup ref was written");
    assert!(!server.branch_deleted(), "{what}");
}

fn blocker(refusal: &ActionRefusal) -> Option<ActionBlocker> {
    match refusal {
        ActionRefusal::BranchBlocked { blocker, .. } => Some(*blocker),
        _ => None,
    }
}

#[test]
fn a_delete_writes_the_backup_before_its_one_request_naming_the_ref() {
    let server = server();
    let (fx, head) = repository();
    standard(&server, &head, DeleteBehaviour::Delete);
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let path = fx.path().to_owned();
    let witness = Arc::clone(&seen);
    server.on_delete(move || {
        let repo = gix::open_opts(&path, gix::open::Options::isolated()).unwrap();
        let backup = repo
            .try_find_reference(BACKUP)
            .unwrap()
            .map_or_else(|| "none".to_owned(), |found| found.id().to_string());
        witness.lock().unwrap().push(backup);
    });
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    assert_eq!(plan.branch, "shipped");
    assert_eq!(plan.backup_ref, BACKUP);
    assert_eq!(backups(&delete.fx), "", "planning writes nothing");
    let report = delete.run(Some(&plan.decision), "shipped").unwrap();
    assert_eq!(
        report.outcome,
        BranchDeleteOutcome::Deleted {
            settled: Settled::Answer
        }
    );
    assert_eq!(report.backup_ref, BACKUP);
    assert_eq!(report.head_oid.to_hex(), head);
    assert!(!report.local_branch);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        std::slice::from_ref(&head),
        "the backup held the head when the one delete arrived"
    );
    assert_eq!(server.delete_requests(), 1);
    assert_eq!(server.mutation_requests(), 1);
    assert_eq!(server.mutations_named("RcGitDeleteBranch"), 1);
    assert_eq!(backups(&delete.fx), format!("{BACKUP} {head}"));
    let sent: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains("mutation"))
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].body,
        delete_branch_request_body(REPO_ID, "shipped", &head).as_bytes()
    );
    assert_eq!(sent[0].target, "/api/graphql");
    assert_eq!(
        sent[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    let body = String::from_utf8_lossy(&sent[0].body);
    assert!(
        body.contains(&format!(r#""before":"{head}""#))
            && body.contains(r#""name":"refs/heads/shipped""#)
            && body.contains(&format!(r#""repository":"{REPO_ID}""#)),
        "the branch's full name and the commit it must still be at travel in variables: {body}"
    );
}

#[test]
fn a_local_branch_of_the_same_name_is_named_and_left_alone() {
    let server = server();
    let (fx, head) = repository();
    fx.git(&["branch", "shipped"]);
    standard(&server, &head, DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    let report = delete.confirmed().unwrap();
    assert!(report.local_branch);
    assert_eq!(delete.fx.rev_parse("refs/heads/shipped"), head);
}

#[test]
fn protected_names_are_denied_with_no_request_and_no_backup() {
    for name in ["main", "master", "release/1.0", "Release/2"] {
        let server = server();
        let (fx, head) = repository();
        let present = read_answer(name, &head, Some(&head));
        script(&server, name, (&present, &present), DeleteBehaviour::Delete);
        let delete = Delete::on(&server, fx, &head);
        let refused = delete.plan().unwrap_err();
        assert!(
            matches!(&refused, ActionRefusal::Denied { reason } if reason.contains(name)),
            "{name}: {refused}"
        );
        let deny = Decision::Deny {
            reason: refused.to_string(),
        };
        let remote = Policy::default().evaluate(&Action::DeleteRemoteBranch {
            name: name.to_owned(),
        });
        for answer in [&deny, &remote] {
            let refused = delete.run(Some(answer), name).unwrap_err();
            assert!(
                matches!(refused, ActionRefusal::Denied { .. }),
                "{name}: {refused}"
            );
        }
        nothing_done(&server, &delete, name);
    }
}

#[test]
fn a_wrong_or_near_miss_phrase_sends_nothing_and_writes_nothing() {
    let server = server();
    let (fx, head) = repository();
    standard(&server, &head, DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    for typed in [
        "",
        " ",
        "Shipped",
        "SHIPPED",
        "shipped/",
        "refs/heads/shipped",
        "origin/shipped",
        "shipped please",
        "\u{201c}shipped\u{201d}",
        "shipped\u{200b}",
        "7",
    ] {
        let refused = delete.run(Some(&plan.decision), typed).unwrap_err();
        assert_eq!(
            refused,
            ActionRefusal::NotTyped {
                action: PullRequestAction::DeleteHeadBranch,
                pull_request: "octo/rc-git#7".to_owned(),
                phrase: "shipped".to_owned(),
            },
            "{typed:?}"
        );
    }
    let refused = delete.run(None, "shipped").unwrap_err();
    assert!(
        matches!(refused, ActionRefusal::NotConfirmed { .. }),
        "{refused}"
    );
    nothing_done(&server, &delete, "a wrong phrase");
}

#[test]
fn an_answer_to_another_action_sends_nothing() {
    let server = server();
    let (fx, head) = repository();
    standard(&server, &head, DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    let policy = Policy::default();
    let other = |number: u64, name: &str| {
        let mut read = HeadBranch {
            branch: BranchName::from_server(name),
            ..fresh_read(&head)
        };
        read.stamp.number = number;
        policy.evaluate(&Action::DeletePullRequestBranch(Box::new(
            PullRequestBranchDelete::new(&delete.target, "octocat", &read).unwrap(),
        )))
    };
    let answers = [
        policy.evaluate(&Action::DeleteLocalBranch {
            name: "shipped".to_owned(),
            merged: false,
        }),
        policy.evaluate(&Action::DeleteRemoteBranch {
            name: "shipped".to_owned(),
        }),
        other(7, "shipped-2"),
        other(8, "shipped"),
        Decision::Allow,
    ];
    for answer in &answers {
        assert_ne!(answer, &plan.decision);
        let refused = delete.run(Some(answer), "shipped").unwrap_err();
        assert!(
            matches!(refused, ActionRefusal::Changed { .. }),
            "{answer:?}: {refused}"
        );
    }
    nothing_done(&server, &delete, "another action's answer");
}

/// [`Delete`]'s read as [`HeadBranch`] holds it for branch `shipped` at `head`.
fn fresh_read(head: &str) -> HeadBranch {
    let oid = rc_git_core::Oid::from_hex(head).unwrap();
    HeadBranch {
        stamp: stamp(head, PullRequestState::Merged),
        merged: true,
        title: "Ship \\u{202e}it".to_owned(),
        branch: BranchName::from_server("shipped"),
        in_repository: true,
        viewer_can_delete: true,
        head_ref: Some(HeadRef {
            target: Some(oid),
            is_head_branch: true,
            open_heads: Some(OpenPullRequests::default()),
        }),
        repository_id: RepositoryId::parse(REPO_ID),
        default_branch: Some(false),
        open_bases: Some(OpenPullRequests::default()),
    }
}

#[test]
fn a_pull_request_that_is_not_merged_or_not_as_seen_sends_nothing() {
    let server = server();
    let (fx, head) = repository();
    let mut delete = Delete::on(&server, fx, &head);
    for (state, merged) in [("OPEN", false), ("CLOSED", false), ("MERGED", false)] {
        let mut present = read_answer("shipped", &head, Some(&head));
        present = with(present, &["pullRequest", "state"], json!(state));
        present = with(present, &["pullRequest", "merged"], json!(merged));
        script(
            &server,
            "shipped",
            (&present, &present),
            DeleteBehaviour::Delete,
        );
        let seen = match state {
            "OPEN" => PullRequestState::Open,
            "CLOSED" => PullRequestState::Closed,
            _ => PullRequestState::Merged,
        };
        delete.stamp = stamp(&head, seen);
        let refused = delete.plan().unwrap_err();
        assert!(
            matches!(blocker(&refused), Some(ActionBlocker::NotMerged(_))),
            "{state}: {refused}"
        );
        assert!(
            refused.to_string().contains("\u{201c}shipped\u{201d}"),
            "{refused}"
        );
        delete.stamp = stamp(&head, PullRequestState::Merged);
        if state != "MERGED" {
            let refused = delete.plan().unwrap_err();
            assert!(
                matches!(refused, ActionRefusal::Changed { .. }),
                "{state}: {refused}"
            );
        }
    }
    let present = read_answer("shipped", &head, Some(&head));
    script(
        &server,
        "shipped",
        (&present, &present),
        DeleteBehaviour::Delete,
    );
    for stale in [
        stamp(PUSHED, PullRequestState::Merged),
        PullRequestStamp {
            base_ref: "develop".to_owned(),
            ..stamp(&head, PullRequestState::Merged)
        },
    ] {
        delete.stamp = stale;
        let refused = delete.plan().unwrap_err();
        assert!(
            matches!(refused, ActionRefusal::Changed { .. }),
            "{refused}"
        );
        let refused = delete.run(Some(&Decision::Allow), "shipped").unwrap_err();
        assert!(
            matches!(refused, ActionRefusal::Changed { .. }),
            "{refused}"
        );
    }
    nothing_done(&server, &delete, "not merged or stale");
}

/// Plans and runs a delete of `present`'s branch with the answer of an unchanged read, and
/// returns the plan's refusal: each case here refuses before anything is sent or written.
fn refused_for(present: &Value, based_on: &Value) -> (ActionRefusal, ActionRefusal) {
    let server = server();
    let (fx, _) = repository();
    let head = present["data"]["repository"]["pullRequest"]["headRefOid"]
        .as_str()
        .unwrap()
        .to_owned();
    script_based_on(
        &server,
        "shipped",
        (present, present),
        based_on,
        DeleteBehaviour::Delete,
    );
    let delete = Delete::on(&server, fx, &head);
    let planned = delete.plan().unwrap_err();
    let unchanged = Policy::default().evaluate(&Action::DeleteRemoteBranch {
        name: "shipped".to_owned(),
    });
    let ran = delete.run(Some(&unchanged), "shipped").unwrap_err();
    nothing_done(&server, &delete, &planned.to_string());
    (planned, ran)
}

/// Each change to the head branch read that refuses the delete, and the blocker it refuses with.
fn refusing_changes(head: &str) -> Vec<(&'static [&'static str], Value, ActionBlocker)> {
    let oid = |hex: &str| rc_git_core::Oid::from_hex(hex).unwrap();
    let moved = ActionBlocker::BranchMoved {
        now: Some(oid(PUSHED)),
        merged: oid(head),
    };
    let elsewhere = ActionBlocker::HeadNotInRepository;
    vec![
        (
            &["pullRequest", "isCrossRepository"],
            json!(true),
            elsewhere,
        ),
        (
            &["pullRequest", "headRepository", "owner", "login"],
            json!("mallory"),
            elsewhere,
        ),
        (&["pullRequest", "headRepository"], Value::Null, elsewhere),
        (
            &["pullRequest", "headRef", "repository", "owner", "login"],
            json!("mallory"),
            elsewhere,
        ),
        (
            &["pullRequest", "headRef", "prefix"],
            json!("refs/tags/"),
            elsewhere,
        ),
        (
            &["pullRequest", "headRef", "name"],
            json!("shipped2"),
            elsewhere,
        ),
        (
            &["pullRequest", "viewerCanDeleteHeadRef"],
            json!(false),
            ActionBlocker::NoDeleteRight,
        ),
        (
            &["pullRequest", "headRef"],
            Value::Null,
            ActionBlocker::BranchGone,
        ),
        (
            &["pullRequest", "headRef", "target", "oid"],
            json!(PUSHED),
            moved,
        ),
        (
            &["defaultBranchRef", "name"],
            json!("shipped"),
            ActionBlocker::DefaultBranch,
        ),
        (
            &["defaultBranchRef", "name"],
            json!("Shipped"),
            ActionBlocker::DefaultBranch,
        ),
        (
            &["defaultBranchRef"],
            Value::Null,
            ActionBlocker::DefaultBranchUnreported,
        ),
        (&["id"], Value::Null, ActionBlocker::NoRepositoryId),
        (&["id"], json!("not an id!"), ActionBlocker::NoRepositoryId),
    ]
}

#[test]
fn a_fork_s_branch_a_withheld_right_a_moved_or_gone_branch_or_the_default_sends_nothing() {
    let (_, head) = repository();
    let base = read_answer("shipped", &head, Some(&head));
    for (path, value, expected) in refusing_changes(&head) {
        let (planned, ran) = refused_for(&with(base.clone(), path, value), &none_based_on());
        assert_eq!(blocker(&planned), Some(expected), "{path:?}: {planned}");
        assert_eq!(blocker(&ran), Some(expected), "{path:?}: {ran}");
        assert!(
            planned.to_string().contains("\u{201c}shipped\u{201d}"),
            "{planned}"
        );
    }
    let pushed = with(
        base,
        &["pullRequest", "headRef", "target", "oid"],
        json!(PUSHED),
    );
    let moved = refused_for(&pushed, &none_based_on()).0;
    assert!(
        moved
            .to_string()
            .contains("it holds commits the pull request did not merge"),
        "{moved}"
    );
}

#[test]
fn another_open_pull_request_using_the_branch_sends_nothing() {
    let (_, head) = repository();
    let base = read_answer("shipped", &head, Some(&head));
    let heads = with(
        base.clone(),
        &["pullRequest", "headRef", "associatedPullRequests"],
        json!({ "totalCount": 1, "nodes": [ { "number": 9 } ] }),
    );
    let (planned, _) = refused_for(&heads, &none_based_on());
    assert_eq!(
        blocker(&planned),
        Some(ActionBlocker::OpenPullRequests {
            branch_use: BranchUse::Head,
            total: 1,
            first: Some(9)
        })
    );
    assert!(planned.to_string().contains("#9"), "{planned}");
    let based_on = json!({ "data": { "repository": { "pullRequests": {
        "totalCount": 3, "nodes": [ { "number": 11 }, { "number": 12 } ] } } } });
    let (planned, ran) = refused_for(&base, &based_on);
    let expected = Some(ActionBlocker::OpenPullRequests {
        branch_use: BranchUse::Base,
        total: 3,
        first: Some(11),
    });
    assert_eq!(blocker(&planned), expected);
    assert_eq!(blocker(&ran), expected);
    assert!(planned.to_string().contains("#11 and 2 more"), "{planned}");
    for (present, based_on, branch_use) in [
        (
            with(
                base.clone(),
                &["pullRequest", "headRef", "associatedPullRequests"],
                Value::Null,
            ),
            none_based_on(),
            BranchUse::Head,
        ),
        (
            base.clone(),
            json!({ "data": { "repository": { "pullRequests": { "nodes": [] } } } }),
            BranchUse::Base,
        ),
    ] {
        let (planned, _) = refused_for(&present, &based_on);
        assert_eq!(
            blocker(&planned),
            Some(ActionBlocker::OpenPullRequestsUnread(branch_use))
        );
    }
}

#[test]
fn a_head_commit_not_in_the_repository_sends_nothing_and_says_to_fetch_first() {
    let present = read_answer("shipped", PUSHED, Some(PUSHED));
    let (planned, ran) = refused_for(&present, &none_based_on());
    let missing = rc_git_core::Oid::from_hex(PUSHED).unwrap();
    assert_eq!(blocker(&planned), Some(ActionBlocker::NotFetched(missing)));
    assert_eq!(blocker(&ran), Some(ActionBlocker::NotFetched(missing)));
    assert!(
        planned
            .to_string()
            .contains("fetch the pull request's changes first"),
        "{planned}"
    );
}

#[test]
fn a_backup_ref_that_cannot_be_written_sends_nothing() {
    let server = server();
    let (fx, head) = repository();
    fx.git(&["update-ref", "refs/rc-git/backup/remote-branch", &head]);
    standard(&server, &head, DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    let refused = delete.run(Some(&plan.decision), "shipped").unwrap_err();
    assert!(
        matches!(&refused, ActionRefusal::Local { reason, .. } if reason.contains("backup ref")),
        "{refused}"
    );
    assert_eq!(server.delete_requests(), 0);
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.branch_deleted());
    assert_eq!(
        backups(&delete.fx),
        format!("refs/rc-git/backup/remote-branch {head}"),
        "only the ref in the way"
    );
}

#[test]
fn a_branch_that_changes_between_the_challenge_and_the_confirm_sends_nothing() {
    let server = server();
    let (fx, head) = repository();
    let present = read_answer("shipped", &head, Some(&head));
    let pushed = with(
        present.clone(),
        &["pullRequest", "headRef", "target", "oid"],
        json!(PUSHED),
    );
    script(
        &server,
        "shipped",
        (&present, &pushed),
        DeleteBehaviour::ChangeAfter { reads: 1 },
    );
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    let refused = delete.confirmed_with(&plan).unwrap_err();
    assert!(
        matches!(blocker(&refused), Some(ActionBlocker::BranchMoved { .. })),
        "{refused}"
    );
    nothing_done(&server, &delete, "a branch pushed to");
}

#[test]
fn a_pull_request_that_changes_between_the_challenge_and_the_confirm_sends_nothing() {
    let server = server();
    let (fx, head) = repository();
    let present = read_answer("shipped", &head, Some(&head));
    let retitled = with(
        present.clone(),
        &["pullRequest", "title"],
        json!("Ship it now"),
    );
    script(
        &server,
        "shipped",
        (&present, &retitled),
        DeleteBehaviour::ChangeAfter { reads: 1 },
    );
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    let refused = delete.confirmed_with(&plan).unwrap_err();
    assert!(
        matches!(refused, ActionRefusal::Changed { .. }),
        "{refused}"
    );
    assert!(refused.to_string().contains("Ship it now"), "{refused}");
    nothing_done(&server, &delete, "a retitled pull request");
}

impl Delete {
    fn confirmed_with(&self, plan: &BranchDeletePlan) -> Result<BranchDeleteReport, ActionRefusal> {
        self.run(Some(&plan.decision), &plan.branch)
    }
}

/// A cancel flag with short budgets, for answers that never come.
fn impatient() -> Cancel {
    Cancel::new()
        .with_connect_timeout(std::time::Duration::from_secs(5))
        .with_idle_timeout(std::time::Duration::from_secs(2))
}

#[test]
fn lost_answers_are_settled_by_one_read_never_a_second_delete() {
    for (behaviour, expected) in [
        (DeleteBehaviour::DeleteThenDrop, "deleted"),
        (DeleteBehaviour::Drop, "not deleted"),
        (DeleteBehaviour::DropThenChange, "unknown"),
    ] {
        let server = server();
        let (fx, head) = repository();
        let present = read_answer("shipped", &head, Some(&head));
        let pushed = with(
            present.clone(),
            &["pullRequest", "headRef", "target", "oid"],
            json!(PUSHED),
        );
        script(&server, "shipped", (&present, &pushed), behaviour);
        let delete = Delete::on(&server, fx, &head);
        let plan = delete.plan().unwrap();
        let reads = server.graphql_reads();
        let report = delete
            .run_with((Some(&plan.decision), "shipped"), &|| {}, &impatient())
            .unwrap();
        let got = match &report.outcome {
            BranchDeleteOutcome::Deleted {
                settled: Settled::Reread,
            } => "deleted",
            BranchDeleteOutcome::NotDeleted { .. } => "not deleted",
            BranchDeleteOutcome::Unknown {
                after: BranchAfter::Changed { now },
                ..
            } if now.map(|now| now.to_hex()).as_deref() == Some(PUSHED) => "unknown",
            other => panic!("{other:?}"),
        };
        assert_eq!(got, expected);
        assert_eq!(server.delete_requests(), 1, "{expected}: never sent again");
        assert_eq!(server.mutation_requests(), 1);
        assert_eq!(
            server.graphql_reads() - reads,
            3,
            "{expected}: the delete's two reads, then one settling read"
        );
        assert_eq!(backups(&delete.fx), format!("{BACKUP} {head}"), "kept");
    }
}

#[test]
fn a_refusal_passes_the_reason_through_without_the_token_and_keeps_the_backup() {
    let server = server();
    let (fx, head) = repository();
    let body = json!({ "data": { "updateRefs": null }, "errors": [ { "type": "FORBIDDEN",
        "message": format!("Cannot delete this branch with {TOKEN}") } ] })
    .to_string();
    standard(
        &server,
        &head,
        DeleteBehaviour::Refuse {
            status: "200 OK",
            body,
        },
    );
    let delete = Delete::on(&server, fx, &head);
    let report = delete.confirmed().unwrap();
    let BranchDeleteOutcome::Refused { reason } = &report.outcome else {
        panic!("{report:?}");
    };
    assert!(reason.contains("Cannot delete this branch"), "{reason}");
    assert!(!reason.contains(TOKEN), "{reason}");
    assert_eq!(server.delete_requests(), 1);
    assert_eq!(backups(&delete.fx), format!("{BACKUP} {head}"));
}

#[test]
fn a_server_error_is_settled_by_one_read() {
    let server = server();
    let (fx, head) = repository();
    standard(
        &server,
        &head,
        DeleteBehaviour::Refuse {
            status: "502 Bad Gateway",
            body: "{}".to_owned(),
        },
    );
    let delete = Delete::on(&server, fx, &head);
    let report = delete.confirmed().unwrap();
    assert!(
        matches!(report.outcome, BranchDeleteOutcome::NotDeleted { .. }),
        "{report:?}"
    );
    assert_eq!(server.delete_requests(), 1);
}

#[test]
fn a_cancel_before_the_request_leaves_sends_nothing() {
    let server = server();
    let (fx, head) = repository();
    standard(&server, &head, DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    let plan = delete.plan().unwrap();
    let cancel = Cancel::new();
    cancel.signal();
    let refused = delete
        .run_with((Some(&plan.decision), "shipped"), &|| {}, &cancel)
        .unwrap_err();
    assert!(
        matches!(refused, ActionRefusal::Cancelled { .. }),
        "{refused}"
    );
    nothing_done(&server, &delete, "cancelled up front");

    let cancel = Cancel::new();
    let at_send = cancel.clone();
    let refused = delete
        .run_with(
            (Some(&plan.decision), "shipped"),
            &move || at_send.signal(),
            &cancel,
        )
        .unwrap_err();
    assert_eq!(
        refused,
        ActionRefusal::BackupKept {
            pull_request: "octo/rc-git#7".to_owned(),
            branch: "\u{201c}shipped\u{201d}".to_owned(),
            backup_ref: BACKUP.to_owned(),
            stop: BackupKeptStop::Cancelled,
        }
    );
    let said = refused.to_string();
    assert!(
        said.contains(BACKUP) && said.contains("the branch on GitHub is untouched"),
        "{said}"
    );
    assert_eq!(server.delete_requests(), 0, "cancelled at the send");
    assert_eq!(
        backups(&delete.fx),
        format!("{BACKUP} {head}"),
        "the backup written before stays"
    );
}

#[test]
fn a_repository_on_another_host_gets_nothing() {
    let server = server();
    let (fx, head) = repository();
    standard(&server, &head, DeleteBehaviour::Delete);
    let mut delete = Delete::on(&server, fx, &head);
    delete.target =
        RepositoryTarget::on_host(&GitHubHost::github_com(), "https://github.com/octo/rc-git")
            .unwrap();
    let before = server.requests();
    let refused = delete.plan().unwrap_err();
    assert!(
        matches!(
            &refused,
            ActionRefusal::Read {
                source: GitHubError::ForeignRemote { .. },
                ..
            }
        ),
        "{refused}"
    );
    let refused = delete.run(Some(&Decision::Allow), "shipped").unwrap_err();
    assert!(matches!(refused, ActionRefusal::Read { .. }), "{refused}");
    assert_eq!(server.requests(), before);
    nothing_done(&server, &delete, "another host");
}

#[test]
fn the_mutation_primitive_sends_nothing_for_a_branch_delete_it_does_not_perform() {
    let server = server();
    let host = host_of(&server);
    let token = token();
    let session = Session::new(&host, &token);
    for action in [
        Action::DeleteRemoteBranch {
            name: "shipped".to_owned(),
        },
        Action::DeleteLocalBranch {
            name: "shipped".to_owned(),
            merged: false,
        },
    ] {
        let proof = Policy::default()
            .authorize(&action, Some("shipped"))
            .unwrap();
        assert!(session.mutate(proof).is_err(), "{action:?}");
    }
    assert_eq!(server.requests(), 0);
}

#[test]
fn hostile_names_are_shown_neutralized_and_never_deletable() {
    let server = server();
    let (fx, head) = repository();
    let name = "fea\u{202e}ture\nBranch: main";
    let present = read_answer(name, &head, Some(&head));
    script(&server, name, (&present, &present), DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    let refused = delete.plan().unwrap_err();
    assert_eq!(blocker(&refused), Some(ActionBlocker::NameNotTypeable));
    let shown = refused.to_string();
    assert!(!shown.contains(['\u{202e}', '\n']), "{shown}");
    assert!(
        shown.contains("fea\\u{202e}ture\\u{a}Branch: main"),
        "{shown}"
    );
    let typed = delete.run(Some(&Decision::Allow), name).unwrap_err();
    assert_eq!(blocker(&typed), Some(ActionBlocker::NameNotTypeable));
    nothing_done(&server, &delete, "a hostile name");
}

#[test]
fn the_delete_names_the_commit_the_branch_must_still_be_at() {
    let server = server();
    let (fx, head) = repository();
    standard(&server, &head, DeleteBehaviour::Delete);
    let delete = Delete::on(&server, fx, &head);
    delete.confirmed().unwrap();
    let sent = server
        .recorded()
        .into_iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("mutation"))
        .unwrap();
    let body = String::from_utf8_lossy(&sent.body).into_owned();
    for part in [
        "updateRefs(input: {repositoryId: $repository",
        "beforeOid: $before",
        r#"afterOid: \"0000000000000000000000000000000000000000\""#,
        "force: false",
    ] {
        assert!(body.contains(part), "{part}: {body}");
    }
}

#[test]
fn a_branch_pushed_to_as_the_delete_arrives_is_kept_by_the_commit_check() {
    let server = server();
    let (fx, head) = repository();
    let present = read_answer("shipped", &head, Some(&head));
    let pushed = with(
        present.clone(),
        &["pullRequest", "headRef", "target", "oid"],
        json!(PUSHED),
    );
    script(
        &server,
        "shipped",
        (&present, &pushed),
        DeleteBehaviour::PushAtDelete,
    );
    let delete = Delete::on(&server, fx, &head);
    let report = delete.confirmed().unwrap();
    let BranchDeleteOutcome::Refused { reason } = &report.outcome else {
        panic!("{report:?}");
    };
    assert!(
        reason.contains("not at the commit beforeOid names"),
        "{reason}"
    );
    assert!(!server.branch_deleted(), "the pushed branch stays");
    assert_eq!(server.delete_requests(), 1, "sent once, never again");
    assert_eq!(backups(&delete.fx), format!("{BACKUP} {head}"), "kept");
}

#[test]
fn a_host_without_the_checked_delete_refuses_it_and_says_so() {
    for code in ["undefinedField", "variableRequiresValidType"] {
        let server = server();
        let (fx, head) = repository();
        let body = json!({ "errors": [ {
            "message": "Field 'updateRefs' doesn't exist on type 'Mutation'",
            "extensions": { "code": code } } ] })
        .to_string();
        standard(
            &server,
            &head,
            DeleteBehaviour::Refuse {
                status: "200 OK",
                body,
            },
        );
        let delete = Delete::on(&server, fx, &head);
        let plan = delete.plan().unwrap();
        let reads = server.graphql_reads();
        let report = delete.run(Some(&plan.decision), "shipped").unwrap();
        let BranchDeleteOutcome::Refused { reason } = &report.outcome else {
            panic!("{code}: {report:?}");
        };
        assert!(
            reason.starts_with("this host does not take the delete rc-git sends")
                && reason.contains("cannot delete the branch safely here")
                && reason.contains("doesn't exist on type"),
            "{code}: {reason}"
        );
        assert_eq!(
            server.graphql_reads() - reads,
            2,
            "{code}: no settling read"
        );
        assert_eq!(server.delete_requests(), 1);
        assert!(!server.branch_deleted());
    }
}

/// A request that could not leave after the backup ref was written (no connection, no TLS
/// handshake) is the same refusal as the cancel at the send above, with the transport's reason.
/// Driven through the mapping the send uses rather than a closed loopback port: a port the fake
/// gave back is taken again by parallel tests (seen: a reset from another test's listener).
#[test]
fn a_request_that_cannot_leave_after_the_backup_names_the_backup() {
    let transport = GitHubError::Transport {
        op: "delete the head branch",
        url: "http://127.0.0.1:9/api/graphql".to_owned(),
        reason: "Couldn't connect to server".to_owned(),
    };
    assert_eq!(
        delete_branch::backup_stop(transport.clone()),
        BackupKeptStop::NotSent(Box::new(transport.clone()))
    );
    assert_eq!(
        delete_branch::backup_stop(GitHubError::Cancelled {
            op: "delete the head branch"
        }),
        BackupKeptStop::Cancelled
    );
    let refused = ActionRefusal::BackupKept {
        pull_request: "octo/rc-git#7".to_owned(),
        branch: "\u{201c}shipped\u{201d}".to_owned(),
        backup_ref: BACKUP.to_owned(),
        stop: delete_branch::backup_stop(transport),
    };
    assert_eq!(
        refused.to_string(),
        "delete branch \u{201c}shipped\u{201d} of octo/rc-git#7: the request could not be sent \
         (delete the head branch http://127.0.0.1:9/api/graphql: Couldn't connect to server), so \
         nothing was sent and the branch on GitHub is untouched; the backup ref \
         refs/rc-git/backup/remote-branch/shipped written first stays, so delete it with git when \
         you no longer need it"
    );
}
