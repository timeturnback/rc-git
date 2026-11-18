//! The sign-in sheet's state machine and its words, driven with the answers the guard would
//! give. A device flow is a real one, started against testkit's fake GitHub host, since the
//! state machine owns it and must free its one slot per process.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rc_git_guard::actions::Cancel;
use rc_git_kit::github::accounts::{GitHubAccount, SignInMethod};
use rc_git_kit::github::copy::{method_note, phase_message, signed_in_summary};
use rc_git_kit::github::sign_in::{SignInFlow, SignInPhase, SignInStart, SignInStep};
use rc_git_kit::github::{
    ApiToken, ClientId, DeviceFlow, DeviceMethod, DevicePoll, GitHubError, GitHubHost,
    TokenIdentity, TokenSet, start_device_flow,
};
use rc_git_kit::platform::api_tokens::AccountKey;
use rc_git_testkit::github::{DeviceScript, GitHubServer, USER_CODE};

const CLIENT: &str = "Iv23li.client";

/// Device flows hold a process-wide slot, so the tests that start one run one at a time.
static DEVICE_FLOWS: Mutex<()> = Mutex::new(());

/// A fake host answering the device-code endpoint for [`CLIENT`], held with the lock that
/// keeps other tests' flows out.
struct Fake {
    server: GitHubServer,
    _one: MutexGuard<'static, ()>,
}

impl Fake {
    fn start() -> Self {
        let one = DEVICE_FLOWS.lock().unwrap_or_else(PoisonError::into_inner);
        let server = GitHubServer::start();
        server.script_device_flow(DeviceScript {
            client_id: CLIENT.to_owned(),
            scope: None,
            verification_uri: None,
            interval: 1,
            expires_in: 900,
            polls: VecDeque::new(),
        });
        Self { server, _one: one }
    }

    fn host(&self) -> GitHubHost {
        GitHubHost::parse(&self.server.host()).unwrap()
    }

    fn device_start(&self) -> SignInStart {
        SignInStart::from_form(&self.server.host(), SignInMethod::GitHubApp, CLIENT, "").unwrap()
    }

    /// What `SignInStep::RequestCode` runs; fails while another flow holds the slot.
    fn start_flow(&self) -> Result<DeviceFlow, GitHubError> {
        let client = ClientId::parse(CLIENT).unwrap();
        start_device_flow(
            &self.host(),
            &client,
            DeviceMethod::GitHubApp,
            &Cancel::new(),
        )
    }
}

fn device_start() -> SignInStart {
    SignInStart::from_form("github.com", SignInMethod::GitHubApp, CLIENT, "").unwrap()
}

fn identity() -> TokenIdentity {
    TokenIdentity {
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    }
}

fn granted() -> TokenSet {
    TokenSet::lasting(ApiToken::new("ghu_granted".to_owned()).unwrap())
}

/// Drives a device flow up to the code on screen, answering under its generation. Fails when
/// the process's one device-flow slot is still held.
fn waiting(flow: &mut SignInFlow, fake: &Fake) -> u64 {
    let step = flow.submit(fake.device_start());
    let generation = match step {
        SignInStep::RequestCode { generation } => generation,
        other => {
            assert_eq!(other, SignInStep::RequestCode { generation: 0 });
            0
        }
    };
    let device = fake.start_flow().unwrap();
    assert_eq!(
        flow.code_shown(generation, Ok(device)),
        SignInStep::Poll { generation }
    );
    assert_eq!(
        flow.phase(),
        &SignInPhase::WaitingForUser {
            user_code: USER_CODE.to_owned(),
            verification_url: format!("{}/login/device", fake.server.origin()),
            slowed_to: None,
        }
    );
    assert!(flow.device().is_some(), "the flow holds the device to poll");
    generation
}

