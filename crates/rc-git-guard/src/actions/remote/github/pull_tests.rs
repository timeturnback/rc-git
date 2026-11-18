//! The pull request reads against the testkit's fake GitHub host, which answers only the exact
//! request body a call must send.

use rc_git_testkit::Fixture;
use rc_git_testkit::github::{FakeToken, GitHubServer};
use serde_json::{Value, json};

use super::testing::{cursor, detail_request_body, list_request_body};
use super::*;
use crate::actions::remote::Cancel;

const TOKEN: &str = "ghp_pull_token_never_printed_0123";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn server() -> GitHubServer {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    server
}

fn host_of(server: &GitHubServer) -> GitHubHost {
    GitHubHost::parse(&server.host()).unwrap()
}

fn token() -> ApiToken {
    ApiToken::new(TOKEN.to_owned()).unwrap()
}

fn target(host: &GitHubHost) -> RepositoryTarget {
    RepositoryTarget::on_host(host, &format!("http://{host}/octo/rc-git.git")).unwrap()
}

fn node(number: u64, title: &str) -> Value {
    json!({
        "number": number, "title": title, "isDraft": false, "state": "OPEN",
        "updatedAt": "2026-11-18T09:00:00Z", "author": { "login": "mona" },
        "headRefName": "feature", "headRefOid": HEAD, "baseRefName": "main",
        "isCrossRepository": false, "headRepositoryOwner": { "login": "octo" },
        "reviewDecision": "REVIEW_REQUIRED",
        "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "PENDING" } } } ] }
    })
}

fn page(nodes: &[Value], next: Option<&str>) -> String {
    json!({ "data": { "repository": { "pullRequests": {
        "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
        "nodes": nodes,
    } } } })
    .to_string()
}

#[test]
fn a_page_lists_open_pull_requests_and_hands_out_its_cursor() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    server.script_graphql(
        &list_request_body(&target, None),
        &page(
            &[node(7, "Add the thing"), Value::Null],
            Some("Y3Vyc29yOjE="),
        ),
    );
    server.script_graphql(
        &list_request_body(&target, Some(&cursor("Y3Vyc29yOjE="))),
        &page(&[node(3, "Older")], None),
    );
    let first = list_open_pull_requests(&host, &token(), &target, None, &Cancel::new()).unwrap();
    assert_eq!(first.pull_requests.len(), 1, "a null node is skipped");
    let pr = &first.pull_requests[0];
    assert_eq!((pr.number, pr.title.as_str()), (7, "Add the thing"));
    assert_eq!(pr.author.as_deref(), Some("mona"));
    assert_eq!(
        (pr.head_ref.as_str(), pr.base_ref.as_str()),
        ("feature", "main")
    );
    assert_eq!(pr.head_oid.to_hex(), HEAD);
    assert_eq!(pr.checks, Some(StatusState::Pending));
    assert_eq!(pr.review, Some(ReviewDecision::ReviewRequired));
    assert_eq!(pr.updated_at, Some(1_794_992_400));
    let second = list_open_pull_requests(
        &host,
        &token(),
        &target,
        first.next.as_ref(),
        &Cancel::new(),
    )
    .unwrap();
    assert_eq!(second.pull_requests[0].number, 3);
    assert_eq!(second.next, None);
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 2);
    for request in &recorded {
        assert_eq!(
            (request.method.as_str(), request.target.as_str()),
            ("POST", "/api/graphql")
        );
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
}

#[test]
fn a_read_of_a_repository_on_another_host_sends_nothing() {
    let account = server();
    let elsewhere = server();
    let account_host = host_of(&account);
    let foreign = target(&host_of(&elsewhere));
    let listed = list_open_pull_requests(&account_host, &token(), &foreign, None, &Cancel::new());
    let read = read_pull_request(&account_host, &token(), &foreign, 7, &Cancel::new());
    for error in [listed.unwrap_err(), read.unwrap_err()] {
        assert!(
            matches!(error, GitHubError::ForeignRemote { .. }),
            "{error}"
        );
    }
    assert_eq!(account.requests() + elsewhere.requests(), 0);
}

