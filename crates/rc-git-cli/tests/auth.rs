//! `rc-git auth` and `rc-git pr` against the testkit's fake GitHub host. Every run points the
//! CLI at a scratch
//! settings plist and a scratch keychain service (`RC_GIT_SETTINGS`, `RC_GIT_KEYCHAIN_SERVICE`),
//! so nothing touches the user's real accounts or keychain items; each test signs its accounts
//! out again through the CLI itself, the program that created the items.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers panic when the binary cannot run"
)]

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rc_git_guard::actions::github::testing::{
    detail_request_body, list_request_body, merge_request_body,
};
use rc_git_guard::actions::github::{GitHubHost, MergeMethod, RepositoryTarget};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{
    DeviceScript, FakeToken, GitHubServer, Grant, MergeBehaviour, MergeScript, PollAnswer,
    USER_CODE,
};

const TOKEN: &str = "ghp_cli_token_never_printed_0123";
const CLIENT: &str = "Iv23li.cli-client";

/// A scratch settings domain and keychain service, and the accounts to sign out at the end.
struct Scratch {
    folder: Fixture,
    service: String,
    signed_in: Vec<(String, String)>,
}

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let serial = NEXT.fetch_add(1, Ordering::SeqCst);
        Self {
            folder: Fixture::empty_dir(),
            service: format!("rc-git-test-{}-{nanos}-{serial}", std::process::id()),
            signed_in: Vec::new(),
        }
    }

    /// The binary with only `PATH` and `HOME` from the caller, pointed at the scratch stores.
    #[expect(clippy::disallowed_methods, reason = "tests run the built binary")]
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rc-git"));
        let settings = self.folder.path().join("settings");
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("RC_GIT_SETTINGS", settings)
            .env("RC_GIT_KEYCHAIN_SERVICE", &self.service)
            .args(args)
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn run_with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Remembers `login` on `host` for the sign-out on drop.
    fn will_sign_out(&mut self, host: &str, login: &str) {
        self.signed_in.push((host.to_owned(), login.to_owned()));
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for (host, login) in std::mem::take(&mut self.signed_in) {
            drop(self.run(&["auth", "logout", "--host", &host, "--login", &login]));
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn server_knowing(token: &str, login: &str) -> GitHubServer {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: token.to_owned(),
        login: login.to_owned(),
        scopes: Some("repo".to_owned()),
        expires: Some("2026-12-01 10:00:00 UTC".to_owned()),
    });
    server
}

#[test]
fn a_piped_token_signs_in_shows_its_status_and_signs_out() {
    let server = server_knowing(TOKEN, "octocat");
    let host = server.host();
    let mut scratch = Scratch::new();
    let login = ["auth", "login", "--host", &host, "--method", "token"];
    let out = scratch.run_with_stdin(&login, &format!("{TOKEN}\n"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    scratch.will_sign_out(&host, "octocat");
    assert_eq!(
        text(&out.stdout),
        format!("signed-in\t{host}\toctocat\ttoken\trepo\t2026-12-01 10:00:00 UTC\n")
    );

    let status = scratch.run(&["auth", "status"]);
    assert!(status.status.success(), "{}", text(&status.stderr));
    assert_eq!(
        text(&status.stdout),
        format!("account\t{host}\toctocat\ttoken\trepo\t2026-12-01 10:00:00 UTC\tok\n")
    );

    let logout = scratch.run(&["auth", "logout", "--host", &host, "--login", "octocat"]);
    assert!(logout.status.success(), "{}", text(&logout.stderr));
    assert_eq!(
        text(&logout.stdout),
        format!("signed-out\t{host}\toctocat\n")
    );
    let after = scratch.run(&["auth", "status"]);
    assert_eq!(after.status.code(), Some(1));
    assert!(text(&after.stderr).contains("no GitHub account is signed in"));
    for output in [&out, &status, &logout, &after] {
        let printed = format!("{}{}", text(&output.stdout), text(&output.stderr));
        assert!(!printed.contains(TOKEN), "{printed}");
    }
}

#[test]
fn a_token_is_never_taken_from_an_argument() {
    let scratch = Scratch::new();
    let out = scratch.run(&["auth", "login", "--method", "token", "--token", TOKEN]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("does not take '--token'"));
}

#[test]
fn a_rejected_token_exits_5_and_never_echoes_it() {
    let server = server_knowing("ghp_some_other_token", "octocat");
    let scratch = Scratch::new();
    let login = [
        "auth",
        "login",
        "--host",
        &server.host(),
        "--method",
        "token",
    ];
    let out = scratch.run_with_stdin(&login, TOKEN);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stderr));
    let printed = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(!printed.contains(TOKEN), "{printed}");
    assert!(printed.contains("rejected the token"), "{printed}");
}

