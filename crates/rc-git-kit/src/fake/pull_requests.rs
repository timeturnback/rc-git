//! A [`PullRequestService`] that answers from scripted values and counts its calls. It answers
//! a read only for the exact arguments a correct caller sends: the link it handed out and a
//! cursor it scripted. Anything else is a failure naming what arrived, so a model that reads
//! the wrong repository or page fails its test.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rc_git_core::Oid;
use rc_git_guard::actions::github::GitHubHost;

use super::CallGate;
use crate::github::accounts::{GitHubAccount, SignInMethod};
use crate::platform::api_tokens::AccountKey;
use crate::pull_requests::link::{Linked, RepositoryLink};
use crate::pull_requests::{
    Cancel, CheckList, DetailFields, MergeMethods, Mergeable, PageCursor, PullRequestDetail,
    PullRequestPage, PullRequestService, PullRequestState, PullRequestSummary, ReadError,
    RepositoryTarget,
};

/// What the fake answers and what it was asked.
#[derive(Debug)]
pub struct FakePullRequestsState {
    /// What [`PullRequestService::link`] answers.
    pub link: RepositoryLink,
    /// The list answers, by the cursor text they answer for (`None`: the first page).
    pub pages: BTreeMap<Option<String>, Result<PullRequestPage, ReadError>>,
    /// The detail answers, by number.
    pub details: BTreeMap<u64, Result<PullRequestDetail, ReadError>>,
    /// Holds the link reads at these gates, one each in arrival order; a read with no gate
    /// left passes. The answer is taken on arrival, before the gate.
    pub link_gates: VecDeque<Arc<CallGate>>,
    /// Holds each list read at the gate, when set.
    pub list_gate: Option<Arc<CallGate>>,
    /// Holds each detail read at the gate, when set.
    pub detail_gate: Option<Arc<CallGate>>,
    /// How many link reads arrived.
    pub link_calls: usize,
    /// The cursor of every list read, in arrival order.
    pub list_calls: Vec<Option<String>>,
    /// The number of every detail read, in arrival order.
    pub detail_calls: Vec<u64>,
    /// Every account choice saved, as `(repo_path, key id)`.
    pub chosen: Vec<(String, String)>,
}

/// The fake service; clones share one state.
#[derive(Debug, Clone)]
pub struct FakePullRequests {
    state: Arc<Mutex<FakePullRequestsState>>,
}

impl FakePullRequests {
    /// A fake linked to [`linked`] with no page or detail scripted.
    #[must_use]
    pub fn new() -> Self {
        Self::with_link(RepositoryLink::Linked(linked()))
    }

    /// A fake that answers `link`.
    #[must_use]
    pub fn with_link(link: RepositoryLink) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakePullRequestsState {
                link,
                link_gates: VecDeque::new(),
                pages: BTreeMap::new(),
                details: BTreeMap::new(),
                list_gate: None,
                detail_gate: None,
                link_calls: 0,
                list_calls: Vec::new(),
                detail_calls: Vec::new(),
                chosen: Vec::new(),
            })),
        }
    }

    /// The shared state, to script answers and read the calls.
    pub fn state(&self) -> MutexGuard<'_, FakePullRequestsState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn expected(&self, linked: &Linked) -> Result<(), ReadError> {
        match &self.state().link {
            RepositoryLink::Linked(expected) if expected == linked => Ok(()),
            _ => Err(ReadError::Failed(format!(
                "the fake has no answer for {}/{}",
                linked.target.owner(),
                linked.target.name()
            ))),
        }
    }
}

impl Default for FakePullRequests {
    fn default() -> Self {
        Self::new()
    }
}

