//! A check's job log in its pull request tab: which checks offer it ([`log_check`], decided here,
//! never in the app), one read as a state machine ([`LogState`]), and the log as the text view
//! shows it ([`ShownLog`]).
//!
//! The read is the guard's (`read_job_log`): it matches the job to the head on screen first and
//! downloads nothing for a check it cannot match. It runs only from the user's click, for one
//! job at a time per tab; a second click cancels the read in flight and starts the new one. It is
//! held in memory only: nothing writes it to disk or to the app's log. A log belongs to the head
//! it was read for: a tab read that shows another head drops it.
//!
//! A running job's log is not offered: GitHub does not document what its log endpoint answers
//! before the job completes, so only a completed job (any conclusion) has the action.

use std::sync::Arc;

use rc_git_core::Oid;

use super::state::DetailState;
use super::{
    Check, CheckProgress, CheckStatus, JobId, JobLog, LogCut, PullRequestDetail, ReadError, RunId,
};
use crate::text_document::TextDocument;

/// A check whose log may be read: its name as shown, its workflow run and its job.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogCheck {
    /// The check's name, single-line as the guard read it.
    pub name: String,
    /// Its workflow run.
    pub run: RunId,
    /// Its job.
    pub job: JobId,
}

/// The log action of `check` in `detail`: a GitHub Actions job with ids the guard accepts that
/// has completed. `None` for every other check.
#[must_use]
pub fn log_check_of(detail: &PullRequestDetail, check: &Check) -> Option<LogCheck> {
    let ids = check.actions?;
    let completed = match check.progress {
        CheckProgress::Run {
            status: CheckStatus::Completed,
            ..
        } => true,
        CheckProgress::Run { .. } | CheckProgress::Status(_) => false,
    };
    let held = detail.checks.checks.iter().any(|held| held == check);
    if !completed || !held {
        return None;
    }
    Some(LogCheck {
        name: check.name.clone(),
        run: RunId::new(ids.workflow_run)?,
        job: JobId::new(ids.job)?,
    })
}

/// The log action of `check` in `tab`: [`log_check_of`] on its detail, and only while that
/// detail is current, so a stale screen never offers it.
#[must_use]
pub fn log_check(tab: &DetailState, check: &Check) -> Option<LogCheck> {
    tab.stamp()?;
    log_check_of(tab.detail()?, check)
}

/// Why a log read showed no log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogStop {
    /// The job could not be matched to the pull request's head commit; the guard's words.
    NotMatched(String),
    /// The job has not finished.
    NotFinished,
    /// A request failed.
    Read(ReadError),
}

/// What [`super::PullRequestService::read_log`] is asked.
#[derive(Clone, Copy)]
pub struct LogAsk<'a> {
    /// The head commit on screen.
    pub head: Oid,
    /// The check.
    pub check: &'a LogCheck,
    /// Called once the job matched, before the download.
    pub on_matched: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for LogAsk<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogAsk")
            .field("head", &self.head)
            .field("check", &self.check)
            .finish_non_exhaustive()
    }
}

/// A log ready to show: what the guard read about it and its text as a document for the text
/// view, built off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownLog {
    /// The job's name as GitHub's job read names it.
    pub name: String,
    /// The head commit it was matched to.
    pub head: Oid,
    /// How much of the log was downloaded.
    pub cut: LogCut,
    /// Lines downloaded but not shown.
    pub lines_left_out: usize,
    /// Lines shown cut short.
    pub lines_shortened: usize,
    /// The neutralized text.
    pub document: Arc<TextDocument>,
}

impl ShownLog {
    /// `log` as shown: its text moved into a document, lines starting by the core's rule.
    #[must_use]
    pub fn new(log: JobLog) -> Self {
        let JobLog {
            name,
            head,
            text,
            cut,
            lines_left_out,
            lines_shortened,
            ..
        } = log;
        let bytes = text.into_bytes();
        let mut starts = Vec::new();
        if !bytes.is_empty() {
            starts.push(0);
        }
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' && index + 1 < bytes.len() {
                starts.push(index + 1);
            }
        }
        Self {
            name,
            head,
            cut,
            lines_left_out,
            lines_shortened,
            document: Arc::new(TextDocument::new(bytes, &starts, true)),
        }
    }
}

/// Where a log read is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogPhase {
    /// The guard is matching the job to the head commit.
    Matching,
    /// The job matched; the log is downloading.
    Downloading,
    /// The log.
    Shown(Arc<ShownLog>),
    /// The job could not be matched to the head commit; why, in the guard's words.
    NotMatched(String),
    /// The job has not finished.
    NotFinished,
    /// The read failed.
    Failed(ReadError),
}

/// One log read of one tab, under its generation: a number no other read of the window had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogState {
    generation: u64,
    number: u64,
    head: Oid,
    check: LogCheck,
    phase: LogPhase,
}

impl LogState {
    /// The read of `check`'s log for tab `number` at `head`, matching.
    #[must_use]
    pub fn begin(generation: u64, number: u64, head: Oid, check: LogCheck) -> Self {
        Self {
            generation,
            number,
            head,
            check,
            phase: LogPhase::Matching,
        }
    }

    /// Its generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The tab's pull request number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The head commit it is read for.
    #[must_use]
    pub fn head(&self) -> Oid {
        self.head
    }

    /// The check.
    #[must_use]
    pub fn check(&self) -> &LogCheck {
        &self.check
    }

    /// Where it is.
    #[must_use]
    pub fn phase(&self) -> &LogPhase {
        &self.phase
    }

    /// Whether a request may still be running: matching or downloading.
    #[must_use]
    pub fn is_running(&self) -> bool {
        match self.phase {
            LogPhase::Matching | LogPhase::Downloading => true,
            LogPhase::Shown(_)
            | LogPhase::NotMatched(_)
            | LogPhase::NotFinished
            | LogPhase::Failed(_) => false,
        }
    }

    /// The job of read `generation` matched: it downloads now. Whether it applied.
    pub fn matched(&mut self, generation: u64) -> bool {
        if generation != self.generation || self.phase != LogPhase::Matching {
            return false;
        }
        self.phase = LogPhase::Downloading;
        true
    }

    /// Read `generation` ended with `answer`. An answer of another read, or of one that already
    /// ended, is dropped. Whether it applied.
    pub fn finish(&mut self, generation: u64, answer: Result<Arc<ShownLog>, LogStop>) -> bool {
        if generation != self.generation || !self.is_running() {
            return false;
        }
        self.phase = match answer {
            Ok(log) => LogPhase::Shown(log),
            Err(LogStop::NotMatched(why)) => LogPhase::NotMatched(why),
            Err(LogStop::NotFinished) => LogPhase::NotFinished,
            Err(LogStop::Read(error)) => LogPhase::Failed(error),
        };
        true
    }

    /// Whether it still belongs on a tab that now shows `head`.
    #[must_use]
    pub fn follows(&self, head: Oid) -> bool {
        self.head == head
    }
}