#[test]
fn a_host_with_a_scheme_or_path_is_a_usage_error() {
    let scratch = Scratch::new();
    for host in ["https://github.com", "github.com/evil", "user@github.com"] {
        let out = scratch.run(&["auth", "login", "--host", host, "--method", "token"]);
        assert_eq!(out.status.code(), Some(2), "{host}: {}", text(&out.stderr));
    }
}

fn device_script(polls: Vec<PollAnswer>) -> DeviceScript {
    DeviceScript {
        client_id: CLIENT.to_owned(),
        scope: None,
        verification_uri: None,
        interval: 1,
        expires_in: 900,
        polls: VecDeque::from(polls),
    }
}

#[test]
fn a_device_sign_in_prints_the_code_and_page_then_renews_its_token_once() {
    let server = server_knowing("ghu_short_lived", "octocat");
    server.add_token(FakeToken {
        token: "ghu_renewed".to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    let short = Grant {
        access: "ghu_short_lived".to_owned(),
        expires_in: Some(60),
        refresh: Some("ghr_first".to_owned()),
    };
    server.script_device_flow(device_script(vec![
        PollAnswer::Pending,
        PollAnswer::Grant(short),
    ]));
    let renewed = Grant {
        access: "ghu_renewed".to_owned(),
        expires_in: Some(28_800),
        refresh: Some("ghr_second".to_owned()),
    };
    server.add_refresh("ghr_first", CLIENT, renewed);
    let host = server.host();
    let mut scratch = Scratch::new();
    let login = ["auth", "login", "--host", &host, "--client-id", CLIENT];
    let out = scratch.run(&login);
    assert!(out.status.success(), "{}", text(&out.stderr));
    scratch.will_sign_out(&host, "octocat");
    let printed = text(&out.stdout);
    assert!(printed.starts_with(&format!(
        "code\t{USER_CODE}\nopen\t{}/login/device\nexpires\t15 minutes\n",
        server.origin()
    )));
    assert!(
        printed.ends_with(&format!(
            "signed-in\t{host}\toctocat\tgithub-app\trepo\t2026-12-01 10:00:00 UTC\n"
        )),
        "{printed}"
    );

    for _ in 0..2 {
        let status = scratch.run(&["auth", "status", "--host", &host]);
        assert!(status.status.success(), "{}", text(&status.stderr));
        assert!(
            text(&status.stdout).ends_with("\tok\n"),
            "{}",
            text(&status.stdout)
        );
    }
    let refreshes = server
        .recorded()
        .iter()
        .filter(|request| {
            String::from_utf8_lossy(&request.body).contains("grant_type=refresh_token")
        })
        .count();
    assert_eq!(refreshes, 1, "the renewed token was stored and reused");
}

#[test]
fn a_device_flow_naming_a_foreign_page_signs_nothing_in() {
    let server = GitHubServer::start();
    let mut foreign = device_script(vec![PollAnswer::Grant(Grant {
        access: TOKEN.to_owned(),
        expires_in: None,
        refresh: None,
    })]);
    foreign.verification_uri = Some("https://evil.example/login/device".to_owned());
    foreign.scope = Some("repo".to_owned());
    server.script_device_flow(foreign);
    let scratch = Scratch::new();
    let host = server.host();
    let out = scratch.run(&[
        "auth",
        "login",
        "--host",
        &host,
        "--method",
        "oauth-app",
        "--client-id",
        CLIENT,
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("evil.example"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        !text(&out.stdout).contains("open\t"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(server.requests_to("/login/oauth/access_token"), 0);
}

#[test]
fn a_device_method_needs_a_client_id() {
    let scratch = Scratch::new();
    let out = scratch.run(&["auth", "login", "--method", "github-app"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("--client-id"));
}

/// Signs [`TOKEN`] in as `octocat` on `server` and makes a repository whose `origin` is
/// `octo/rc-git` there (or `remote` when given), with an empty ssh directory beside it.
fn pr_setup(server: &GitHubServer, remote: Option<&str>) -> (Scratch, Fixture, Fixture) {
    let mut scratch = Scratch::new();
    let host = server.host();
    let login = ["auth", "login", "--host", &host, "--method", "token"];
    let out = scratch.run_with_stdin(&login, TOKEN);
    assert!(out.status.success(), "{}", text(&out.stderr));
    scratch.will_sign_out(&host, "octocat");
    let repo = Fixture::standard();
    let url = format!("{}/octo/rc-git.git", server.origin());
    repo.git(&["remote", "add", "origin", remote.unwrap_or(&url)]);
    (scratch, repo, Fixture::empty_dir())
}

fn pr_target(server: &GitHubServer) -> RepositoryTarget {
    let host = GitHubHost::parse(&server.host()).unwrap();
    RepositoryTarget::on_host(&host, &format!("{}/octo/rc-git.git", server.origin())).unwrap()
}

fn run_pr(scratch: &Scratch, repo: &Fixture, ssh: &Fixture, verb: &[&str]) -> Output {
    let path = repo.path().to_string_lossy().into_owned();
    let ssh_dir = ssh.path().to_string_lossy().into_owned();
    let mut args = vec!["pr"];
    args.extend_from_slice(verb);
    args.extend_from_slice(&[path.as_str(), "--ssh-dir", ssh_dir.as_str()]);
    scratch.run(&args)
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn pr_list_prints_one_neutralized_record_per_pull_request() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    let page = format!(
        r#"{{"data": {{"repository": {{"pullRequests": {{
            "pageInfo": {{"hasNextPage": true, "endCursor": "Y3Vyc29y"}},
            "nodes": [{{
                "number": 7, "title": "evil\u202e\t\u001b[2Jtitle",
                "isDraft": false, "state": "OPEN", "updatedAt": "2026-11-18T09:00:00Z",
                "author": {{"login": "mona"}}, "headRefName": "feature",
                "headRefOid": "{HEAD}", "baseRefName": "main", "isCrossRepository": false,
                "headRepositoryOwner": {{"login": "octo"}}, "reviewDecision": "APPROVED",
                "commits": {{"nodes": [
                    {{"commit": {{"statusCheckRollup": {{"state": "FAILURE"}}}}}}
                ]}}
            }}]
        }}}}}}}}"#
    );
    server.script_graphql(&list_request_body(&pr_target(&server), None), &page);
    let out = run_pr(&scratch, &repo, &ssh, &["list"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "pr\t7\tOpen\tfailing\tApproved\tmona\tfeature\tmain\tevil\\u{202e}\\u{9}title\n\
         more\tY3Vyc29y\n"
    );
    let graphql: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|request| request.target == "/api/graphql")
        .collect();
    assert_eq!(graphql.len(), 1);
    assert_eq!(
        graphql[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert!(!format!("{}{}", text(&out.stdout), text(&out.stderr)).contains(TOKEN));
}

#[test]
fn pr_view_and_checks_print_the_detail_and_its_checks_required_first() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    let detail = format!(
        r#"{{"data": {{"repository": {{
            "mergeCommitAllowed": true, "squashMergeAllowed": true,
            "rebaseMergeAllowed": false, "autoMergeAllowed": false,
            "pullRequest": {{
                "number": 7, "title": "Ship",
                "body": "line one\r\nline \u001b]8;;x\u0007two",
                "url": "https://evil.example/x", "isDraft": false, "state": "OPEN",
                "updatedAt": "2026-11-18T09:00:00Z", "author": null,
                "headRefName": "feature", "headRefOid": "{HEAD}", "baseRefName": "main",
                "isCrossRepository": true, "headRepositoryOwner": {{"login": "fork"}},
                "reviewDecision": null, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
                "viewerCanMergeAsAdmin": false, "viewerCanEnableAutoMerge": false,
                "viewerCanDisableAutoMerge": false, "viewerCanDeleteHeadRef": false,
                "autoMergeRequest": null,
                "commits": {{"nodes": [{{"commit": {{"statusCheckRollup": {{
                    "state": "PENDING",
                    "contexts": {{"totalCount": 2, "nodes": [
                        {{"__typename": "StatusContext", "context": "deploy",
                          "state": "PENDING", "targetUrl": null, "description": "waiting",
                          "isRequired": false}},
                        {{"__typename": "CheckRun", "name": "build", "status": "COMPLETED",
                          "conclusion": "SUCCESS", "detailsUrl": "https://ci.example/1",
                          "databaseId": 5, "isRequired": true,
                          "checkSuite": {{"workflowRun": null}}}}
                    ]}}
                }}}}}}]}}
            }}
        }}}}}}"#
    );
    server.script_graphql(&detail_request_body(&pr_target(&server), 7, false), &detail);
    let view = run_pr(&scratch, &repo, &ssh, &["view", "7"]);
    assert!(view.status.success(), "{}", text(&view.stderr));
    let printed = text(&view.stdout);
    for line in [
        "number\t7\n",
        "author\t-\n",
        &format!("head\tfork\tfeature\t{HEAD}\n"),
        "url\thttps://evil.example/x\n",
        "checks\t1 pending, 1 passing; 1 required.\n",
        "body\tline one\nbody\tline two\n",
    ] {
        assert!(printed.contains(line), "{line:?} in {printed}");
    }
    assert!(!printed.contains('\u{1b}'), "{printed}");
    let checks = run_pr(&scratch, &repo, &ssh, &["checks", "7"]);
    assert!(checks.status.success(), "{}", text(&checks.stderr));
    assert_eq!(
        text(&checks.stdout),
        "check\tpassing\trequired\tbuild\thttps://ci.example/1\t-\n\
         check\tpending\toptional\tdeploy\t-\twaiting\n"
    );
}

