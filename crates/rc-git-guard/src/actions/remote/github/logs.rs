//! A GitHub Actions job's log, for one check of the pull request on screen. A read: nothing on
//! GitHub changes, so it has no `Action` and no challenge, and it runs only when the user asks,
//! one job at a time. Nothing here writes it anywhere: it is held in memory and shown.
//!
//! [`read_job_log`] runs three steps, in order, and stops at the first that fails:
//!
//! 1. **Match the job to what is on screen.** The check carries ids the server reported, and
//!    GitHub does not say that a check run's `databaseId` is the Actions job id. So the job is
//!    read first, by id, from the REST API on the account's host (`GET
//!    /repos/{owner}/{repo}/actions/jobs/{job_id}`, "Get a job for a workflow run"), and the log
//!    is read only when that job's `head_sha` is the stamped head commit and its `run_id` the
//!    check's workflow run. A mismatch, a 404 or a missing field is [`JobLogError::NotMatched`]:
//!    no log that might be another commit's or another job's is ever shown. A job whose `status`
//!    is not `completed` is [`JobLogError::NotFinished`]: GitHub does not document what its log
//!    endpoint answers for a running job.
//! 2. **Ask where the log is.** `GET /repos/{owner}/{repo}/actions/jobs/{job_id}/logs` ("Download
//!    job logs for a workflow run"), with the token, to the account's host like every API call.
//!    GitHub answers `302 Found` with the log's address in `Location`, a link it says expires
//!    after a minute. This is the only call whose redirect rc-git reads
//!    (`Session::job_log_location`); curl follows none.
//! 3. **The one hop.** The address must pass [`LogDownloadUrl::parse`] (`super::download`):
//!    `https://`, a public DNS name, no userinfo, no IP literal, nothing that is not printable
//!    ASCII, at most 4096 bytes. Then one `GET` goes there ([`download`]), with no token and no
//!    GitHub API header (the request type has no token field), following no redirect, asking for
//!    the last [`LOG_BYTES`] bytes (`Range: bytes=-<n>`). A `206` is the end of the log; a `200`
//!    (a host that ignores the range) is kept up to [`LOG_BYTES`] and the transfer stops there. A
//!    `416` that does not name an empty log (a host that refuses suffix ranges) is followed by one
//!    more `GET` to the same address without a range, kept up to [`LOG_BYTES`] from its start
//!    ([`download_log`]); no other answer sends a second request, and nothing a third. Either way
//!    what was left out is a value ([`LogCut`]), never an error, and the API's own over-the-cap
//!    rule (an error) is untouched.
//!
//! The log is whatever the workflow printed, so it is attacker-controlled: it is decoded as UTF-8
//! with bad bytes replaced, split into lines at `\n`, `\r\n` and a lone `\r`, and each line passes
//! through `log_line` (ANSI escape sequences dropped; control, bidi and invisible characters
//! shown as visible `\u{..}` escapes; tabs kept), cut at [`LOG_LINE_CHARS`] characters with a `…`,
//! and at most [`LOG_LINES`] lines are kept (the last ones, unless the log was cut at its start).
//! Nothing in it is a link.

use std::fmt;

use rc_git_core::Oid;
use serde_json::Value;

use super::api::Session;
use super::download::LogDownloadUrl;
use super::host::{GitHubHost, PLAIN_HTTP};
use super::http::{DownloadRequest, Downloaded, download};
use super::rerun::RunId;
use super::secret::ApiToken;
use super::text::{log_line, single_line};
use super::{GitHubError, GitHubResult, RepositoryTarget};
use crate::actions::remote::Cancel;

/// The most bytes of a log rc-git downloads and holds: the last ones, where a failure is.
pub const LOG_BYTES: usize = 2 * 1024 * 1024;

/// The most lines of a log shown.
pub const LOG_LINES: usize = 20_000;

/// The most characters of one log line shown; a longer line ends in `…`.
pub const LOG_LINE_CHARS: usize = 1_000;

/// The longest job name kept, in characters.
const NAME_CHARS: usize = 200;

/// A GitHub Actions job's id, as the host reported it: a positive integer that fits a signed
/// 64-bit id (GitHub's REST `job_id` is an `int64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(u64);

