//! The words of the GitHub accounts pane and the sign-in sheet, decided here so the app only
//! places them. What each method may do is said before the user signs in.

use std::time::Duration;

use super::accounts::{GitHubAccount, SignInMethod};
use super::sign_in::SignInPhase;

/// The accounts pane's heading.
pub const ACCOUNTS_HEADING: &str = "GitHub Accounts";

/// The note under the heading.
pub const ACCOUNTS_NOTE: &str = "Accounts RCGit uses for pull requests, per host. Tokens stay in \
                                 your login keychain and are sent only to the host they were \
                                 issued for. A repository on a host with several accounts asks \
                                 which one to use.";

/// What the pane says when no account is signed in.
pub const NO_ACCOUNTS: &str = "No GitHub accounts.";

/// The sign-in sheet's title.
pub const SIGN_IN_TITLE: &str = "Sign In to GitHub";

/// The host field's placeholder.
pub const HOST_PLACEHOLDER: &str = "github.com, or your GitHub Enterprise host";

/// The client id field's placeholder.
pub const CLIENT_ID_PLACEHOLDER: &str = "Client ID of the app";

/// The token field's placeholder.
pub const TOKEN_PLACEHOLDER: &str = "Personal access token";

/// The repository permissions a GitHub App needs.
pub const GITHUB_APP_PERMISSIONS: &str = "Pull requests: read and write. Contents: read and \
                                          write. Actions: read and write. Checks: read. Commit \
                                          statuses: read. Metadata: read.";

/// The method's name in the picker and the accounts list.
#[must_use]
pub fn method_label(method: SignInMethod) -> &'static str {
    match method {
        SignInMethod::GitHubApp => "GitHub App",
        SignInMethod::OAuthApp => "OAuth App",
        SignInMethod::Token => "Personal access token",
    }
}

/// What the method may do, shown before sign-in.
#[must_use]
pub fn method_note(method: SignInMethod) -> String {
    match method {
        SignInMethod::GitHubApp => format!(
            "Recommended. Sign in through a GitHub App installed on the repositories you work \
             on. It needs these repository permissions: {GITHUB_APP_PERMISSIONS} Its tokens \
             expire after eight hours and renew themselves."
        ),
        SignInMethod::OAuthApp => "Sign in through an OAuth App. It asks for the repo scope: \
                                   read and write access to every repository your account can \
                                   reach, public and private."
            .to_owned(),
        SignInMethod::Token => format!(
            "Paste a fine-grained personal access token limited to the repositories you need, \
             with these permissions: {GITHUB_APP_PERMISSIONS} RCGit checks it with the host \
             and keeps it in your login keychain."
        ),
    }
}

/// The sheet's message for `phase`.
#[must_use]
pub fn phase_message(phase: &SignInPhase) -> String {
    match phase {
        SignInPhase::Form { note } => note.clone().unwrap_or_default(),
        SignInPhase::RequestingCode => "Asking the host for a code…".to_owned(),
        SignInPhase::WaitingForUser {
            verification_url,
            slowed_to,
            ..
        } => {
            let slower = match slowed_to {
                Some(interval) => format!(
                    " The host asked RCGit to check every {} seconds.",
                    interval.as_secs()
                ),
                None => String::new(),
            };
            format!(
                "Enter this code at {verification_url}, then approve RCGit. This sheet \
                 continues once you have.{slower}"
            )
        }
        SignInPhase::Checking => "Checking the token with the host…".to_owned(),
        SignInPhase::Saving => "Saving the token to your keychain…".to_owned(),
        SignInPhase::SignedIn { account } => signed_in_summary(account),
    }
}

/// What the sheet and the CLI say after a sign-in: the login, the granted scopes, and when the
/// token expires, as the host reported them.
#[must_use]
pub fn signed_in_summary(account: &GitHubAccount) -> String {
    let mut lines = vec![format!(
        "Signed in to {} as {}.",
        account.key.host(),
        account.key.login()
    )];
    lines.push(match &account.scopes {
        Some(scopes) if scopes.is_empty() => "Scopes: none.".to_owned(),
        Some(scopes) => format!("Scopes: {scopes}."),
        None => "Permissions: the app's or the token's own.".to_owned(),
    });
    if let Some(expires) = &account.token_expires {
        lines.push(format!("The token expires {expires}."));
    }
    lines.join(" ")
}

/// The confirm button's label for `method`.
#[must_use]
pub fn confirm_label(method: SignInMethod) -> &'static str {
    match method {
        SignInMethod::GitHubApp | SignInMethod::OAuthApp => "Get Code",
        SignInMethod::Token => "Sign In",
    }
}

/// The button that opens the verification page.
#[must_use]
pub fn open_page_label(verification_url: &str) -> String {
    let shown = verification_url
        .strip_prefix("https://")
        .or_else(|| verification_url.strip_prefix("http://"))
        .unwrap_or(verification_url);
    format!("Open {shown}")
}

/// One row of the accounts list: the login, the host, and the method.
#[must_use]
pub fn account_row(account: &GitHubAccount) -> (String, String, &'static str) {
    (
        account.key.login().to_owned(),
        account.key.host().to_string(),
        method_label(account.method),
    )
}

/// How long a code stays good, for the CLI's prompt.
#[must_use]
pub fn minutes(duration: Duration) -> u64 {
    duration.as_secs().div_ceil(60)
}