#[test]
fn pr_reads_nothing_for_a_remote_on_another_host_or_without_an_account() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, Some("https://github.com.evil.example/o/r.git"));
    let foreign = run_pr(&scratch, &repo, &ssh, &["list"]);
    assert_eq!(foreign.status.code(), Some(1), "{}", text(&foreign.stderr));
    assert!(text(&foreign.stderr).contains("names no GitHub repository"));
    let unsigned = Fixture::standard();
    unsigned.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/octo/rc-git.git",
    ]);
    let none = run_pr(&scratch, &unsigned, &ssh, &["list"]);
    assert_eq!(none.status.code(), Some(5), "{}", text(&none.stderr));
    assert!(text(&none.stderr).contains("rc-git auth login --host github.com"));
    assert_eq!(server.requests_to("/api/graphql"), 0);
}

/// Scripts pull request 7 with head `head` on base `main`, in the full detail read.
fn script_pull_request(server: &GitHubServer, head: &str) {
    script_pull_request_on(server, head, None);
}

/// [`script_pull_request`], with GitHub reporting its base at `base` when set.
fn script_pull_request_on(server: &GitHubServer, head: &str, base: Option<&str>) {
    let base = base.map_or_else(String::new, |oid| format!(r#""baseRefOid": "{oid}","#));
    let detail = format!(
        r#"{{"data": {{"repository": {{
            "mergeCommitAllowed": true, "squashMergeAllowed": true,
            "rebaseMergeAllowed": true, "autoMergeAllowed": false,
            "pullRequest": {{
                "number": 7, "title": "Ship", "body": "", "url": null, "isDraft": false,
                "state": "OPEN", "updatedAt": "2026-11-18T09:00:00Z",
                "author": {{"login": "mona"}}, "headRefName": "topic", "headRefOid": "{head}",
                "baseRefName": "main", {base} "isCrossRepository": false,
                "headRepositoryOwner": {{"login": "octo"}}, "reviewDecision": null,
                "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
                "viewerCanMergeAsAdmin": false, "viewerCanEnableAutoMerge": false,
                "viewerCanDisableAutoMerge": false, "viewerCanDeleteHeadRef": false,
                "autoMergeRequest": null,
                "commits": {{"nodes": [{{"commit": {{"statusCheckRollup": null}}}}]}}
            }}
        }}}}}}"#
    );
    server.script_graphql(&detail_request_body(&pr_target(server), 7, false), &detail);
}

