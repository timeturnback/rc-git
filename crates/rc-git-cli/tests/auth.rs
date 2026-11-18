//! `rc-git auth` against the testkit's fake GitHub host. Every run points the CLI at a scratch
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

use rc_git_testkit::Fixture;
use rc_git_testkit::github::{DeviceScript, FakeToken, GitHubServer, Grant, PollAnswer, USER_CODE};

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