#[test]
fn a_device_sign_in_polls_through_pending_and_slow_down_to_a_saved_account() {
    let fake = Fake::start();
    let mut flow = SignInFlow::new();
    let generation = waiting(&mut flow, &fake);
    assert!(flow.can_cancel());
    assert_eq!(
        flow.polled(generation, Ok(DevicePoll::Pending)),
        SignInStep::Poll { generation }
    );
    let slower = DevicePoll::SlowedDown {
        interval: Duration::from_secs(10),
    };
    assert_eq!(
        flow.polled(generation, Ok(slower)),
        SignInStep::Poll { generation }
    );
    assert!(phase_message(flow.phase()).contains("every 10 seconds"));
    let step = flow.polled(generation, Ok(DevicePoll::Granted(granted())));
    assert_eq!(
        step,
        SignInStep::Check {
            generation,
            token: granted().access
        }
    );
    assert!(flow.device().is_none(), "no poll follows a grant");
    let SignInStep::Save {
        account, tokens, ..
    } = flow.checked(generation, Ok(identity()))
    else {
        panic!("a checked token is saved");
    };
    assert_eq!(tokens, granted());
    assert_eq!(account.method, SignInMethod::GitHubApp);
    assert_eq!(account.client_id, Some(ClientId::parse(CLIENT).unwrap()));
    assert_eq!(account.key.host(), &fake.host());
    assert_eq!(account.key.login(), "octocat");
    assert!(!flow.can_cancel(), "the save is not cancelled halfway");
    assert!(!flow.cancel());
    assert_eq!(
        flow.saved(generation, account.clone(), Ok(())),
        SignInStep::Idle
    );
    assert_eq!(flow.phase(), &SignInPhase::SignedIn { account });
}

#[test]
fn a_pasted_token_is_checked_then_saved_without_a_client_id() {
    let mut flow = SignInFlow::new();
    let start =
        SignInStart::from_form("ghe.example.com", SignInMethod::Token, "", " ghp_x\n").unwrap();
    let SignInStep::Check { generation, token } = flow.submit(start) else {
        panic!("a token start checks the token");
    };
    assert_eq!(token.expose_secret(), "ghp_x");
    let SignInStep::Save { account, .. } = flow.checked(generation, Ok(identity())) else {
        panic!("a checked token is saved");
    };
    assert_eq!(
        (account.method, account.client_id),
        (SignInMethod::Token, None)
    );
    assert_eq!(
        account.key.host(),
        &GitHubHost::parse("ghe.example.com").unwrap()
    );
}

#[test]
fn cancel_returns_to_the_form_and_drops_every_late_answer() {
    let fake = Fake::start();
    let mut flow = SignInFlow::new();
    let generation = waiting(&mut flow, &fake);
    assert!(flow.cancel());
    assert!(flow.device().is_none(), "a cancel frees the device flow");
    assert_eq!(
        flow.phase(),
        &SignInPhase::Form {
            note: Some("Sign-in cancelled.".to_owned())
        }
    );
    assert_eq!(
        flow.polled(generation, Ok(DevicePoll::Granted(granted()))),
        SignInStep::Idle
    );
    assert_eq!(flow.checked(generation, Ok(identity())), SignInStep::Idle);
    assert!(matches!(flow.phase(), SignInPhase::Form { .. }));
    assert!(!flow.cancel(), "nothing runs on the form");

    let restarted = waiting(&mut flow, &fake);
    assert_ne!(restarted, generation);
    assert_eq!(
        flow.polled(generation, Ok(DevicePoll::Pending)),
        SignInStep::Idle,
        "an answer from the cancelled run does not move the new one"
    );
    assert!(flow.device().is_some(), "nor drops its device flow");
}

#[test]
fn a_device_flow_that_lands_after_a_cancel_is_dropped() {
    let fake = Fake::start();
    let mut flow = SignInFlow::new();
    let SignInStep::RequestCode { generation } = flow.submit(fake.device_start()) else {
        panic!("a device start asks for a code");
    };
    assert!(flow.cancel());
    let late = fake.start_flow().unwrap();
    assert_eq!(flow.code_shown(generation, Ok(late)), SignInStep::Idle);
    assert!(flow.device().is_none());
    fake.start_flow()
        .expect("the late flow's slot is free again");
}

#[test]
fn a_failed_poll_or_token_check_frees_the_device_flow_for_a_retry() {
    let fake = Fake::start();
    let mut flow = SignInFlow::new();
    let generation = waiting(&mut flow, &fake);
    let denied = GitHubError::AccessDenied {
        host: fake.server.host(),
    };
    assert_eq!(flow.polled(generation, Err(denied)), SignInStep::Idle);
    assert!(flow.device().is_none(), "no poll follows a failed one");

    let retried = waiting(&mut flow, &fake);
    let step = flow.polled(retried, Ok(DevicePoll::Granted(granted())));
    assert!(matches!(step, SignInStep::Check { .. }), "{step:?}");
    let refused = GitHubError::Unauthorized {
        op: "check the token",
        url: format!("{}/api/v3/user", fake.server.origin()),
        host: fake.server.host(),
    };
    assert_eq!(flow.checked(retried, Err(refused)), SignInStep::Idle);
    assert!(matches!(flow.phase(), SignInPhase::Form { note: Some(_) }));
    waiting(&mut flow, &fake);
}

