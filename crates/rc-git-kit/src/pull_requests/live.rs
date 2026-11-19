//! The live [`PullRequestService`]: the guard's reads, the app's account settings, and the
//! token keeper over the keychain. The app and the CLI both use it, so they share the accounts,
//! the per-repository choices and the tokens.
//!
//! A merge runs through the guard's `plan_merge_pull_request` and `merge_pull_request`, an
//! auto-merge change through `plan_auto_merge` and `set_auto_merge`, and a re-run through
//! `plan_rerun_failed_jobs` and `rerun_failed_jobs`, each with the account's token and kit's one
//! policy; nothing here retries one. A job log is read through the guard's `read_job_log`, which
//! matches the job to the head on screen before it downloads anything.
//!
//! A pull request's head is fetched like any fetch: over the remote's own git transport, with
//! the git credentials and approved ssh host keys saved for the remote's host
//! ([`LivePullRequests::with_git_credentials`]). The API token is never one of them: it goes
//! only to the API host, in the guard's GitHub client.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rc_git_core::{BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Oid, TreeChange};
use rc_git_guard::actions::github::{
    self as guard, ActionTarget, ApiToken, AutoMergeRequest, JobLogError, JobLogRequest,
    MergeAnswer, MergeRequest, MergeTarget, PullRequestRemote, RerunRequest,
    list_open_pull_requests, merge_pull_request, plan_merge_pull_request, pull_request_base_ref,
    pull_request_remote, read_job_log, read_pull_request, refresh_user_token, unix_now,
};
use rc_git_guard::actions::{PullHeadFetch, PullHeadRequest, RemoteAuth, fetch_pull_request_head};

use super::changes::{ChangesRead, FetchProblem, HeadFetched, read_changes};
use super::copy::{fetch_problem, problem};
use super::flow::{ActionAsk, ActionStop, Stop};
use super::link::{Linked, RepositoryLink, link_repository};
use super::logs::{LogAsk, LogStop};
use super::merge::{MergeAsk, MergeStop, PlanAsk};
use super::{
    ActionPlan, ActionRefusal, AutoMergeChange, AutoMergeOutcome, JobLog, MergeOutcome, MergePlan,
    MergeRefusal, PullRequestService, PullRequestStamp, ReadError, RerunReport,
};
use super::{Cancel, GitHubError, PageCursor, PullRequestDetail, PullRequestPage};
use crate::github::accounts::{GitHubAccount, GitHubAccounts};
use crate::github::tokens::{TokenError, TokenKeeper};
use crate::guard_bridge::{Credentials, FailureKind, classify};
use crate::models::{new_blob, old_blob};
use crate::platform::api_tokens::{AccountKey, ApiTokenStore};
use crate::platform::credentials::{CredentialKind, CredentialStore};
use crate::platform::host_keys::HostKeyStore;
use crate::platform::stores::SharedSettings;
use crate::repository::{LiveRepository, RepositoryReading};
use crate::repository_actions::policy;

/// The git credentials a pull request head fetch may offer: the secrets saved per host for
/// fetch and push, and the approved ssh host keys.
struct GitSecrets {
    store: Arc<dyn CredentialStore>,
    host_keys: Arc<dyn HostKeyStore>,
}

/// The service over the guard, the settings and the token store.
pub struct LivePullRequests {
    accounts: GitHubAccounts,
    keeper: TokenKeeper<Arc<dyn ApiTokenStore>>,
    ssh_dir: Option<PathBuf>,
    git: Option<GitSecrets>,
}

impl std::fmt::Debug for LivePullRequests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivePullRequests")
            .field("ssh_dir", &self.ssh_dir)
            .finish_non_exhaustive()
    }
}

impl LivePullRequests {
    /// The service over the accounts in `settings` and the tokens in `tokens`, reading ssh
    /// aliases from `~/.ssh/config`.
    #[must_use]
    pub fn new(settings: SharedSettings, tokens: Arc<dyn ApiTokenStore>) -> Self {
        Self {
            accounts: GitHubAccounts::new(settings),
            keeper: TokenKeeper::new(tokens),
            ssh_dir: None,
            git: None,
        }
    }