#[test]
fn hostile_text_arrives_neutralized_and_links_keep_their_real_host() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    let mut hostile = node(9, "evil\u{202e}gpj.exe\n\u{1b}[31mred");
    hostile["headRefName"] = json!("x\u{2066}y");
    hostile["author"] = json!({ "login": "a\u{7}b" });
    hostile["body"] =
        json!("line one\r\n\u{1b}]8;;https://evil.example\u{7}click\u{200b}\nline two");
    hostile["url"] = json!(format!("http://{host}/octo/rc-git/pull/9"));
    hostile["mergeable"] = json!("MERGEABLE");
    hostile["commits"] = json!({ "nodes": [ { "commit": { "statusCheckRollup": {
        "state": "FAILURE",
        "contexts": { "totalCount": 3, "nodes": [
            { "__typename": "CheckRun", "name": "build\u{202e}", "status": "COMPLETED",
              "conclusion": "FAILURE", "detailsUrl": "https://ci.example.com/b/1",
              "databaseId": 55, "isRequired": true,
              "checkSuite": { "workflowRun": { "databaseId": 44 } } },
            { "__typename": "StatusContext", "context": "deploy", "state": "PENDING",
              "targetUrl": "javascript:alert(1)", "description": "wait\nfor it",
              "isRequired": false },
            { "__typename": "Mystery" }
        ] } } } } ] });
    let answer = json!({ "data": { "repository": {
        "mergeCommitAllowed": true, "squashMergeAllowed": true, "rebaseMergeAllowed": false,
        "autoMergeAllowed": true, "pullRequest": hostile } } });
    server.script_graphql(&detail_request_body(&target, 9, false), &answer.to_string());
    let detail = read_pull_request(&host, &token(), &target, 9, &Cancel::new()).unwrap();
    assert_eq!(detail.summary.title, "evil\\u{202e}gpj.exe\\u{a}red");
    assert_eq!(detail.summary.head_ref, "x\\u{2066}y");
    assert_eq!(detail.summary.author.as_deref(), Some("a\\u{7}b"));
    assert_eq!(detail.body, "line one\nclick\\u{200b}\nline two");
    assert_eq!(
        detail.url,
        Some(ServerLink::OnHost(format!(
            "http://{host}/octo/rc-git/pull/9"
        )))
    );
    let [build, deploy] = detail.checks.checks.as_slice() else {
        panic!("{:?}", detail.checks);
    };
    assert_eq!(build.name, "build\\u{202e}");
    assert_eq!(
        build.details,
        Some(ServerLink::Elsewhere {
            url: "https://ci.example.com/b/1".to_owned(),
            host: "ci.example.com".to_owned()
        })
    );
    assert_eq!(
        build.actions,
        Some(ActionsIds {
            workflow_run: 44,
            job: 55
        })
    );
    assert_eq!((build.required, deploy.required), (Some(true), Some(false)));
    assert_eq!(deploy.details, None, "a javascript: link is dropped");
    assert_eq!(deploy.description.as_deref(), Some("wait\\u{a}for it"));
    assert_eq!(detail.checks.total, 3);
}

fn detail_answer(number: u64) -> Value {
    let mut pr = node(number, "Ship it");
    for (key, value) in [
        ("body", json!("")),
        ("url", Value::Null),
        ("mergeable", json!("CONFLICTING")),
        ("mergeStateStatus", json!("BLOCKED")),
        ("viewerCanMergeAsAdmin", json!(true)),
        ("viewerCanEnableAutoMerge", json!(false)),
        ("viewerCanDisableAutoMerge", json!(false)),
        ("viewerCanDeleteHeadRef", json!(true)),
        (
            "autoMergeRequest",
            json!({ "mergeMethod": "SQUASH", "enabledBy": { "login": "o" } }),
        ),
    ] {
        pr[key] = value;
    }
    json!({ "data": { "repository": {
        "mergeCommitAllowed": false, "squashMergeAllowed": true, "rebaseMergeAllowed": true,
        "autoMergeAllowed": true, "pullRequest": pr } } })
}

