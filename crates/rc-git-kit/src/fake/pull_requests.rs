//! A [`PullRequestService`] that answers from scripted values and counts its calls. It answers
//! a read only for the exact arguments a correct caller sends: the link it handed out and a
//! cursor it scripted. Anything else is a failure naming what arrived, so a model that reads
//! the wrong repository or page fails its test.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rc_git_core::{BlobDiff, Oid, TreeChange};
use rc_git_guard::actions::github::GitHubHost;

use super::CallGate;
use crate::github::accounts::{GitHubAccount, SignInMethod};
use crate::platform::api_tokens::AccountKey;
use crate::pull_requests::changes::{ChangesRead, FetchProblem, HeadFetched};
use crate::pull_requests::flow::{ActionAsk, ActionStop};
use crate::pull_requests::link::{Linked, RepositoryLink};
use crate::pull_requests::logs::{LogAsk, LogCheck, LogStop};
use crate::pull_requests::merge::{MergeAsk, MergeStop, PlanAsk};
use crate::pull_requests::{
    ActionPlan, ActionsIds, AutoMerge, AutoMergeChange, AutoMergeOutcome, Cancel, Check,
    CheckConclusion, CheckList, CheckProgress, CheckStatus, Decision, DetailFields, JobLog,
    MergeKind, MergeMethod, MergeMethods, MergeOutcome, MergePlan, MergeStateStatus, Mergeable,
    PageCursor, PullRequestDetail, PullRequestId, PullRequestPage, PullRequestService,
    PullRequestStamp, PullRequestState, PullRequestSummary, ReadError, RepositoryTarget,
    RerunReport, ViewerAbilities,
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
    /// What [`PullRequestService::changes`] answers, by the head it is asked for; answers are
    /// taken in order, the last one repeating.
    pub changes: BTreeMap<Oid, VecDeque<Result<ChangesRead, String>>>,
    /// What [`PullRequestService::fetch_head`] answers, by the head it is asked to fetch.
    pub fetches: BTreeMap<Oid, Result<HeadFetched, FetchProblem>>,
    /// What [`PullRequestService::file_diff`] answers, by the changed path.
    pub file_diffs: BTreeMap<String, Result<BlobDiff, String>>,
    /// Holds each fetch at the gate, when set.
    pub fetch_gate: Option<Arc<CallGate>>,
    /// How many detail reads have answered, counted just before each returns.
    pub details_answered: usize,
    /// The stamp of every changes read, in arrival order.
    pub changes_calls: Vec<PullRequestStamp>,
    /// The base commit every changes read compared with, in arrival order.
    pub changes_bases: Vec<Option<Oid>>,
    /// The stamp of every fetch, in arrival order.
    pub fetch_calls: Vec<PullRequestStamp>,
    /// The path of every file diff, in arrival order.
    pub file_diff_calls: Vec<String>,
    /// What [`PullRequestService::plan_merge`] answers.
    pub plan: Result<MergePlan, MergeStop>,
    /// What [`PullRequestService::merge`] answers once its request is sent.
    pub merge: Result<MergeOutcome, MergeStop>,
    /// Holds each plan read at the gate, when set.
    pub plan_gate: Option<Arc<CallGate>>,
    /// Holds each merge at the gate before its request is sent, when set; a cancel raised
    /// meanwhile sends nothing.
    pub unsent_gate: Option<Arc<CallGate>>,
    /// Holds each merge at the gate after its request is sent, when set.
    pub sent_gate: Option<Arc<CallGate>>,
    /// The stamp, method and kind of every plan read, in arrival order.
    pub plan_calls: Vec<(PullRequestStamp, MergeMethod, MergeKind)>,
    /// Every merge asked for, in arrival order.
    pub merge_calls: Vec<MergeCall>,
    /// How many merge requests were sent: a merge's `on_send` and the sending itself.
    pub merges_sent: usize,
    /// What [`PullRequestService::plan_auto_merge`] and [`PullRequestService::plan_rerun`]
    /// answer.
    pub action_plan: Result<ActionPlan, ActionStop>,
    /// What [`PullRequestService::set_auto_merge`] answers once its request is sent.
    pub auto_merge: Result<AutoMergeOutcome, ActionStop>,
    /// What [`PullRequestService::rerun`] answers once its requests are sent.
    pub rerun: Result<RerunReport, ActionStop>,
    /// The stamp and the change of every auto-merge or re-run plan read (`None`: a re-run), in
    /// arrival order.
    pub action_plan_calls: Vec<(PullRequestStamp, Option<AutoMergeChange>)>,
    /// Every auto-merge change or re-run asked for, in arrival order.
    pub action_calls: Vec<FlowCall>,
    /// How many auto-merge changes or re-runs were sent.
    pub actions_sent: usize,
    /// What [`PullRequestService::read_log`] answers once the job matched.
    pub log: Result<JobLog, LogStop>,
    /// Holds each log read at the gate before its job matches, when set; a cancel raised
    /// meanwhile ends it cancelled.
    pub log_gate: Option<Arc<CallGate>>,
    /// The head and check of every log read, in arrival order.
    pub log_calls: Vec<(Oid, LogCheck)>,
}

