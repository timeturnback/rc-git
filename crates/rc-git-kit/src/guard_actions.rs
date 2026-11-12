//! The guard's action types a front end builds requests from and reads results of: the
//! commit and stash requests, who they record, and how an action fails. Re-exported so the app
//! names them through kit and needs no dependency on the guard itself.

pub use rc_git_guard::actions::{
    ActionError, ActionResult, Author, CommitRequest, Committed, Deleted, FetchOutcome, Identity,
    StashRequest, UpstreamStatus,
};