#[test]
fn a_detail_carries_the_merge_inputs_and_the_stamp_of_what_was_read() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    server.script_graphql(
        &detail_request_body(&target, 5, false),
        &detail_answer(5).to_string(),
    );
    let detail = read_pull_request(&host, &token(), &target, 5, &Cancel::new()).unwrap();
    assert_eq!(detail.fields, DetailFields::Full);
    assert_eq!(detail.mergeable, Mergeable::Conflicting);
    assert_eq!(detail.merge_state, Some(MergeStateStatus::Blocked));
    assert_eq!(
        detail.merge_methods,
        MergeMethods {
            merge: false,
            squash: true,
            rebase: true
        }
    );
    let viewer = detail.viewer.unwrap();
    assert!(viewer.can_merge_as_admin && viewer.can_delete_head_ref);
    assert!(!viewer.can_enable_auto_merge);
    assert_eq!(
        detail.auto_merge.as_ref().unwrap().method,
        Some(MergeMethod::Squash)
    );
    let stamp = detail.stamp();
    assert_eq!(
        (
            stamp.number,
            stamp.head_oid.to_hex(),
            stamp.base_ref.as_str(),
            stamp.state
        ),
        (5, HEAD.to_owned(), "main", PullRequestState::Open)
    );
}

#[test]
fn a_host_without_a_newer_field_is_asked_once_more_without_them() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    let unknown = json!({ "errors": [ {
        "message": "Field 'viewerCanMergeAsAdmin' doesn't exist on type 'PullRequest'",
        "extensions": { "code": "undefinedField", "typeName": "PullRequest" } } ] });
    server.script_graphql(
        &detail_request_body(&target, 5, false),
        &unknown.to_string(),
    );
    server.script_graphql(
        &detail_request_body(&target, 5, true),
        &detail_answer(5).to_string(),
    );
    let detail = read_pull_request(&host, &token(), &target, 5, &Cancel::new()).unwrap();
    assert_eq!(detail.fields, DetailFields::Basic);
    assert_eq!((detail.viewer, detail.merge_state), (None, None));
    assert_eq!(server.requests_to("/api/graphql"), 2);
}

#[test]
fn a_graphql_failure_names_github_s_message_without_the_token() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    let missing = json!({ "data": { "repository": null }, "errors": [ { "type": "NOT_FOUND",
        "message": format!("Could not resolve to a Repository; you sent {TOKEN}") } ] });
    server.script_graphql(&list_request_body(&target, None), &missing.to_string());
    let error =
        list_open_pull_requests(&host, &token(), &target, None, &Cancel::new()).unwrap_err();
    assert!(
        matches!(&error, GitHubError::Status { message, .. }
            if message.contains("Could not resolve") && message.contains("<redacted>")),
        "{error}"
    );
    assert!(!format!("{error} {error:?}").contains(TOKEN));
}