/// One auto-merge change or re-run the fake was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowCall {
    /// What the user saw.
    pub stamp: PullRequestStamp,
    /// The auto-merge change; `None` for a re-run.
    pub change: Option<AutoMergeChange>,
    /// The decision the user answered.
    pub decision: Option<Decision>,
}

/// One merge the fake was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCall {
    /// What the user saw.
    pub stamp: PullRequestStamp,
    /// The method.
    pub method: MergeMethod,
    /// The plain merge or the admin merge.
    pub kind: MergeKind,
    /// The decision the user answered.
    pub decision: Option<Decision>,
    /// What the user typed, as it arrived.
    pub typed: String,
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
                changes: BTreeMap::new(),
                fetches: BTreeMap::new(),
                file_diffs: BTreeMap::new(),
                fetch_gate: None,
                details_answered: 0,
                changes_calls: Vec::new(),
                changes_bases: Vec::new(),
                fetch_calls: Vec::new(),
                file_diff_calls: Vec::new(),
                plan: Err(MergeStop::Failed(
                    "the fake has no plan scripted".to_owned(),
                )),
                merge: Err(MergeStop::Failed(
                    "the fake has no merge scripted".to_owned(),
                )),
                plan_gate: None,
                unsent_gate: None,
                sent_gate: None,
                plan_calls: Vec::new(),
                merge_calls: Vec::new(),
                merges_sent: 0,
                action_plan: Err(ActionStop::Failed(
                    "the fake has no plan scripted".to_owned(),
                )),
                auto_merge: Err(ActionStop::Failed(
                    "the fake has no auto-merge scripted".to_owned(),
                )),
                rerun: Err(ActionStop::Failed(
                    "the fake has no re-run scripted".to_owned(),
                )),
                action_plan_calls: Vec::new(),
                action_calls: Vec::new(),
                actions_sent: 0,
                log: Err(LogStop::Read(ReadError::Failed(
                    "the fake has no log scripted".to_owned(),
                ))),
                log_gate: None,
                log_calls: Vec::new(),
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
        let answer = self.expected(linked).and_then(|()| {
            self.state()
                .details
                .get(&number)
                .cloned()
                .unwrap_or_else(|| {
                    Err(ReadError::Failed(format!(
                        "the fake has no pull request {number}"
                    )))
                })
        });
        self.state().details_answered += 1;
        answer
    }

    fn choose_account(&self, repo_path: &str, key: &AccountKey) -> Result<(), String> {
        self.state().chosen.push((repo_path.to_owned(), key.id()));
        Ok(())
    }

    fn changes(
        &self,
        _repo_path: &str,
        linked: &Linked,
        stamp: &PullRequestStamp,
        base: Option<Oid>,
    ) -> Result<ChangesRead, String> {
        self.state().changes_calls.push(stamp.clone());
        self.state().changes_bases.push(base);
        self.expected(linked)
            .map_err(|error| format!("{error:?}"))?;
        let mut state = self.state();
        let Some(answers) = state.changes.get_mut(&stamp.head_oid) else {
            return Err(format!("the fake has no changes for {}", stamp.head_oid));
        };
        if answers.len() > 1 {
            return answers
                .pop_front()
                .unwrap_or_else(|| Err("no answer".to_owned()));
        }
        answers
            .front()
            .cloned()
            .unwrap_or_else(|| Err("no answer".to_owned()))
    }

    fn file_diff(&self, _repo_path: &str, change: &TreeChange) -> Result<BlobDiff, String> {
        let path = change.path.to_string();
        let mut state = self.state();
        state.file_diff_calls.push(path.clone());
        state
            .file_diffs
            .get(&path)
            .cloned()
            .unwrap_or_else(|| Err(format!("the fake has no diff of {path}")))
    }

    fn fetch_head(
        &self,
        _repo_path: &str,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<HeadFetched, FetchProblem> {
        let gate = {
            let mut state = self.state();
            state.fetch_calls.push(stamp.clone());
            state.fetch_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        if cancel.is_cancelled() {
            return Err(FetchProblem::Cancelled);
        }
        self.expected(linked)
            .map_err(|error| FetchProblem::Failed(format!("{error:?}")))?;
        self.state()
            .fetches
            .get(&stamp.head_oid)
            .cloned()
            .unwrap_or_else(|| {
                Err(FetchProblem::Failed(format!(
                    "the fake has no fetch of {}",
                    stamp.head_oid
                )))
            })
    }

    fn plan_merge(
        &self,
        linked: &Linked,
        ask: PlanAsk<'_>,
        cancel: &Cancel,
    ) -> Result<MergePlan, MergeStop> {
        let gate = {
            let mut state = self.state();
            state
                .plan_calls
                .push((ask.stamp.clone(), ask.method, ask.kind));
            state.plan_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        if cancel.is_cancelled() {
            return Err(MergeStop::Cancelled);
        }
        self.expected(linked)
            .map_err(|error| MergeStop::Failed(format!("{error:?}")))?;
        self.state().plan.clone()
    }

    fn merge(
        &self,
        linked: &Linked,
        ask: MergeAsk<'_>,
        cancel: &Cancel,
    ) -> Result<MergeOutcome, MergeStop> {
        let gate = {
            let mut state = self.state();
            state.merge_calls.push(MergeCall {
                stamp: ask.stamp.clone(),
                method: ask.method,
                kind: ask.kind,
                decision: ask.decision.cloned(),
                typed: ask.typed.to_owned(),
            });
            state.unsent_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        if cancel.is_cancelled() {
            return Err(MergeStop::Cancelled);
        }
        self.expected(linked)
            .map_err(|error| MergeStop::Failed(format!("{error:?}")))?;
        if ask.decision.is_none() {
            return Err(MergeStop::Failed("the fake got no confirmation".to_owned()));
        }
        (ask.on_send)();
        let gate = {
            let mut state = self.state();
            state.merges_sent += 1;
            state.sent_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        self.state().merge.clone()
    }

    fn plan_auto_merge(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        change: AutoMergeChange,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop> {
        self.action_plan(linked, stamp, Some(change), cancel)
    }

    fn set_auto_merge(
        &self,
        linked: &Linked,
        change: AutoMergeChange,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<AutoMergeOutcome, ActionStop> {
        self.send_action(linked, Some(change), ask, cancel)?;
        self.state().auto_merge.clone()
    }

    fn plan_rerun(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop> {
        self.action_plan(linked, stamp, None, cancel)
    }

    fn rerun(
        &self,
        linked: &Linked,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<RerunReport, ActionStop> {
        self.send_action(linked, None, ask, cancel)?;
        self.state().rerun.clone()
    }

    fn read_log(
        &self,
        linked: &Linked,
        ask: LogAsk<'_>,
        cancel: &Cancel,
    ) -> Result<JobLog, LogStop> {
        let gate = {
            let mut state = self.state();
            state.log_calls.push((ask.head, ask.check.clone()));
            state.log_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        if cancel.is_cancelled() {
            return Err(LogStop::Read(ReadError::Cancelled));
        }
        self.expected(linked).map_err(LogStop::Read)?;
        (ask.on_matched)();
        self.state().log.clone()
    }
}

impl FakePullRequests {
    /// An auto-merge or re-run plan read: recorded, held at the plan gate, then answered.
    fn action_plan(
        &self,
        linked: &Linked,
        stamp: &PullRequestStamp,
        change: Option<AutoMergeChange>,
        cancel: &Cancel,
    ) -> Result<ActionPlan, ActionStop> {
        let gate = {
            let mut state = self.state();
            state.action_plan_calls.push((stamp.clone(), change));
            state.plan_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        if cancel.is_cancelled() {
            return Err(ActionStop::Cancelled);
        }
        self.expected(linked)
            .map_err(|error| ActionStop::Failed(format!("{error:?}")))?;
        self.state().action_plan.clone()
    }

    /// An auto-merge change or a re-run: recorded, held at the gates around its send, which is
    /// counted; the caller answers.
    fn send_action(
        &self,
        linked: &Linked,
        change: Option<AutoMergeChange>,
        ask: ActionAsk<'_>,
        cancel: &Cancel,
    ) -> Result<(), ActionStop> {
        let gate = {
            let mut state = self.state();
            state.action_calls.push(FlowCall {
                stamp: ask.stamp.clone(),
                change,
                decision: ask.decision.cloned(),
            });
            state.unsent_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
        if cancel.is_cancelled() {
            return Err(ActionStop::Cancelled);
        }
        self.expected(linked)
            .map_err(|error| ActionStop::Failed(format!("{error:?}")))?;
        if ask.decision.is_none() {
            return Err(ActionStop::Failed(
                "the fake got no confirmation".to_owned(),
            ));
        }
        (ask.on_send)();
        let gate = {
            let mut state = self.state();
            state.actions_sent += 1;
            state.sent_gate.clone()
        };
        if let Some(gate) = gate {
            gate.pass();
        }
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
        id: PullRequestId::parse("PR_kwDOfake"),
        base_oid: None,
        body: body.to_owned(),
        url: None,
        mergeable: Mergeable::Mergeable,
        merge_state: Some(MergeStateStatus::Clean),
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

/// The plan the guard would hand out for merging `detail` (as read from [`linked`]'s
/// repository) with `method`: the policy's own decision for it.
///
/// # Panics
/// When something stands in the way of merging `detail` with `method`.
#[must_use]
pub fn merge_plan(detail: &PullRequestDetail, method: MergeMethod) -> MergePlan {
    let linked = linked();
    #[expect(clippy::expect_used, reason = "a test helper given a mergeable detail")]
    let merge = rc_git_guard::actions::github::PullRequestMerge::new(
        &linked.target,
        linked.account.key.login(),
        detail,
        method,
    )
    .expect("a mergeable pull request");
    let decision = crate::repository_actions::policy()
        .evaluate(&rc_git_guard::Action::MergePullRequest(Box::new(merge)));
    MergePlan {
        decision,
        detail: detail.clone(),
        bypassed: None,
    }
}

/// `detail` blocked by branch protection, read by an account GitHub says may merge it past
/// that protection: what the admin merge control and [`admin_merge_plan`] take.
#[must_use]
pub fn blocked_detail(mut detail: PullRequestDetail) -> PullRequestDetail {
    detail.merge_state = Some(MergeStateStatus::Blocked);
    detail.viewer = Some(ViewerAbilities {
        can_merge_as_admin: true,
        ..ViewerAbilities::default()
    });
    detail
}

/// The plan the guard would hand out for an admin merge of `detail` (as read from [`linked`]'s
/// repository) with `method`: the policy's own decision for it, with what it skips.
///
/// # Panics
/// When something stands in the way of an admin merge of `detail` with `method`.
#[must_use]
pub fn admin_merge_plan(detail: &PullRequestDetail, method: MergeMethod) -> MergePlan {
    let linked = linked();
    #[expect(clippy::expect_used, reason = "a test helper given a blocked detail")]
    let merge = rc_git_guard::actions::github::PullRequestAdminMerge::new(
        &linked.target,
        linked.account.key.login(),
        detail,
        method,
    )
    .expect("a pull request an admin may merge");
    let bypassed = merge.bypassed().clone();
    let decision = crate::repository_actions::policy().evaluate(
        &rc_git_guard::Action::AdminMergePullRequest(Box::new(merge)),
    );
    MergePlan {
        decision,
        detail: detail.clone(),
        bypassed: Some(bypassed),
    }
}

/// `detail` blocked by branch protection in a repository that allows auto-merge, read by an
/// account GitHub says may turn it on and off: what the auto-merge control offers.
#[must_use]
pub fn auto_mergeable_detail(mut detail: PullRequestDetail) -> PullRequestDetail {
    detail.merge_state = Some(MergeStateStatus::Blocked);
    detail.auto_merge_allowed = Some(true);
    detail.viewer = Some(ViewerAbilities {
        can_enable_auto_merge: true,
        can_disable_auto_merge: true,
        ..ViewerAbilities::default()
    });
    detail
}

/// [`auto_mergeable_detail`] with squash auto-merge on, turned on by `mona`.
#[must_use]
pub fn auto_merge_on_detail(detail: PullRequestDetail) -> PullRequestDetail {
    let mut detail = auto_mergeable_detail(detail);
    detail.auto_merge = Some(AutoMerge {
        method: Some(MergeMethod::Squash),
        enabled_by: Some("mona".to_owned()),
    });
    detail
}

/// The plan the guard would hand out for changing auto-merge of `detail` (as read from
/// [`linked`]'s repository) as `change` says: the policy's own decision for it.
///
/// # Panics
/// When something stands in the way of that change.
#[must_use]
pub fn auto_merge_plan(detail: &PullRequestDetail, change: AutoMergeChange) -> ActionPlan {
    use rc_git_guard::actions::github::{AutoMergeDisable, AutoMergeEnable};
    let linked = linked();
    let login = linked.account.key.login();
    #[expect(
        clippy::expect_used,
        reason = "a test helper given a detail the change fits"
    )]
    let action = match change {
        AutoMergeChange::Enable(method) => rc_git_guard::Action::EnableAutoMerge(Box::new(
            AutoMergeEnable::new(&linked.target, login, detail, method)
                .expect("a pull request auto-merge can be turned on for"),
        )),
        AutoMergeChange::Disable => rc_git_guard::Action::DisableAutoMerge(Box::new(
            AutoMergeDisable::new(&linked.target, login, detail)
                .expect("a pull request with auto-merge on"),
        )),
    };
    ActionPlan {
        decision: crate::repository_actions::policy().evaluate(&action),
        detail: detail.clone(),
    }
}

/// `detail` with a failed GitHub Actions job of each of `runs`.
#[must_use]
pub fn failing_detail(mut detail: PullRequestDetail, runs: &[u64]) -> PullRequestDetail {
    for (index, run) in runs.iter().enumerate() {
        detail.checks.checks.push(Check {
            name: format!("job {index}"),
            progress: CheckProgress::Run {
                status: CheckStatus::Completed,
                conclusion: Some(CheckConclusion::Failure),
            },
            description: None,
            required: None,
            details: None,
            actions: Some(ActionsIds {
                workflow_run: *run,
                job: 1000 + *run,
            }),
        });
    }
    detail.checks.total = detail.checks.checks.len();
    detail
}

/// The plan the guard would hand out for re-running the failed jobs of `detail` (as read from
/// [`linked`]'s repository): the policy's own decision for it.
///
/// # Panics
/// When `detail` has no failed run to re-run.
#[must_use]
pub fn rerun_plan(detail: &PullRequestDetail) -> ActionPlan {
    let linked = linked();
    #[expect(
        clippy::expect_used,
        reason = "a test helper given a detail with failed runs"
    )]
    let rerun = rc_git_guard::actions::github::PullRequestRerun::new(
        &linked.target,
        linked.account.key.login(),
        detail,
    )
    .expect("a pull request with failed runs");
    ActionPlan {
        decision: crate::repository_actions::policy()
            .evaluate(&rc_git_guard::Action::RerunFailedJobs(Box::new(rerun))),
        detail: detail.clone(),
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