/// The git requests the fake host got: its ref advertisement endpoint.
fn git_requests(server: &GitHubServer) -> Vec<rc_git_testkit::github::RecordedRequest> {
    server
        .recorded()
        .into_iter()
        .filter(|request| request.target.contains(".git/info/refs"))
        .collect()
}

#[test]
fn pr_diff_lists_the_changes_of_a_local_head_from_its_merge_base() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    let base = repo.rev_parse("HEAD~1");
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    repo.git(&["checkout", "-q", "-b", "topic", &base]);
    repo.write("topic.txt", b"topic\n");
    repo.commit("topic work", "2024-05-01T00:00:00+0000");
    let head = repo.rev_parse("topic");
    let reported = repo.rev_parse("refs/remotes/origin/main");
    script_pull_request_on(&server, &head, Some(&reported));

    let out = run_pr(&scratch, &repo, &ssh, &["diff", "7"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let names = repo.git_output(&["diff", "--name-status", &format!("{reported}...{head}")]);
    assert_eq!(
        text(&out.stdout),
        format!("base\t{base}\tpull-request-base\t{reported}\nhead\t{head}\n{names}\n")
    );

    script_pull_request(&server, &head);
    let out = run_pr(&scratch, &repo, &ssh, &["diff", "7"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with(&format!(
            "base\t{base}\tlocal-copy\trefs/remotes/origin/main\n"
        )),
        "a host that reports no base commit is compared with the local copy, and says so"
    );

    let absent = "fedcba9876543210fedcba9876543210fedcba98";
    script_pull_request_on(&server, &head, Some(absent));
    let out = run_pr(&scratch, &repo, &ssh, &["diff", "7"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(&format!("commit {absent}, where main is on GitHub")),
        "{}",
        text(&out.stderr)
    );
    assert!(text(&out.stdout).is_empty(), "no diff from another base");
    assert!(
        git_requests(&server).is_empty(),
        "a local head needs no fetch"
    );
}

#[test]
fn pr_diff_fetches_only_with_fetch_and_never_offers_the_api_token_to_git() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let missing = "0123456789abcdef0123456789abcdef01234567";
    script_pull_request(&server, missing);

    let without = run_pr(&scratch, &repo, &ssh, &["diff", "7"]);
    assert_eq!(without.status.code(), Some(1), "{}", text(&without.stderr));
    assert!(text(&without.stderr).contains("rerun with --fetch"));
    assert!(git_requests(&server).is_empty(), "no fetch without --fetch");

    let fetch = run_pr(&scratch, &repo, &ssh, &["diff", "7", "--fetch"]);
    assert_eq!(fetch.status.code(), Some(5), "{}", text(&fetch.stderr));
    let asked = git_requests(&server);
    assert!(!asked.is_empty(), "--fetch asks the remote");
    for request in &asked {
        assert_eq!(
            request.header("authorization"),
            None,
            "the API token is never a git credential"
        );
    }

    let with_git = [
        "diff",
        "7",
        "--fetch",
        "--username",
        "x",
        "--token",
        "git-secret",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &with_git);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stderr));
    let offered: Vec<String> = git_requests(&server)
        .iter()
        .filter_map(|request| request.header("authorization").map(str::to_owned))
        .collect();
    assert_eq!(
        offered,
        ["Basic eDpnaXQtc2VjcmV0"],
        "only the git credential: x:git-secret"
    );
    let printed = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(
        !printed.contains(TOKEN) && !printed.contains("git-secret"),
        "{printed}"
    );
    assert_eq!(
        repo.git_output(&["for-each-ref", "refs/rc-git/"]),
        "",
        "nothing written"
    );
}