    /// The same, fetching pull request heads with the git credentials in `store` (the ones
    /// fetch and push use, keyed by host) and the ssh host keys approved in `host_keys`.
    /// Without them a fetch goes out with none, as an anonymous `git fetch` would.
    #[must_use]
    pub fn with_git_credentials(
        mut self,
        store: Arc<dyn CredentialStore>,
        host_keys: Arc<dyn HostKeyStore>,
    ) -> Self {
        self.git = Some(GitSecrets { store, host_keys });
        self
    }

    /// The same, reading ssh aliases from `dir/config` instead (the CLI's `--ssh-dir`, tests).
    #[must_use]
    pub fn with_ssh_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.ssh_dir = dir;
        self
    }

    /// [`PullRequestService::link`], with account `login` chosen when several are signed in on
    /// the remote's host: the CLI's `--login`, an explicit choice for one command that is not
    /// saved. A login that is not among them chooses nothing, and the link still asks.
    #[must_use]
    pub fn link_choosing(&self, repo_path: &str, login: &str) -> RepositoryLink {
        let link = self.link(repo_path);
        let RepositoryLink::ChooseAccount { accounts, .. } = &link else {
            return link;
        };
        let Some(key) = accounts.iter().find(|key| key.login() == login) else {
            return link;
        };
        let Ok(remote) = pull_request_remote(Path::new(repo_path)) else {
            return RepositoryLink::Hidden;
        };
        let mut choices = self.accounts.choices();
        choices.insert(repo_path.to_owned(), key.id());
        link_repository(
            remote.as_ref(),
            repo_path,
            &self.accounts.load(),
            &choices,
            self.ssh_dir.as_deref(),
        )
    }

    /// The account's working token, renewed when it expired.
    fn token(&self, account: &GitHubAccount, cancel: &Cancel) -> Result<ApiToken, ReadError> {
        let host = account.key.host();
        let fresh = self
            .keeper
            .token(account, unix_now(), |refresh| match &account.client_id {
                Some(client) => refresh_user_token(host, client, refresh, cancel),
                None => Err(GitHubError::RefreshRejected {
                    host: host.to_string(),
                    code: "no client id".to_owned(),
                }),
            });
        match fresh {
            Ok(fresh) => Ok(fresh.token),
            Err(TokenError::SignedOut { account, .. }) => Err(ReadError::SignedOut { account }),
            Err(TokenError::Store(error)) => Err(ReadError::Failed(error.to_string())),
            Err(TokenError::GitHub(error)) => Err(ReadError::from_github(&error)),
        }
    }
}

impl PullRequestService for LivePullRequests {
    fn link(&self, repo_path: &str) -> RepositoryLink {
        let Ok(remote) = pull_request_remote(Path::new(repo_path)) else {
            return RepositoryLink::Hidden;
        };
        link_repository(
            remote.as_ref(),
            repo_path,
            &self.accounts.load(),
            &self.accounts.choices(),
            self.ssh_dir.as_deref(),
        )
    }

    fn list(
        &self,
        linked: &Linked,
        after: Option<&PageCursor>,
        cancel: &Cancel,
    ) -> Result<PullRequestPage, ReadError> {
        let token = self.token(&linked.account, cancel)?;
        let host = linked.account.key.host();
        list_open_pull_requests(host, &token, &linked.target, after, cancel)
            .map_err(|error| ReadError::from_github(&error))
    }

    fn detail(
        &self,
        linked: &Linked,
        number: u64,
        cancel: &Cancel,
    ) -> Result<PullRequestDetail, ReadError> {
        let token = self.token(&linked.account, cancel)?;
        let host = linked.account.key.host();
        read_pull_request(host, &token, &linked.target, number, cancel)
            .map_err(|error| ReadError::from_github(&error))
    }

    fn choose_account(&self, repo_path: &str, key: &AccountKey) -> Result<(), String> {
        self.accounts
            .choose(repo_path, key)
            .map_err(|error| error.to_string())
    }

    fn changes(
        &self,
        repo_path: &str,
        linked: &Linked,
        stamp: &PullRequestStamp,
        base: Option<Oid>,
    ) -> Result<ChangesRead, String> {
        same_remote(repo_path, linked)?;
        let repository =
            LiveRepository::open(repo_path).map_err(|error| error.message().to_owned())?;
        let tracking = pull_request_base_ref(Path::new(repo_path), &stamp.base_ref)
            .map_err(|error| error.to_string())?;
        read_changes(&repository, stamp, base, tracking)
    }