impl JobId {
    /// `id` as a job id: 1 to 2^63 − 1, else `None`.
    #[must_use]
    pub fn new(id: u64) -> Option<Self> {
        let fits = i64::try_from(id).is_ok_and(|id| id > 0);
        fits.then_some(Self(id))
    }

    /// The id's number.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The log to read: which job of which run, for which head commit, by which account.
#[derive(Clone, Copy)]
pub struct JobLogRequest<'a> {
    /// The account's host: the only host the token goes to.
    pub host: &'a GitHubHost,
    /// The account's token.
    pub token: &'a ApiToken,
    /// The repository, which must be on `host`.
    pub target: &'a RepositoryTarget,
    /// The head commit of the pull request on screen (its stamp's).
    pub head: Oid,
    /// The check's workflow run.
    pub run: RunId,
    /// The check's job.
    pub job: JobId,
    /// Called once the job matched, just before the log is asked for.
    pub on_matched: &'a (dyn Fn() + Sync),
}

impl fmt::Debug for JobLogRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobLogRequest")
            .field("host", &self.host)
            .field("token", &self.token)
            .field("target", &self.target)
            .field("head", &self.head)
            .field("run", &self.run)
            .field("job", &self.job)
            .finish_non_exhaustive()
    }
}

/// How much of the log was downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogCut {
    /// All of it.
    Whole,
    /// Its last `kept` bytes, from the first line break on: the storage host honored the range.
    Tail {
        /// Bytes kept.
        kept: usize,
    },
    /// Its first `kept` bytes: the storage host sent the whole log and the transfer stopped at
    /// [`LOG_BYTES`]; the rest was never downloaded.
    Head {
        /// Bytes kept.
        kept: usize,
    },
}

/// A job's log, neutralized, as read for one head commit.
#[derive(Clone, PartialEq, Eq)]
pub struct JobLog {
    /// The job.
    pub job: JobId,
    /// Its workflow run.
    pub run: RunId,
    /// The head commit it was matched to.
    pub head: Oid,
    /// The job's name as GitHub's job read names it, single-line.
    pub name: String,
    /// The lines, joined with `\n`: no other line break, no control, bidi or invisible character
    /// but as a visible escape, no ANSI escape sequence.
    pub text: String,
    /// How many lines `text` holds.
    pub lines: usize,
    /// How much of the log was downloaded.
    pub cut: LogCut,
    /// Lines downloaded but not shown, past [`LOG_LINES`]: the first ones, or the last ones when
    /// the log was cut at its start ([`LogCut::Head`]).
    pub lines_left_out: usize,
    /// Lines shown cut at [`LOG_LINE_CHARS`] characters.
    pub lines_shortened: usize,
}

impl fmt::Debug for JobLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobLog")
            .field("job", &self.job)
            .field("run", &self.run)
            .field("head", &self.head)
            .field("name", &self.name)
            .field("text", &format_args!("<{} bytes>", self.text.len()))
            .field("lines", &self.lines)
            .field("cut", &self.cut)
            .field("lines_left_out", &self.lines_left_out)
            .field("lines_shortened", &self.lines_shortened)
            .finish()
    }
}

/// Why no log is shown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobLogError {
    /// GitHub's job read does not match the check on screen: another head commit, another run,
    /// no such job, or an answer without those fields. No log was asked for.
    #[error("this check's log could not be matched to the pull request's head commit: {why}")]
    NotMatched {
        /// The job.
        job: JobId,
        /// What did not match, in rc-git's words.
        why: String,
    },
    /// The job has not finished. No log was asked for.
    #[error("job {job} has not finished, so its log is not read yet")]
    NotFinished {
        /// The job.
        job: JobId,
    },
    /// A request failed: the job read, the log's address, or the download.
    #[error("{0}")]
    Failed(GitHubError),
}