const NODE: &str = "PR_kwDOcli7";
const MOVED: &str = "89abcdef0123456789abcdef0123456789abcdef";

/// Pull request 7 at `head` on `main` in `state`, mergeable with a squash.
fn mergeable(head: &str, state: &str) -> String {
    format!(
        r#"{{"data": {{"repository": {{
            "mergeCommitAllowed": false, "squashMergeAllowed": true,
            "rebaseMergeAllowed": false, "autoMergeAllowed": false,
            "pullRequest": {{
                "id": "{NODE}", "number": 7, "title": "Ship", "body": "", "url": null,
                "isDraft": false, "state": "{state}", "updatedAt": "2026-11-18T09:00:00Z",
                "author": {{"login": "mona"}}, "headRefName": "topic", "headRefOid": "{head}",
                "baseRefName": "main", "isCrossRepository": false,
                "headRepositoryOwner": {{"login": "octo"}}, "reviewDecision": null,
                "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
                "viewerCanMergeAsAdmin": false, "viewerCanEnableAutoMerge": false,
                "viewerCanDisableAutoMerge": false, "viewerCanDeleteHeadRef": false,
                "autoMergeRequest": null,
                "commits": {{"nodes": [{{"commit": {{"statusCheckRollup": null}}}}]}}
            }}
        }}}}}}"#
    )
}

/// The merge mutation's answer: merged at [`HEAD`] into `base` as [`MOVED`].
fn merged_reply(base: &str) -> String {
    format!(
        r#"{{"data": {{"mergePullRequest": {{"pullRequest": {{"number": 7, "state": "MERGED",
            "merged": true, "headRefOid": "{HEAD}", "baseRefName": "{base}",
            "mergeCommit": {{"oid": "{MOVED}"}}}}}}}}}}"#
    )
}

