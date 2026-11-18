//! `rc-git auth`: sign in to GitHub, see the accounts, sign out. The accounts are the app's
//! (its defaults domain) and the tokens its keychain items, so the app and the CLI share both;
//! the CLI is the second writer the token keeper re-reads for. A personal access token is read
//! from standard input only, never from an argument or the environment.

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use rc_git_guard::actions::Cancel;
use rc_git_guard::actions::github::DEV_ALLOWANCE;
use rc_git_kit::app_build::BUNDLE_IDENTIFIER;
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::copy::minutes;
use rc_git_kit::github::tokens::TokenKeeper;
use rc_git_kit::github::{
    ApiToken, ClientId, DeviceMethod, DevicePoll, GitHubHost, TokenSet, refresh_user_token,
    save_sign_in, sign_out, start_device_flow, unix_now, validate_token,
};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore};
use rc_git_kit::platform::settings::SettingsStore;
use rc_git_mac::{KeychainApiTokenStore, Preferences};

use crate::{EXIT_AUTH, EXIT_ERROR, UsageError, VerbArgs, interruptible_cancel, split_args};

/// A test build's hook: the plist, by absolute path, its accounts live in instead of the app's
/// defaults domain.
const SETTINGS_ENV: &str = "RC_GIT_SETTINGS";

/// A test build's hook: the `rc-git-…` keychain service its tokens live under instead of
/// rc-git's own.
const KEYCHAIN_ENV: &str = "RC_GIT_KEYCHAIN_SERVICE";

/// The most bytes read from standard input for a token.
const MAX_TOKEN_INPUT: u64 = 4096;

/// The stores `auth` reads and writes.
struct Stores {
    settings: Arc<dyn SettingsStore>,
    accounts: GitHubAccounts,
    tokens: KeychainApiTokenStore,
}

/// Where the test hooks point the stores; `None` is the app's own.
#[derive(Debug, Default, PartialEq, Eq)]
struct HookTargets {
    settings: Option<String>,
    keychain_service: Option<String>,
}

/// The test hooks `var` names, read only when `dev_allowance` says this build carries the dev
/// allowance (`cargo test` builds). A shipped CLI reads neither variable, so nothing in its
/// environment can point `auth` at other accounts or another keychain service.
fn hook_targets(dev_allowance: bool, var: impl Fn(&str) -> Option<String>) -> HookTargets {
    if !dev_allowance {
        return HookTargets::default();
    }
    HookTargets {
        settings: var(SETTINGS_ENV),
        keychain_service: var(KEYCHAIN_ENV),
    }
}

fn stores() -> Result<Stores> {
    let hooks = hook_targets(DEV_ALLOWANCE, |name| std::env::var(name).ok());
    let settings: Arc<dyn SettingsStore> = match hooks.settings {
        Some(path) if path.starts_with('/') => Arc::new(Preferences::domain(&path)?),
        Some(path) => bail!("{SETTINGS_ENV} '{path}' must be an absolute path"),
        None => Arc::new(Preferences::domain(BUNDLE_IDENTIFIER)?),
    };
    let tokens = match hooks.keychain_service {
        Some(service) => KeychainApiTokenStore::with_service(&service).map_err(|e| anyhow!(e))?,
        None => KeychainApiTokenStore::new(),
    };
    Ok(Stores {
        accounts: GitHubAccounts::new(Arc::clone(&settings)),
        settings,
        tokens,
    })
}

impl Stores {
    /// Makes this process's settings writes visible to the app.
    fn flush(&self) -> Result<()> {
        self.settings
            .synchronize()
            .context("save the GitHub accounts")
    }
}

/// Runs `rc-git auth <args>`.
pub(crate) fn run(args: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let Some((verb, rest)) = args.split_first() else {
        return Err(UsageError("auth needs login, status or logout".to_owned()).into());
    };
    match verb.as_str() {
        "login" => login(rest, out),
        "status" => status(rest, out),
        "logout" => logout(rest, out),
        other => Err(UsageError(format!(
            "auth has no '{other}'; use login, status or logout"
        ))
        .into()),
    }
}

