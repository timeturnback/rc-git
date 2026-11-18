//! How a pull request's checks read: each check's state (the guard's [`CheckState`], so the tab
//! and a merge's challenge count alike), the list in the order the tab shows it (required checks
//! first, then by state, failing first), the summary line, and the state icon of a row in the
//! list.

pub use rc_git_guard::actions::github::CheckState;

use super::{Check, CheckList, CheckProgress, StatusState};

/// How `progress` stands: the guard's reading, which a merge's challenge counts by too.
#[must_use]
pub fn check_state(progress: &CheckProgress) -> CheckState {
    progress.state()
}

/// How a commit status, or a rollup, stands.
#[must_use]
pub fn status_check_state(state: StatusState) -> CheckState {
    state.check_state()
}

/// Whether any of `checks` has not finished, so the tab keeps polling.
#[must_use]
pub fn pending(checks: &[Check]) -> bool {
    checks
        .iter()
        .any(|check| check_state(&check.progress) == CheckState::Pending)
}

/// `checks` in the order the tab lists them: required ones first, then failing, pending,
/// passing and neutral, then by name.
#[must_use]
pub fn ordered(checks: &[Check]) -> Vec<&Check> {
    let mut sorted: Vec<&Check> = checks.iter().collect();
    sorted.sort_by(|a, b| {
        let key = |check: &Check| (check.required != Some(true), check_state(&check.progress));
        key(a).cmp(&key(b)).then_with(|| a.name.cmp(&b.name))
    });
    sorted
}

/// The line over the checks: how many fail, run and pass, how many of them are required, and
/// how many the page did not hold. `No checks.` when the commit has none.
#[must_use]
pub fn summary(list: &CheckList) -> String {
    if list.total == 0 {
        return "No checks.".to_owned();
    }
    let count = |state| {
        list.checks
            .iter()
            .filter(|check| check_state(&check.progress) == state)
            .count()
    };
    let mut parts = Vec::new();
    for (state, word) in [
        (CheckState::Failing, "failing"),
        (CheckState::Pending, "pending"),
        (CheckState::Passing, "passing"),
        (CheckState::Neutral, "neutral or skipped"),
    ] {
        let n = count(state);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    let mut sections = vec![parts.join(", ")];
    let required = list
        .checks
        .iter()
        .filter(|check| check.required == Some(true))
        .count();
    if required > 0 {
        sections.push(format!("{required} required"));
    }
    let unread = list.total.saturating_sub(list.checks.len());
    if unread > 0 {
        sections.push(format!("{unread} more not shown"));
    }
    format!("{}.", sections.join("; "))
}

/// The state icon of a row in the list: `None` when the head commit has no checks.
#[must_use]
pub fn row_state(rollup: Option<StatusState>) -> Option<CheckState> {
    rollup.map(status_check_state)
}