/// The log `request` names, as the module describes: the job matched first, then its address,
/// then one download from there without the token.
///
/// # Errors
/// [`JobLogError::NotMatched`] and [`JobLogError::NotFinished`] before the log is asked for;
/// [`JobLogError::Failed`] with [`GitHubError::ForeignRemote`] before any request for a target on
/// another host, [`GitHubError::LogDownloadRefused`] for an address the download refuses (nothing
/// is sent to it), [`GitHubError::LogDownload`] for a storage answer that is not the log, and the
/// transport, status and rate-limit errors of each request.
pub fn read_job_log(request: JobLogRequest<'_>, cancel: &Cancel) -> Result<JobLog, JobLogError> {
    const OP: &str = "download the job log";
    let session = Session::new(request.host, request.token);
    let name = matched_job(&session, &request, cancel)?;
    (request.on_matched)();
    let location = session
        .job_log_location(OP, request.target, request.job, cancel)
        .map_err(JobLogError::Failed)?;
    let url = LogDownloadUrl::parse(location.text(), PLAIN_HTTP).map_err(|refused| {
        JobLogError::Failed(GitHubError::LogDownloadRefused {
            host: refused
                .host
                .unwrap_or_else(|| "an address rc-git could not read".to_owned()),
            reason: refused.reason,
        })
    })?;
    let host = single_line(url.host(), 300);
    let downloaded = download_log(OP, &url, cancel).map_err(JobLogError::Failed)?;
    let (bytes, cut) = body_of(downloaded).map_err(|reason| {
        JobLogError::Failed(GitHubError::LogDownload {
            host: host.clone(),
            reason,
        })
    })?;
    let shown = shown_text(&bytes, cut);
    Ok(JobLog {
        job: request.job,
        run: request.run,
        head: request.head,
        name,
        text: shown.text,
        lines: shown.lines,
        cut,
        lines_left_out: shown.left_out,
        lines_shortened: shown.shortened,
    })
}

/// The log at `url`, in two requests at most: one `GET` asking for its last [`LOG_BYTES`] bytes,
/// and, only when the storage host answers that one `416` without saying the log is empty, one
/// more `GET` to the same address without a range, kept to its first [`LOG_BYTES`] bytes. Suffix
/// ranges are not documented for the storage GitHub uses, so a host that refuses one still gives
/// the log. No other answer sends a second request, and nothing sends a third.
///
/// # Errors
/// What [`download`] returns for either request.
fn download_log(
    op: &'static str,
    url: &LogDownloadUrl,
    cancel: &Cancel,
) -> GitHubResult<Downloaded> {
    let request = |tail| DownloadRequest {
        op,
        url: url.clone(),
        limit: LOG_BYTES,
        tail,
    };
    let tail = download(request(true), cancel)?;
    if !range_refused(&tail) {
        return Ok(tail);
    }
    download(request(false), cancel)
}

/// Whether `downloaded` is a `416` refusing the range itself, rather than one naming an empty log
/// (`Content-Range: bytes */0`), which [`body_of`] reads as the log.
fn range_refused(downloaded: &Downloaded) -> bool {
    downloaded.status == 416 && downloaded.content_range.as_deref() != Some("bytes */0")
}

/// The job's name, once GitHub's job read matches `request`: its id, its run, its head commit
/// and a finished status.
fn matched_job(
    session: &Session<'_>,
    request: &JobLogRequest<'_>,
    cancel: &Cancel,
) -> Result<String, JobLogError> {
    const OP: &str = "read the job";
    let job = request.job.to_string();
    let not_matched = |why: String| JobLogError::NotMatched {
        job: request.job,
        why,
    };
    let reply = match session.get_repo(OP, request.target, &["actions", "jobs", &job], cancel) {
        Ok(reply) => reply,
        Err(GitHubError::Status { status: 404, .. }) => {
            return Err(not_matched(format!(
                "GitHub knows no job {job} in this repository"
            )));
        }
        Err(error) => return Err(JobLogError::Failed(error)),
    };
    let value = &reply.value;
    let number = |key: &str| value.get(key).and_then(Value::as_u64);
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let head = text("head_sha").and_then(|hex| Oid::from_hex(hex).ok());
    let (Some(id), Some(run), Some(head), Some(status), Some(name)) = (
        number("id"),
        number("run_id"),
        head,
        text("status"),
        text("name"),
    ) else {
        return Err(not_matched(
            "GitHub's answer lacks the job's id, run, head commit, status or name".to_owned(),
        ));
    };
    if id != request.job.get() {
        return Err(not_matched(format!(
            "GitHub answered for job {id}, not job {job}"
        )));
    }
    if run != request.run.get() {
        return Err(not_matched(format!(
            "job {job} belongs to workflow run {run}, not run {}",
            request.run
        )));
    }
    if head != request.head {
        return Err(not_matched(format!(
            "job {job} ran on commit {}, not the head commit {}",
            short(head),
            short(request.head)
        )));
    }
    if status != "completed" {
        return Err(JobLogError::NotFinished { job: request.job });
    }
    Ok(single_line(name, NAME_CHARS))
}