fn host_arg(args: &VerbArgs<'_>) -> Result<GitHubHost> {
    let text = args.value(&["--host"]).unwrap_or("github.com");
    GitHubHost::parse(text).map_err(|error| UsageError(error.to_string()).into())
}

fn login(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args(
        "auth login",
        raw,
        &["--host", "--method", "--client-id"],
        &[],
    )?;
    args.expect(0, 0, "no positional arguments")?;
    let host = host_arg(&args)?;
    let word = args.value(&["--method"]).unwrap_or("github-app");
    let method = SignInMethod::from_word(word).ok_or_else(|| {
        UsageError(format!(
            "--method '{word}': use github-app, oauth-app or token"
        ))
    })?;
    let stores = stores()?;
    let cancel = interruptible_cancel()?;
    let (tokens, client) = match method {
        SignInMethod::Token => {
            if args.value(&["--client-id"]).is_some() {
                return Err(
                    UsageError("--client-id is for github-app and oauth-app".to_owned()).into(),
                );
            }
            (TokenSet::lasting(read_token()?), None)
        }
        SignInMethod::GitHubApp | SignInMethod::OAuthApp => {
            let text = args.value(&["--client-id"]).ok_or_else(|| {
                UsageError(format!(
                    "--method {word} needs --client-id, the app's client ID"
                ))
            })?;
            let client = ClientId::parse(text).map_err(|e| UsageError(e.to_string()))?;
            let device = if method == SignInMethod::GitHubApp {
                DeviceMethod::GitHubApp
            } else {
                DeviceMethod::OAuthApp
            };
            (device_flow(&host, &client, device, &cancel)?, Some(client))
        }
    };
    let identity = validate_token(&host, &tokens.access, &cancel)?;
    let key = AccountKey::new(host, &identity.login).map_err(|e| anyhow!(e))?;
    let account = GitHubAccount {
        key,
        method,
        client_id: client,
        scopes: identity.scopes,
        token_expires: identity.expires,
    };
    save_sign_in(&stores.tokens, &stores.accounts, &account, &tokens).map_err(|e| anyhow!(e))?;
    stores.flush()?;
    writeln!(out, "signed-in\t{}", account_fields(&account))?;
    Ok(ExitCode::SUCCESS)
}

/// The token piped on standard input: everything up to the end, surrounding whitespace
/// trimmed. A terminal is refused, since typing there would show the token on screen.
fn read_token() -> Result<ApiToken> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        bail!(
            "auth login --method token reads the token from standard input; pipe it in \
             (for example: pbpaste | rc-git auth login --method token) rather than typing it"
        );
    }
    let mut text = String::new();
    stdin
        .lock()
        .take(MAX_TOKEN_INPUT)
        .read_to_string(&mut text)
        .context("read the token from standard input")?;
    Ok(ApiToken::new(text.trim().to_owned())?)
}

/// Runs a device flow to its tokens, printing the code and the page first.
fn device_flow(
    host: &GitHubHost,
    client: &ClientId,
    method: DeviceMethod,
    cancel: &Cancel,
) -> Result<TokenSet> {
    let flow = start_device_flow(host, client, method, cancel)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "code\t{}", flow.user_code())?;
    writeln!(stdout, "open\t{}", flow.verification_url())?;
    writeln!(stdout, "expires\t{} minutes", minutes(flow.remaining()))?;
    stdout.flush()?;
    drop(stdout);
    loop {
        match flow.poll(cancel)? {
            DevicePoll::Pending | DevicePoll::SlowedDown { .. } => {}
            DevicePoll::Granted(tokens) => return Ok(tokens),
        }
    }
}

fn status(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("auth status", raw, &["--host"], &[])?;
    args.expect(0, 0, "no positional arguments")?;
    let host = args
        .value(&["--host"])
        .map(|_| host_arg(&args))
        .transpose()?;
    let stores = stores()?;
    let accounts: Vec<GitHubAccount> = stores
        .accounts
        .load()
        .into_iter()
        .filter(|account| host.as_ref().is_none_or(|host| account.key.host() == host))
        .collect();
    if accounts.is_empty() {
        bail!(
            "no GitHub account is signed in{}",
            host.map(|h| format!(" on {h}")).unwrap_or_default()
        );
    }
    let cancel = interruptible_cancel()?;
    let keeper = TokenKeeper::new(&stores.tokens);
    let mut all_ok = true;
    for account in &accounts {
        let state = check(&keeper, account, &cancel);
        all_ok &= state == "ok";
        writeln!(out, "account\t{}\t{state}", account_fields(account))?;
    }
    Ok(if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_AUTH)
    })
}

