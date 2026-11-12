//! What the update's fetch and fast-forward produced, read off the [`SyncModel`] that ran it
//! (`UpdateEnvironmentLive.swift`, `sync()`).
//!
//! [`SyncModel`]: crate::sync::SyncModel

use crate::sync::{SyncState, reconcile_command};

use super::UpdateSyncResult;

/// The update's sync result for the state the pull of `source_root` ended in. A diverged
/// checkout is reconciled in git, so its failure names the command. Any state but a finished,
/// diverged or failed workflow means the pull never ran to an end: a cancel before it
/// started, or a status read that left it idle.
#[must_use]
pub fn sync_result(state: &SyncState, source_root: &str) -> UpdateSyncResult {
    match state {
        SyncState::Done(summary) => UpdateSyncResult::Synced {
            notice: summary.notice.clone(),
        },
        SyncState::Diverged(message) => UpdateSyncResult::Failure(format!(
            "{message}; run `{}` in a terminal, then update again",
            reconcile_command(source_root)
        )),
        SyncState::Failed { message, .. } => UpdateSyncResult::Failure(message.clone()),
        SyncState::Idle | SyncState::Running(_) | SyncState::Cancelling(_) => {
            UpdateSyncResult::Failure("the pull did not finish".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sync::{SyncStep, SyncSummary};

    use super::*;

    #[test]
    fn a_finished_pull_carries_its_notice() {
        let summary = SyncSummary {
            notice: Some("saved credentials unreadable".to_owned()),
            ..SyncSummary::default()
        };
        assert_eq!(
            sync_result(&SyncState::Done(summary), "/src"),
            UpdateSyncResult::Synced {
                notice: Some("saved credentials unreadable".to_owned())
            }
        );
        assert_eq!(
            sync_result(&SyncState::Done(SyncSummary::default()), "/src"),
            UpdateSyncResult::Synced { notice: None }
        );
    }

    #[test]
    fn a_diverged_checkout_names_the_rebase() {
        assert_eq!(
            sync_result(
                &SyncState::Diverged("2 ahead, 4 behind".to_owned()),
                "/my src"
            ),
            UpdateSyncResult::Failure(
                "2 ahead, 4 behind; run `cd '/my src' && git pull --rebase` in a terminal, \
                 then update again"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_failure_keeps_its_message() {
        let state = SyncState::Failed {
            step: SyncStep::Fetch,
            message: "cancelled".to_owned(),
        };
        assert_eq!(
            sync_result(&state, "/src"),
            UpdateSyncResult::Failure("cancelled".to_owned())
        );
    }

    #[test]
    fn a_pull_that_never_ended_is_a_failure() {
        for state in [
            SyncState::Idle,
            SyncState::Running(SyncStep::Pull),
            SyncState::Cancelling(SyncStep::Fetch),
        ] {
            assert_eq!(
                sync_result(&state, "/src"),
                UpdateSyncResult::Failure("the pull did not finish".to_owned()),
                "{state:?}"
            );
        }
    }
}
