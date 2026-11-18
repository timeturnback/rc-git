//! When the list and an open pull request tab read again. Only what is on screen polls, and
//! only while its window is the active one: the list every [`LIST_INTERVAL`] while its section
//! shows, an open tab every [`DETAIL_INTERVAL`] while one of its checks runs. Nothing polls
//! after a sign-out, and a spent rate limit pauses polling until the host's reset time.

use std::time::Duration;

use super::ReadError;
use super::state::Freshness;

/// How often the list reads again while it shows.
pub const LIST_INTERVAL: Duration = Duration::from_secs(60);

/// How often an open tab with a running check reads again.
pub const DETAIL_INTERVAL: Duration = Duration::from_secs(15);

/// What a poller knows when it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollInputs {
    /// Whether the window is the active one.
    pub window_active: bool,
    /// Whether the section or the tab is on screen.
    pub visible: bool,
    /// Now, in seconds since the Unix epoch.
    pub now: u64,
}

/// When to read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollPlan {
    /// Not until something changes (shown again, the window active again, a new read).
    Stop,
    /// After this long.
    After(Duration),
}

/// When the list reads again, after a read that left it `freshness`.
#[must_use]
pub fn list_poll(inputs: PollInputs, freshness: &Freshness) -> PollPlan {
    if !inputs.window_active || !inputs.visible {
        return PollPlan::Stop;
    }
    after_failure(inputs, freshness).unwrap_or(PollPlan::After(LIST_INTERVAL))
}

/// When an open tab reads again, after a read that left it `freshness`, `pending` saying
/// whether a check of the current detail still runs.
#[must_use]
pub fn detail_poll(inputs: PollInputs, freshness: &Freshness, pending: bool) -> PollPlan {
    if !inputs.window_active || !inputs.visible {
        return PollPlan::Stop;
    }
    if let Some(plan) = after_failure(inputs, freshness) {
        return plan;
    }
    match freshness {
        Freshness::Current { .. } if pending => PollPlan::After(DETAIL_INTERVAL),
        Freshness::Current { .. } | Freshness::Loading | Freshness::Failed(_) => PollPlan::Stop,
    }
}

/// Whether a read left at `freshness` is due again at `now` for something that polls every
/// `interval`, when it comes back on screen or its window back to the front: never read, or
/// failed (other than signed out), or read `interval` ago or longer.
#[must_use]
pub fn is_due(freshness: &Freshness, interval: Duration, now: u64) -> bool {
    match freshness {
        Freshness::Current { at } => now.saturating_sub(*at) >= interval.as_secs(),
        Freshness::Failed(ReadError::SignedOut { .. }) => false,
        Freshness::Failed(ReadError::RateLimited { reset: Some(reset) }) => now >= *reset,
        Freshness::Loading | Freshness::Failed(_) => true,
    }
}

/// The plan a failed read imposes: none after a sign-out, the host's reset after a spent rate
/// limit (a minute when it named none), the list's interval after any other failure.
fn after_failure(inputs: PollInputs, freshness: &Freshness) -> Option<PollPlan> {
    let Freshness::Failed(error) = freshness else {
        return None;
    };
    Some(match error {
        ReadError::SignedOut { .. } => PollPlan::Stop,
        ReadError::RateLimited { reset: Some(reset) } if *reset > inputs.now => {
            PollPlan::After(Duration::from_secs(reset - inputs.now + 1))
        }
        ReadError::RateLimited { .. } | ReadError::Cancelled | ReadError::Failed(_) => {
            PollPlan::After(LIST_INTERVAL)
        }
    })
}