    fn file_diff(&self, repo_path: &str, change: &TreeChange) -> Result<BlobDiff, String> {
        let repository =
            LiveRepository::open(repo_path).map_err(|error| error.message().to_owned())?;
        repository
            .diff_blobs(
                old_blob(change),
                new_blob(change),
                DEFAULT_BLOB_LIMIT,
                DEFAULT_DIFF_CONTEXT,
            )
            .map_err(|error| error.message().to_owned())
    }

    fn fetch_head(
        &self,
        repo_path: &str,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<HeadFetched, FetchProblem> {
        let remote = same_remote(repo_path, linked).map_err(FetchProblem::Failed)?;
        let host = remote.host();
        let kind = if remote.uses_ssh() {
            CredentialKind::SshPassphrase
        } else {
            CredentialKind::HttpsToken
        };
        let (creds, accepted) = self.git_secrets(host, kind)?;
        let auth = RemoteAuth {
            creds: creds.as_ref(),
            host: Some(host),
            accepted_hostkey: accepted.as_deref(),
            ssh_dir: self.ssh_dir.as_deref(),
        };
        let request = PullHeadRequest {
            number: stamp.number,
            expected: stamp.head_oid,
        };
        let error =
            match fetch_pull_request_head(Path::new(repo_path), policy(), request, auth, cancel) {
                Ok(PullHeadFetch::Fetched { .. }) => return Ok(HeadFetched::Fetched),
                Ok(PullHeadFetch::Moved { found, .. }) => return Ok(HeadFetched::Moved { found }),
                Err(error) => error,
            };
        let message = fetch_problem(&error, host);
        Err(match classify(&error) {
            FailureKind::Cancelled => FetchProblem::Cancelled,
            FailureKind::AuthRequired | FailureKind::PassphraseRequired { .. } => {
                FetchProblem::NeedsSecret(message)
            }
            FailureKind::HostKeyUnknown { .. } => FetchProblem::UnknownHostKey(message),
            FailureKind::IdentityRequired
            | FailureKind::Stale
            | FailureKind::CredentialHostRefused
            | FailureKind::Diverged
            | FailureKind::Failed => FetchProblem::Failed(message),
        })
    }

    fn plan_merge(
        &self,
        linked: &Linked,
        ask: PlanAsk<'_>,
        cancel: &Cancel,
    ) -> Result<MergePlan, MergeStop> {
        let token = self.merge_token(linked, cancel)?;
        let merge = MergeTarget {
            host: linked.account.key.host(),
            token: &token,
            login: linked.account.key.login(),
            target: &linked.target,
            stamp: ask.stamp,
            method: ask.method,
            kind: ask.kind,
        };
        plan_merge_pull_request(policy(), &merge, cancel).map_err(stop)
    }

    fn merge(
        &self,
        linked: &Linked,
        ask: MergeAsk<'_>,
        cancel: &Cancel,
    ) -> Result<MergeOutcome, MergeStop> {
        let token = self.merge_token(linked, cancel)?;
        let request = MergeRequest {
            merge: MergeTarget {
                host: linked.account.key.host(),
                token: &token,
                login: linked.account.key.login(),
                target: &linked.target,
                stamp: ask.stamp,
                method: ask.method,
                kind: ask.kind,
            },
            answer: ask.decision.map(|decision| MergeAnswer {
                decision,
                typed: ask.typed,
            }),
            on_send: ask.on_send,
        };
        merge_pull_request(policy(), request, cancel).map_err(stop)
    }

    fn plan_auto_merge(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        change: AutoMergeChange,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop> {
        let token = self.merge_token(linked, cancel)?;
        let who = action_target(linked, &token, stamp);
        guard::plan_auto_merge(policy(), &who, change, cancel).map_err(action_stop)
    }

    fn set_auto_merge(
        &self,
        linked: &Linked,
        change: AutoMergeChange,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<AutoMergeOutcome, ActionStop> {
        let token = self.merge_token(linked, cancel)?;
        let request = AutoMergeRequest {
            who: action_target(linked, &token, ask.stamp),
            change,
            answer: ask.decision,
            on_send: ask.on_send,
        };
        guard::set_auto_merge(policy(), request, cancel).map_err(action_stop)
    }

    fn plan_rerun(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop> {
        let token = self.merge_token(linked, cancel)?;
        let who = action_target(linked, &token, stamp);
        guard::plan_rerun_failed_jobs(policy(), &who, cancel).map_err(action_stop)
    }

    fn rerun(
        &self,
        linked: &Linked,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<RerunReport, ActionStop> {
        let token = self.merge_token(linked, cancel)?;
        let request = RerunRequest {
            who: action_target(linked, &token, ask.stamp),
            answer: ask.decision,
            on_send: ask.on_send,
        };
        guard::rerun_failed_jobs(policy(), request, cancel).map_err(action_stop)
    }

    fn read_log(
        &self,
        linked: &Linked,
        ask: LogAsk<'_>,
        cancel: &Cancel,
    ) -> Result<JobLog, LogStop> {
        let token = self.token(&linked.account, cancel).map_err(LogStop::Read)?;
        let request = JobLogRequest {
            host: linked.account.key.host(),
            token: &token,
            target: &linked.target,
            head: ask.head,
            run: ask.check.run,
            job: ask.check.job,
            on_matched: ask.on_matched,
        };
        read_job_log(request, cancel).map_err(|error| match error {
            JobLogError::NotMatched { why, .. } => LogStop::NotMatched(why),
            JobLogError::NotFinished { .. } => LogStop::NotFinished,
            JobLogError::Failed(error) => LogStop::Read(ReadError::from_github(&error)),
        })
    }
}

impl LivePullRequests {
    /// The account's token for a guarded action, its failure in that action's terms.
    fn merge_token<R>(&self, linked: &Linked, cancel: &Cancel) -> Result<ApiToken, Stop<R>> {
        self.token(&linked.account, cancel)
            .map_err(|error| match error {
                ReadError::SignedOut { account } => Stop::SignedOut { account },
                ReadError::Cancelled => Stop::Cancelled,
                error @ (ReadError::RateLimited { .. } | ReadError::Failed(_)) => {
                    Stop::Failed(problem(&error, unix_now()))
                }
            })
    }
}

/// The pull request `stamp` names in `linked`'s repository, by `linked`'s account with `token`.
fn action_target<'a>(
    linked: &'a Linked,
    token: &'a ApiToken,
    stamp: &'a PullRequestStamp,
) -> ActionTarget<'a> {
    ActionTarget {
        host: linked.account.key.host(),
        token,
        login: linked.account.key.login(),
        target: &linked.target,
        stamp,
    }
}

/// A guard refusal in a merge's terms.
fn stop(refusal: MergeRefusal) -> MergeStop {
    match refusal {
        MergeRefusal::Cancelled { .. } => MergeStop::Cancelled,
        refusal => MergeStop::Refused(refusal),
    }
}

/// A guard refusal in an auto-merge change's or a re-run's terms.
fn action_stop(refusal: ActionRefusal) -> ActionStop {
    match refusal {
        ActionRefusal::Cancelled { .. } => ActionStop::Cancelled,
        refusal => ActionStop::Refused(refusal),
    }
}

impl LivePullRequests {
    /// The git secret saved for `host` and `kind`, and the ssh host key approved for `host`.
    fn git_secrets(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(Option<Credentials>, Option<String>), FetchProblem> {
        let Some(git) = &self.git else {
            return Ok((None, None));
        };
        let creds = git
            .store
            .credentials(host, kind)
            .map_err(|error| FetchProblem::Failed(error.to_string()))?;
        Ok((creds, git.host_keys.fingerprint(host)))
    }
}

/// The repository's pull request remote, when it is still the one `linked` was read through:
/// a remote changed since would fetch from somewhere the pull request was not read.
fn same_remote(repo_path: &str, linked: &Linked) -> Result<PullRequestRemote, String> {
    let remote = pull_request_remote(Path::new(repo_path))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the repository no longer has a remote".to_owned())?;
    if remote.name() != linked.remote {
        return Err(format!(
            "the repository's pull requests now come from '{}', not '{}'; open the pull \
             request again",
            remote.name(),
            linked.remote
        ));
    }
    Ok(remote)
}