/// Pull request 7 on `server`, open at [`HEAD`], merging with a squash as `behaviour` says.
fn script_merge(server: &GitHubServer, behaviour: MergeBehaviour) {
    script_merge_into(server, "main", behaviour);
}

/// [`script_merge`], with GitHub's answer naming `base` as the base it merged into.
fn script_merge_into(server: &GitHubServer, base: &str, behaviour: MergeBehaviour) {
    let reply = merged_reply(base);
    server.script_merge(MergeScript {
        read: detail_request_body(&pr_target(server), 7, false),
        open: mergeable(HEAD, "OPEN"),
        moved: mergeable(MOVED, "OPEN"),
        merged: mergeable(HEAD, "MERGED"),
        mutation: merge_request_body(NODE, HEAD, MergeMethod::Squash),
        reply,
        behaviour,
    });
}

#[test]
fn pr_merge_without_its_head_or_yes_merges_nothing() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Merge);
    let signed_in = server.requests();
    for args in [
        vec![
            "merge", "7", "--method", "squash", "--base", "main", "--yes",
        ],
        vec!["merge", "7", "--head", HEAD, "--base", "main", "--yes"],
        vec!["merge", "7", "--method", "squash", "--head", HEAD, "--yes"],
        vec![
            "merge", "7", "--method", "squash", "--head", "0123", "--base", "main", "--yes",
        ],
    ] {
        let out = run_pr(&scratch, &repo, &ssh, &args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(
            server.requests(),
            signed_in,
            "{args:?}: a usage error reads nothing"
        );
    }
    let unconfirmed = [
        "merge", "7", "--method", "squash", "--head", HEAD, "--base", "main",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &unconfirmed);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    let lines: Vec<&str> = printed.lines().collect();
    assert!(
        lines[0].starts_with("would-merge\tMerge this pull request on GitHub."),
        "{printed}"
    );
    for line in [
        "would-merge\tAccount: octocat@",
        "would-merge\tBase: \u{201c}main\u{201d}",
        "would-merge\tHead: 0123456 on \u{201c}topic\u{201d}",
        "would-merge\tMethod: a squash merge",
        "would-merge\tTitle: \u{201c}Ship\u{201d}",
    ] {
        assert!(
            lines.iter().any(|l| l.starts_with(line)),
            "{line}\n{printed}"
        );
    }
    assert!(
        lines.iter().all(|l| l.starts_with("would-merge\t")),
        "{printed}"
    );
    assert!(text(&out.stderr).contains("needs --yes"));
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn pr_merge_with_yes_merges_once_and_prints_the_merge_commit() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Merge);
    let args = [
        "merge", "7", "--method", "squash", "--head", HEAD, "--base", "main", "--yes",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &args);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    let merged = format!("merged\tocto/rc-git#7\t{MOVED}\t{HEAD}\tmain\nnote\tMerged #7 into main");
    assert!(printed.starts_with(&merged), "{printed}");
    assert!(printed.contains("unchanged until you fetch"), "{printed}");
    let said = text(&out.stderr);
    assert!(
        said.contains("sending\tMerge this pull request on GitHub.")
            && said.contains("\nsending\tBase: \u{201c}main\u{201d}\n"),
        "the challenge goes to stderr before the request: {said}"
    );
    assert_eq!(server.mutation_requests(), 1);
    assert!(server.merged());
    assert!(!format!("{printed}{}", text(&out.stderr)).contains(TOKEN));
}