/// The first seven hex digits of `oid`.
fn short(oid: Oid) -> String {
    oid.to_hex().chars().take(7).collect()
}

/// The log's bytes and how much of it they are, from the storage host's answer: a `206` is its
/// end, a `200` the log from its start (cut when it was longer than the cap), a `416` with
/// `Content-Range: bytes */0` an empty log. Anything else names what came back, never its body.
fn body_of(downloaded: Downloaded) -> Result<(Vec<u8>, LogCut), String> {
    match downloaded.status {
        200 => {
            let kept = downloaded.body.len();
            let cut = if downloaded.cut {
                LogCut::Head { kept }
            } else {
                LogCut::Whole
            };
            Ok((downloaded.body, cut))
        }
        206 => tail(downloaded),
        416 if downloaded.content_range.as_deref() == Some("bytes */0") => {
            Ok((Vec::new(), LogCut::Whole))
        }
        status => Err(format!("the storage host answered HTTP {status}")),
    }
}

/// The end of the log a `206` carries: it must name a range that ends where the log ends and
/// holds exactly the bytes sent. A tail that starts mid-log loses its first, partial line.
fn tail(downloaded: Downloaded) -> Result<(Vec<u8>, LogCut), String> {
    if downloaded.cut {
        return Err("the storage host sent more than the byte range asked for".to_owned());
    }
    let Some((first, last, total)) = downloaded.content_range.as_deref().and_then(content_range)
    else {
        return Err("the storage host's partial answer names no byte range".to_owned());
    };
    if last - first + 1 != downloaded.body.len() || total.is_some_and(|total| total != last + 1) {
        return Err(
            "the storage host's partial answer is not the end of the log it names".to_owned(),
        );
    }
    let mut body = downloaded.body;
    if first == 0 {
        return Ok((body, LogCut::Whole));
    }
    if let Some(newline) = body.iter().position(|byte| *byte == b'\n') {
        body.drain(..=newline);
    }
    let kept = body.len();
    Ok((body, LogCut::Tail { kept }))
}

/// `bytes <first>-<last>/<total>` (`total` `*` when unknown) as numbers, `first <= last`.
fn content_range(text: &str) -> Option<(usize, usize, Option<usize>)> {
    let (range, total) = text.strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    let number = |part: &str| {
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse::<usize>().ok())
            .flatten()
    };
    let (first, last) = (number(first)?, number(last)?);
    let total = match total {
        "*" => None,
        total => Some(number(total)?),
    };
    (first <= last).then_some((first, last, total))
}

/// The log's text as shown.
struct Shown {
    text: String,
    lines: usize,
    left_out: usize,
    shortened: usize,
}

/// `bytes` decoded, split into lines, each neutralized and cut, at most [`LOG_LINES`] of them:
/// the last ones, or the first when the log was cut at its start.
fn shown_text(bytes: &[u8], cut: LogCut) -> Shown {
    let decoded = String::from_utf8_lossy(bytes);
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(&decoded);
    let total = lines(decoded).count();
    let skip = match cut {
        LogCut::Whole | LogCut::Tail { .. } => total.saturating_sub(LOG_LINES),
        LogCut::Head { .. } => 0,
    };
    let mut shown = Shown {
        text: String::new(),
        lines: 0,
        left_out: 0,
        shortened: 0,
    };
    for line in lines(decoded).skip(skip).take(LOG_LINES) {
        let (line, cut) = log_line(line, LOG_LINE_CHARS);
        if cut {
            shown.shortened += 1;
        }
        if shown.lines > 0 {
            shown.text.push('\n');
        }
        shown.text.push_str(&line);
        shown.lines += 1;
    }
    shown.left_out = total - shown.lines;
    shown
}