#[test]
fn a_spent_graphql_rate_limit_names_its_reset() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    let spent = json!({ "errors": [ { "message": "API rate limit exceeded" } ] });
    server.script_graphql_with_headers(
        &list_request_body(&target, None),
        &spent.to_string(),
        &[
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset", "1800000000"),
        ],
    );
    let error =
        list_open_pull_requests(&host, &token(), &target, None, &Cancel::new()).unwrap_err();
    assert!(
        matches!(
            error,
            GitHubError::RateLimited {
                reset: Some(1_800_000_000),
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_refused_token_or_a_bad_number_is_said_and_a_bad_number_sends_nothing() {
    let server = server();
    let host = host_of(&server);
    let target = target(&host);
    let wrong = ApiToken::new("ghp_some_other_token".to_owned()).unwrap();
    let error = list_open_pull_requests(&host, &wrong, &target, None, &Cancel::new()).unwrap_err();
    assert!(matches!(error, GitHubError::Unauthorized { .. }), "{error}");
    let before = server.requests();
    for number in [0, 1 << 31] {
        let error =
            read_pull_request(&host, &token(), &target, number, &Cancel::new()).unwrap_err();
        assert!(matches!(error, GitHubError::InvalidValue { .. }), "{error}");
    }
    assert_eq!(server.requests(), before);
}

/// An ssh directory holding `config`.
fn ssh_dir(config: &str) -> Fixture {
    let dir = Fixture::empty_dir();
    dir.write("config", config.as_bytes());
    dir
}

#[test]
fn an_ssh_alias_reaches_the_account_only_through_the_hostname_it_maps_to() {
    let com = GitHubHost::github_com();
    let remote = "git@github-work:octo/rc-git.git";
    let mapped = ssh_dir("Host github-work\n  HostName GitHub.com\n  User git\n");
    let target = RepositoryTarget::for_account(&com, remote, Some(mapped.path())).unwrap();
    assert_eq!(
        (target.host(), target.owner(), target.name()),
        (&com, "octo", "rc-git")
    );
    assert_eq!(
        ssh_dialed_host(remote, Some(mapped.path())).unwrap(),
        Some(com.clone())
    );
    for config in [
        "Host github-work\n  HostName github.com.evil.example\n",
        "Host github-work\n  HostName evil.example\n",
        "Host github-work\n  HostName github.com\n  Port 2222\n",
        "Host other\n  HostName github.com\n",
    ] {
        let dir = ssh_dir(config);
        let error = RepositoryTarget::for_account(&com, remote, Some(dir.path())).unwrap_err();
        assert!(
            matches!(error, GitHubError::ForeignRemote { .. }),
            "{config}: {error}"
        );
    }
    let proxied = ssh_dir("Host github-work\n  HostName github.com\n  ProxyCommand nc %h %p\n");
    let error = RepositoryTarget::for_account(&com, remote, Some(proxied.path())).unwrap_err();
    assert!(error.to_string().contains("ProxyCommand"), "{error}");
}

#[test]
fn a_remote_on_the_account_host_never_reads_the_ssh_config() {
    let com = GitHubHost::github_com();
    let unreadable = Fixture::empty_dir();
    unreadable.write("config", b"Host *\n  ProxyCommand nc %h %p\n");
    for remote in [
        "git@github.com:octo/rc-git.git",
        "https://github.com/octo/rc-git",
    ] {
        let target = RepositoryTarget::for_account(&com, remote, Some(unreadable.path())).unwrap();
        assert_eq!(target.name(), "rc-git");
    }
    let https = "https://github-work/octo/rc-git";
    assert_eq!(
        ssh_dialed_host(https, Some(unreadable.path())).unwrap(),
        None
    );
}

#[test]
fn the_pull_request_remote_is_the_branch_remote_else_origin() {
    let fixture = Fixture::standard();
    assert_eq!(pull_request_remote(fixture.path()).unwrap(), None);
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://user:pw@github.com/octo/rc-git.git",
    ]);
    fixture.git(&[
        "remote",
        "add",
        "upstream",
        "git@github.com:team/rc-git.git",
    ]);
    let origin = pull_request_remote(fixture.path()).unwrap().unwrap();
    assert_eq!(origin.name(), "origin");
    assert_eq!(origin.shown(), "https://github.com/octo/rc-git.git");
    assert!(!format!("{origin:?}").contains("pw"));
    let refused = RepositoryTarget::for_account(&GitHubHost::github_com(), origin.url(), None);
    assert!(refused.is_err(), "a URL with userinfo names no repository");
    let branch = fixture.git_output(&["branch", "--show-current"]);
    fixture.git(&[
        "config",
        &format!("branch.{}.remote", branch.trim()),
        "upstream",
    ]);
    let upstream = pull_request_remote(fixture.path()).unwrap().unwrap();
    assert_eq!(upstream.name(), "upstream");
    fixture.git(&["checkout", "--detach"]);
    assert_eq!(
        pull_request_remote(fixture.path()).unwrap().unwrap().name(),
        "origin"
    );
}