#[test]
fn pr_merge_of_a_head_that_moved_refuses_before_any_mutation() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Merge);
    let args = [
        "merge", "7", "--method", "squash", "--head", MOVED, "--base", "main", "--yes",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &args);
    assert_eq!(out.status.code(), Some(11), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("changed since you looked"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn pr_merge_whose_answer_is_lost_exits_12_and_sends_once() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Drop);
    let args = [
        "merge", "7", "--method", "squash", "--head", HEAD, "--base", "main", "--yes",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &args);
    assert_eq!(out.status.code(), Some(12), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(said.contains("could not tell whether #7 merged"), "{said}");
    assert!(said.contains("on GitHub before trying again"), "{said}");
    assert_eq!(server.mutation_requests(), 1);
}

#[test]
fn pr_merge_of_a_base_retargeted_since_it_was_seen_refuses_before_any_mutation() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Merge);
    let args = [
        "merge",
        "7",
        "--method",
        "squash",
        "--head",
        HEAD,
        "--base",
        "experimental",
        "--yes",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &args);
    assert_eq!(out.status.code(), Some(11), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("its base is now 'main', not 'experimental'"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn pr_merge_github_says_landed_on_another_base_exits_13_naming_both() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge_into(&server, "release", MergeBehaviour::Merge);
    let args = [
        "merge", "7", "--method", "squash", "--head", HEAD, "--base", "main", "--yes",
    ];
    let out = run_pr(&scratch, &repo, &ssh, &args);
    assert_eq!(out.status.code(), Some(13), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    assert_eq!(
        printed,
        format!("merged-into-other-base\tocto/rc-git#7\t{MOVED}\t{HEAD}\trelease\tmain\n")
    );
    let said = text(&out.stderr);
    assert!(
        said.contains("Warning: #7 merged into release, not main, the base you confirmed"),
        "{said}"
    );
    assert!(!printed.contains("\nmerged\t") && !printed.starts_with("merged\t"));
    assert_eq!(server.mutation_requests(), 1);
}

/// Pull request 7 on `server`, open at [`HEAD`], blocked by branch protection with the account
/// allowed past it and a review required, merging with a squash as `behaviour` says.
fn script_admin_merge(server: &GitHubServer, behaviour: MergeBehaviour) {
    let blocked = |head: &str, state: &str| {
        mergeable(head, state)
            .replace(
                r#""mergeStateStatus": "CLEAN""#,
                r#""mergeStateStatus": "BLOCKED""#,
            )
            .replace(
                r#""viewerCanMergeAsAdmin": false"#,
                r#""viewerCanMergeAsAdmin": true"#,
            )
            .replace(
                r#""reviewDecision": null"#,
                r#""reviewDecision": "REVIEW_REQUIRED""#,
            )
    };
    let open = blocked(HEAD, "OPEN");
    assert!(open.contains("BLOCKED") && open.contains("REVIEW_REQUIRED") && open.contains("true"));
    let reply = merged_reply("main");
    server.script_merge(MergeScript {
        read: detail_request_body(&pr_target(server), 7, false),
        open,
        moved: blocked(MOVED, "OPEN"),
        merged: mergeable(HEAD, "MERGED"),
        mutation: merge_request_body(NODE, HEAD, MergeMethod::Squash),
        reply,
        behaviour,
    });
}

/// `pr merge 7` of [`HEAD`] with a squash, as an admin merge, with `extra` after.
fn admin_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "merge", "7", "--method", "squash", "--head", HEAD, "--base", "main", "--admin",
    ];
    args.extend_from_slice(extra);
    args
}

/// The fingerprint a dry run of the admin merge printed, from its `plan` record.
fn printed_plan(out: &std::process::Output) -> String {
    let printed = text(&out.stdout);
    let line = printed.lines().find(|line| line.starts_with("plan\t"));
    assert!(line.is_some(), "no plan record: {printed}");
    line.unwrap().trim_start_matches("plan\t").to_owned()
}

/// The plan fingerprint of the admin merge `server` scripts, read by a dry run.
fn dry_run_plan(scratch: &Scratch, repo: &Fixture, ssh: &Fixture) -> String {
    let out = run_pr(scratch, repo, ssh, &admin_args(&[]));
    assert_eq!(out.status.code(), Some(10), "{}", text(&out.stderr));
    printed_plan(&out)
}

#[test]
fn pr_merge_admin_without_its_number_prints_what_it_skips_and_sends_nothing() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_admin_merge(&server, MergeBehaviour::Merge);
    for extra in [&[][..], &["--yes"][..]] {
        let out = run_pr(&scratch, &repo, &ssh, &admin_args(extra));
        assert_eq!(out.status.code(), Some(10), "{}", text(&out.stderr));
        let printed = text(&out.stdout);
        assert!(
            printed.starts_with(
                "would-merge-admin\tMerge this pull request on GitHub past its branch \
                 protection, as an administrator."
            ),
            "{printed}"
        );
        assert!(
            printed.contains("\nwould-merge-admin\tHead: 0123456 on \u{201c}topic\u{201d}\n"),
            "{printed}"
        );
        assert!(
            printed.contains("\nbypass\treview\treview-required\n"),
            "{printed}"
        );
        let plan = printed_plan(&out);
        assert_eq!(plan.len(), 16, "{printed}");
        assert!(
            printed.ends_with(&format!(
                "bypass\tunreported-rules\tany the API does not report\nplan\t{plan}\n"
            )),
            "{printed}"
        );
        assert!(
            text(&out.stderr).contains(&format!("--confirm 7 --plan {plan} (--yes is not enough)")),
            "{}",
            text(&out.stderr)
        );
    }
    let plan = dry_run_plan(&scratch, &repo, &ssh);
    for typed in ["8", "#7", "07", ""] {
        let out = run_pr(
            &scratch,
            &repo,
            &ssh,
            &admin_args(&["--confirm", typed, "--plan", &plan]),
        );
        assert_eq!(
            out.status.code(),
            Some(10),
            "{typed:?}: {}",
            text(&out.stderr)
        );
        assert!(
            text(&out.stderr).contains("type '7' to confirm"),
            "{}",
            text(&out.stderr)
        );
    }
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}