/// The lines of `text`, split at `\n`, `\r\n` and a lone `\r`; a final break starts no empty line.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split_terminator('\n')
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_ids_are_positive_and_fit_a_signed_64_bit_id() {
        assert_eq!(JobId::new(0), None);
        assert_eq!(JobId::new(1 << 63), None);
        let largest = 9_223_372_036_854_775_807;
        assert_eq!(JobId::new(largest).map(JobId::get), Some(largest));
    }

    #[test]
    fn a_content_range_reads_only_its_exact_shape() {
        assert_eq!(content_range("bytes 0-9/10"), Some((0, 9, Some(10))));
        assert_eq!(content_range("bytes 5-9/*"), Some((5, 9, None)));
        for bad in [
            "bytes 9-5/10",
            "bytes -5/10",
            "items 0-9/10",
            "bytes 0-9",
            "bytes +1-9/10",
        ] {
            assert_eq!(content_range(bad), None, "{bad}");
        }
    }

    fn downloaded(status: u32, range: Option<&str>, body: &[u8], cut: bool) -> Downloaded {
        Downloaded {
            status,
            content_range: range.map(str::to_owned),
            body: body.to_vec(),
            cut,
        }
    }

    #[test]
    fn a_partial_answer_is_the_tail_without_its_partial_first_line() {
        let (body, cut) = body_of(downloaded(
            206,
            Some("bytes 10-19/20"),
            b"ne\nlast\n12",
            false,
        ))
        .unwrap();
        assert_eq!(body, b"last\n12");
        assert_eq!(cut, LogCut::Tail { kept: 7 });
        let (body, cut) = body_of(downloaded(206, Some("bytes 0-3/4"), b"all\n", false)).unwrap();
        assert_eq!((body.as_slice(), cut), (&b"all\n"[..], LogCut::Whole));
        for (range, body, cut) in [
            (Some("bytes 10-19/30"), &b"0123456789"[..], false),
            (Some("bytes 10-19/20"), b"012345678", false),
            (None, b"x", false),
            (Some("bytes 10-19/20"), b"0123456789", true),
        ] {
            assert!(
                body_of(downloaded(206, range, body, cut)).is_err(),
                "{range:?}"
            );
        }
    }

    #[test]
    fn a_whole_answer_is_kept_up_to_the_cap_and_says_so() {
        let (_, cut) = body_of(downloaded(200, None, b"abc", false)).unwrap();
        assert_eq!(cut, LogCut::Whole);
        let (_, cut) = body_of(downloaded(200, None, b"abc", true)).unwrap();
        assert_eq!(cut, LogCut::Head { kept: 3 });
        let (body, cut) = body_of(downloaded(416, Some("bytes */0"), b"", false)).unwrap();
        assert_eq!((body.len(), cut), (0, LogCut::Whole));
        for status in [416, 403, 500, 302] {
            let error = body_of(downloaded(status, Some("bytes */9"), b"<Error/>", false));
            assert_eq!(
                error.unwrap_err(),
                format!("the storage host answered HTTP {status}")
            );
        }
    }

    #[test]
    fn lines_split_at_every_break_and_keep_the_last_ones() {
        let collected: Vec<&str> = lines("a\r\nb\rc\n\nd\n").collect();
        assert_eq!(collected, ["a", "b", "c", "", "d"]);
        let mut many = String::new();
        for n in 0..LOG_LINES + 5 {
            many.push_str(&n.to_string());
            many.push('\n');
        }
        let shown = shown_text(many.as_bytes(), LogCut::Whole);
        assert_eq!((shown.lines, shown.left_out), (LOG_LINES, 5));
        assert!(shown.text.starts_with("5\n6\n"), "the last lines are kept");
        let shown = shown_text(many.as_bytes(), LogCut::Head { kept: many.len() });
        assert!(
            shown.text.starts_with("0\n1\n"),
            "a head keeps the first lines"
        );
        assert_eq!(shown.left_out, 5);
    }
}