#[test]
fn a_declined_expired_or_foreign_flow_ends_on_the_form_with_its_reason() {
    let fake = Fake::start();
    let host = fake.server.host();
    for error in [
        GitHubError::AccessDenied { host: host.clone() },
        GitHubError::DeviceCodeExpired { host: host.clone() },
    ] {
        let mut flow = SignInFlow::new();
        let generation = waiting(&mut flow, &fake);
        let message = error.to_string();
        assert_eq!(flow.polled(generation, Err(error)), SignInStep::Idle);
        assert_eq!(
            flow.phase(),
            &SignInPhase::Form {
                note: Some(message)
            }
        );
    }
    let mut flow = SignInFlow::new();
    let SignInStep::RequestCode { generation } = flow.submit(device_start()) else {
        panic!("a device start asks for a code");
    };
    let foreign = GitHubError::ForeignVerificationUri {
        host,
        uri: "https://evil.example/login/device".to_owned(),
        expected: "https://github.com/login/device".to_owned(),
    };
    assert_eq!(flow.code_shown(generation, Err(foreign)), SignInStep::Idle);
    let SignInPhase::Form { note: Some(note) } = flow.phase() else {
        panic!("the refusal is shown on the form");
    };
    assert!(note.contains("evil.example"), "{note}");
}

#[test]
fn a_submit_while_running_starts_nothing() {
    let fake = Fake::start();
    let mut flow = SignInFlow::new();
    waiting(&mut flow, &fake);
    assert_eq!(flow.submit(fake.device_start()), SignInStep::Idle);
}

#[test]
fn the_form_names_what_is_missing() {
    let missing = SignInStart::from_form("github.com", SignInMethod::OAuthApp, " ", "");
    assert!(missing.unwrap_err().contains("client ID"));
    let host = SignInStart::from_form("https://github.com", SignInMethod::GitHubApp, "Iv1.x", "");
    assert!(host.unwrap_err().contains("not a GitHub host"));
    let token = SignInStart::from_form("github.com", SignInMethod::Token, "", "ghp_a b");
    let problem = token.unwrap_err();
    assert!(problem.contains("no spaces"), "{problem}");
    assert!(!problem.contains("ghp_a"), "{problem}");
    let oauth = SignInStart::from_form("github.com", SignInMethod::OAuthApp, "Ov23.x", "").unwrap();
    assert!(matches!(
        oauth,
        SignInStart::Device {
            method: DeviceMethod::OAuthApp,
            ..
        }
    ));
}

#[test]
fn settings_says_what_each_method_may_do_before_sign_in() {
    let app = method_note(SignInMethod::GitHubApp);
    for permission in [
        "Pull requests: read and write",
        "Contents: read and write",
        "Actions: read and write",
        "Checks: read",
        "Commit statuses: read",
        "Metadata: read",
    ] {
        assert!(app.contains(permission), "{app}");
    }
    assert!(method_note(SignInMethod::OAuthApp).contains("repo scope"));
    assert!(method_note(SignInMethod::Token).contains("fine-grained"));
}

#[test]
fn after_sign_in_the_login_scopes_and_expiry_are_shown() {
    let account = GitHubAccount {
        key: AccountKey::new(GitHubHost::github_com(), "octocat").unwrap(),
        method: SignInMethod::Token,
        client_id: None,
        scopes: Some("repo, read:org".to_owned()),
        token_expires: Some("2026-12-01 10:00:00 UTC".to_owned()),
    };
    assert_eq!(
        signed_in_summary(&account),
        "Signed in to github.com as octocat. Scopes: repo, read:org. The token expires \
         2026-12-01 10:00:00 UTC."
    );
    let app = GitHubAccount {
        scopes: None,
        token_expires: None,
        ..account
    };
    assert_eq!(
        signed_in_summary(&app),
        "Signed in to github.com as octocat. Permissions: the app's or the token's own."
    );
}