/// One account's state: `ok`, or what stops it, on one line.
fn check<S: ApiTokenStore>(
    keeper: &TokenKeeper<S>,
    account: &GitHubAccount,
    cancel: &Cancel,
) -> String {
    let host = account.key.host();
    let fresh = keeper.token(account, unix_now(), |refresh| match &account.client_id {
        Some(client) => refresh_user_token(host, client, refresh, cancel),
        None => Err(rc_git_kit::github::GitHubError::RefreshRejected {
            host: host.to_string(),
            code: "no client id".to_owned(),
        }),
    });
    let token = match fresh {
        Ok(fresh) => fresh.token,
        Err(error) => return one_line(&error.to_string()),
    };
    match validate_token(host, &token, cancel) {
        Ok(identity) if identity.login == account.key.login() => "ok".to_owned(),
        Ok(identity) => format!(
            "the token signs in as {}, not {}",
            identity.login,
            account.key.login()
        ),
        Err(error) => one_line(&error.to_string()),
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

fn logout(raw: &[String], out: &mut Vec<u8>) -> Result<ExitCode> {
    let args = split_args("auth logout", raw, &["--host", "--login"], &[])?;
    args.expect(0, 0, "no positional arguments")?;
    let host = host_arg(&args)?;
    let login = args
        .value(&["--login"])
        .ok_or_else(|| UsageError("auth logout needs --login".to_owned()))?;
    let key = AccountKey::new(host, login).map_err(UsageError)?;
    let stores = stores()?;
    if stores.accounts.find(&key).is_none() && stores.tokens.tokens(&key)?.is_none() {
        writeln!(io::stderr().lock(), "error: {key} is not signed in")?;
        return Ok(ExitCode::from(EXIT_ERROR));
    }
    sign_out(&stores.tokens, &stores.accounts, &key).map_err(|e| anyhow!(e))?;
    stores.flush()?;
    writeln!(out, "signed-out\t{}\t{}", key.host(), key.login())?;
    Ok(ExitCode::SUCCESS)
}

/// `host<TAB>login<TAB>method<TAB>scopes<TAB>expires`, `-` for what the host did not say.
fn account_fields(account: &GitHubAccount) -> String {
    let dash = |value: &Option<String>| match value {
        Some(text) if !text.is_empty() => one_line(text),
        Some(_) | None => "-".to_owned(),
    };
    format!(
        "{}\t{}\t{}\t{}\t{}",
        account.key.host(),
        account.key.login(),
        account.method.word(),
        dash(&account.scopes),
        dash(&account.token_expires)
    )
}

#[cfg(test)]
mod tests {
    use rc_git_kit::github::copy::method_label;

    use super::*;

    #[test]
    fn account_fields_keep_one_record_per_line() {
        let account = GitHubAccount {
            key: AccountKey::new(GitHubHost::github_com(), "octocat").unwrap(),
            method: SignInMethod::Token,
            client_id: None,
            scopes: Some("repo,\tevil\nline".to_owned()),
            token_expires: None,
        };
        assert_eq!(
            account_fields(&account),
            "github.com\toctocat\ttoken\trepo, evil line\t-"
        );
        assert_eq!(method_label(account.method), "Personal access token");
    }

    #[test]
    fn a_shipped_build_never_reads_the_test_hooks() {
        let asked = std::cell::Cell::new(0);
        let var = |name: &str| {
            asked.set(asked.get() + 1);
            Some(format!("/scratch/{name}"))
        };
        assert_eq!(hook_targets(false, var), HookTargets::default());
        assert_eq!(asked.get(), 0, "no variable was read");
        assert_eq!(
            hook_targets(true, var),
            HookTargets {
                settings: Some("/scratch/RC_GIT_SETTINGS".to_owned()),
                keychain_service: Some("/scratch/RC_GIT_KEYCHAIN_SERVICE".to_owned()),
            }
        );
    }
}