impl PullRequestService for FakePullRequests {
    fn link(&self, _repo_path: &str) -> RepositoryLink {
        let (answer, gate) = {
            let mut state = self.state();
            state.link_calls += 1;
            (state.link.clone(), state.link_gates.pop_front())
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        answer
    }

    fn list(
        &self,
        linked: &Linked,
        after: Option<&PageCursor>,
        _cancel: &Cancel,
    ) -> Result<PullRequestPage, ReadError> {
        let key = after.map(|cursor| cursor.as_str().to_owned());
        let gate = {
            let mut state = self.state();
            state.list_calls.push(key.clone());
            state.list_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        self.expected(linked)?;
        self.state().pages.get(&key).cloned().unwrap_or_else(|| {
            Err(ReadError::Failed(format!(
                "the fake has no page after {key:?}"
            )))
        })
    }

    fn detail(
        &self,
        linked: &Linked,
        number: u64,
        _cancel: &Cancel,
    ) -> Result<PullRequestDetail, ReadError> {
        let gate = {
            let mut state = self.state();
            state.detail_calls.push(number);
            state.detail_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        self.expected(linked)?;
        self.state()
            .details
            .get(&number)
            .cloned()
            .unwrap_or_else(|| {
                Err(ReadError::Failed(format!(
                    "the fake has no pull request {number}"
                )))
            })
    }

    fn choose_account(&self, repo_path: &str, key: &AccountKey) -> Result<(), String> {
        self.state().chosen.push((repo_path.to_owned(), key.id()));
        Ok(())
    }
}

/// `octocat` on github.com reading `octo/rc-git` from `origin`.
///
/// # Panics
/// Never: the values are fixed and valid.
#[must_use]
pub fn linked() -> Linked {
    #[expect(clippy::expect_used, reason = "fixed, valid values in a test helper")]
    let key = AccountKey::new(GitHubHost::github_com(), "octocat").expect("a valid login");
    #[expect(clippy::expect_used, reason = "fixed, valid values in a test helper")]
    let target =
        RepositoryTarget::on_host(&GitHubHost::github_com(), "https://github.com/octo/rc-git")
            .expect("a valid remote");
    Linked {
        account: GitHubAccount {
            key,
            method: SignInMethod::Token,
            client_id: None,
            scopes: None,
            token_expires: None,
        },
        target,
        remote: "origin".to_owned(),
    }
}

/// An open pull request `number` titled `title`, its head at `head`, with no checks.
#[must_use]
pub fn pull_request(number: u64, title: &str, head: Oid) -> PullRequestSummary {
    PullRequestSummary {
        number,
        title: title.to_owned(),
        author: Some("mona".to_owned()),
        head_ref: "feature".to_owned(),
        head_oid: head,
        base_ref: "main".to_owned(),
        head_owner: Some("octo".to_owned()),
        cross_repository: false,
        draft: false,
        state: PullRequestState::Open,
        updated_at: Some(1_794_992_400),
        checks: None,
        review: None,
    }
}

/// `summary` in full, with `body` and `checks`.
#[must_use]
pub fn pull_request_detail(
    summary: PullRequestSummary,
    body: &str,
    checks: CheckList,
) -> PullRequestDetail {
    PullRequestDetail {
        summary,
        body: body.to_owned(),
        url: None,
        mergeable: Mergeable::Mergeable,
        merge_state: None,
        merge_methods: MergeMethods {
            merge: true,
            squash: true,
            rebase: true,
        },
        auto_merge_allowed: None,
        viewer: None,
        auto_merge: None,
        checks,
        fields: DetailFields::Full,
    }
}

/// A page holding `pull_requests`, with `next` as its cursor.
///
/// # Panics
/// When `next` is not a valid cursor (1 to 256 printable ASCII characters).
#[must_use]
pub fn page(pull_requests: Vec<PullRequestSummary>, next: Option<&str>) -> PullRequestPage {
    #[expect(clippy::expect_used, reason = "a test helper given a fixed cursor")]
    let next = next.map(|text| PageCursor::from_text(text).expect("a valid cursor"));
    PullRequestPage {
        pull_requests,
        next,
    }
}