#[test]
fn pr_merge_admin_with_its_number_merges_once() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_admin_merge(&server, MergeBehaviour::Merge);
    let plan = dry_run_plan(&scratch, &repo, &ssh);
    let out = run_pr(
        &scratch,
        &repo,
        &ssh,
        &admin_args(&["--confirm", "7", "--plan", &plan]),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    let merged = format!("merged\tocto/rc-git#7\t{MOVED}\t{HEAD}\tmain\nnote\tMerged #7 into main");
    assert!(printed.starts_with(&merged), "{printed}");
    assert!(
        text(&out.stderr).contains("sending\tSkipped: a required review that has not approved it"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(server.mutation_requests(), 1);
    assert!(server.merged());
    assert!(!format!("{printed}{}", text(&out.stderr)).contains(TOKEN));
}

#[test]
fn pr_merge_admin_of_a_pull_request_nothing_blocks_or_that_github_refuses_merges_nothing() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Merge);
    let confirmed = admin_args(&["--confirm", "7"]);
    let out = run_pr(&scratch, &repo, &ssh, &confirmed);
    assert_eq!(out.status.code(), Some(11), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("no branch protection to merge past; use Merge"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(server.mutation_requests(), 0);

    let refusing = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&refusing, None);
    let body = r#"{"data": {"mergePullRequest": null}, "errors": [{"type": "FORBIDDEN",
        "message": "You're not authorized to bypass branch protections."}]}"#;
    script_admin_merge(
        &refusing,
        MergeBehaviour::Refuse {
            status: "200 OK",
            body: body.to_owned(),
        },
    );
    let plan = dry_run_plan(&scratch, &repo, &ssh);
    let out = run_pr(
        &scratch,
        &repo,
        &ssh,
        &admin_args(&["--confirm", "7", "--plan", &plan]),
    );
    assert_eq!(out.status.code(), Some(11), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(
        said.contains("GitHub refused the admin merge of #7"),
        "{said}"
    );
    assert!(said.contains("has not verified"), "{said}");
    assert_eq!(refusing.mutation_requests(), 1);
    assert!(!format!("{}{said}", text(&out.stdout)).contains(TOKEN));
}

#[test]
fn pr_merge_takes_confirm_only_with_admin() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_merge(&server, MergeBehaviour::Merge);
    let signed_in = server.requests();
    for (flag, value) in [("--confirm", "7"), ("--plan", "0123456789abcdef")] {
        let args = [
            "merge", "7", "--method", "squash", "--head", HEAD, "--base", "main", flag, value,
            "--yes",
        ];
        let out = run_pr(&scratch, &repo, &ssh, &args);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains(&format!("{flag} goes with --admin")),
            "{}",
            text(&out.stderr)
        );
    }
    assert_eq!(server.requests(), signed_in, "a usage error reads nothing");
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn pr_merge_admin_merges_only_the_plan_its_fingerprint_names() {
    let server = server_knowing(TOKEN, "octocat");
    let (scratch, repo, ssh) = pr_setup(&server, None);
    script_admin_merge(&server, MergeBehaviour::Merge);
    let plan = dry_run_plan(&scratch, &repo, &ssh);
    let without = run_pr(&scratch, &repo, &ssh, &admin_args(&["--confirm", "7"]));
    assert_eq!(without.status.code(), Some(2), "{}", text(&without.stderr));
    assert!(
        text(&without.stderr).contains("needs --plan"),
        "{}",
        text(&without.stderr)
    );
    let other = if plan.starts_with('0') { "1" } else { "0" }.repeat(16);
    let out = run_pr(
        &scratch,
        &repo,
        &ssh,
        &admin_args(&["--confirm", "7", "--plan", &other]),
    );
    assert_eq!(out.status.code(), Some(11), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("changed since the plan you were shown"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(server.mutation_requests(), 0);
    assert!(!server.merged());
}
