//! Reading a job's log against the testkit's fake GitHub host, which answers only the exact job
//! read and logs request a correct client sends, and a second fake standing in for the storage
//! host GitHub's redirect names, which records every request's headers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rc_git_testkit::github::{GitHubServer, LogsAnswer, StorageBehaviour, StorageServer};
use serde_json::json;

use super::merge_tests::{HEAD, MOVED, TOKEN, host_of, oid, server, target, token};
use super::*;
use crate::actions::remote::Cancel;

const REPOSITORY: &str = "octo/rc-git";
const JOB: u64 = 9001;
const RUN: u64 = 41;
const SIGNATURE: &str = "c2lnbmF0dXJlLXNlY3JldC0xMjM0NTY3ODkw";

/// GitHub's job read for job [`JOB`] of run `run` at `head`, in `status`.
fn job_body(head: &str, run: u64, status: &str) -> String {
    json!({ "id": JOB, "run_id": run, "head_sha": head, "name": "test \u{202e}job",
        "status": status, "conclusion": "failure" })
    .to_string()
}

/// The signed address of the log on `storage`.
fn signed(storage: &StorageServer) -> String {
    format!(
        "{}/logs/job-{JOB}.txt?sv=2025&sig={SIGNATURE}",
        storage.origin()
    )
}

/// Job [`JOB`] of run [`RUN`] at [`HEAD`] on `server`, its logs redirecting to `storage`.
fn script(server: &GitHubServer, storage: &StorageServer) {
    server.script_job(REPOSITORY, JOB, "200 OK", &job_body(HEAD, RUN, "completed"));
    redirect_to(server, &signed(storage));
}

fn redirect_to(server: &GitHubServer, location: &str) {
    server.script_job_logs(
        REPOSITORY,
        JOB,
        LogsAnswer::Redirect {
            status: "302 Found",
            location: location.to_owned(),
        },
    );
}

/// Reads job [`JOB`]'s log from `server` for `head`, checking that no message carries a secret;
/// the answer and how many times the job matched.
fn read_log(
    server: &GitHubServer,
    head: &str,
    cancel: &Cancel,
) -> (Result<JobLog, JobLogError>, usize) {
    let host = host_of(server);
    let token = token();
    let target = target(&host);
    let matched = AtomicUsize::new(0);
    let on_matched = || {
        matched.fetch_add(1, Ordering::SeqCst);
    };
    let request = JobLogRequest {
        host: &host,
        token: &token,
        target: &target,
        head: oid(head),
        run: RunId::new(RUN).unwrap(),
        job: JobId::new(JOB).unwrap(),
        on_matched: &on_matched,
    };
    let result = read_job_log(request, cancel);
    let printed = match &result {
        Ok(log) => format!("{log:?} {log:#?}"),
        Err(error) => format!("{error} {error:?}"),
    };
    for secret in [TOKEN, SIGNATURE, "sig=", "job-9001.txt"] {
        assert!(!printed.contains(secret), "{secret}: {printed}");
    }
    (result, matched.load(Ordering::SeqCst))
}

/// `bytes` or a little more of numbered log lines, `line 0000000\n` first.
fn numbered(bytes: usize) -> Vec<u8> {
    let mut log = Vec::new();
    let mut line = 0;
    while log.len() < bytes {
        log.extend_from_slice(format!("line {line:07}\n").as_bytes());
        line += 1;
    }
    log
}

#[test]
fn a_matched_job_s_log_is_downloaded_once_without_the_token() {
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: b"\xef\xbb\xbfstep 1\r\nstep 2\n".to_vec(),
        ranges: true,
    });
    script(&server, &storage);
    let (result, matched) = read_log(&server, HEAD, &Cancel::new());
    let log = result.unwrap();
    assert_eq!(log.text, "step 1\nstep 2");
    assert_eq!(log.name, "test \\u{202e}job");
    assert_eq!(
        (log.lines, log.cut, log.head),
        (2, LogCut::Whole, oid(HEAD))
    );
    assert_eq!(matched, 1);
    let downloads = storage.recorded();
    assert_eq!(
        downloads.len(),
        1,
        "exactly one request to the storage host"
    );
    let download = &downloads[0];
    assert_eq!(download.method, "GET");
    assert_eq!(
        download.target,
        format!("/logs/job-{JOB}.txt?sv=2025&sig={SIGNATURE}")
    );
    assert_eq!(download.header("authorization"), None);
    assert_eq!(download.header("x-github-api-version"), None);
    assert_ne!(
        download.header("accept"),
        Some("application/vnd.github+json")
    );
    assert_eq!(
        download.header("range"),
        Some(format!("bytes=-{LOG_BYTES}").as_str())
    );
    for (name, value) in &download.headers {
        assert!(
            !value.contains(TOKEN) && !value.contains("Bearer") && !name.contains("auth"),
            "{name}: {value}"
        );
    }
    assert!(download.body.is_empty() && !download.target.contains(TOKEN));
    assert_eq!(
        (
            server.job_reads(REPOSITORY, JOB),
            server.log_requests(REPOSITORY, JOB)
        ),
        (1, 1)
    );
    for request in server.recorded() {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str()),
            "{}",
            request.target
        );
    }
    assert_eq!(server.mutation_requests(), 0);
}

#[test]
fn only_the_log_request_reads_a_redirect() {
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: b"log\n".to_vec(),
        ranges: true,
    });
    server.redirect_to(&signed(&storage));
    let host = host_of(&server);
    let token = token();
    let error = super::api::Session::new(&host, &token)
        .get("read", &["redirect"], "", &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, GitHubError::Redirected { status: 302, .. }),
        "{error}"
    );
    server.script_job(REPOSITORY, JOB, "302 Found", "{}");
    redirect_to(&server, &signed(&storage));
    let (result, matched) = read_log(&server, HEAD, &Cancel::new());
    assert!(
        matches!(
            result,
            Err(JobLogError::Failed(GitHubError::Redirected {
                status: 302,
                ..
            }))
        ),
        "{result:?}"
    );
    assert_eq!(matched, 0);
    assert_eq!(server.log_requests(REPOSITORY, JOB), 0);
    assert_eq!(storage.requests(), 0, "no ordinary call follows a redirect");
}

#[test]
fn a_redirect_from_the_storage_host_is_an_error_and_never_a_second_hop() {
    let server = server();
    let second = StorageServer::start(StorageBehaviour::Log {
        body: b"log\n".to_vec(),
        ranges: true,
    });
    let storage = StorageServer::start(StorageBehaviour::Answer {
        status: "302 Found",
        headers: vec![("Location".to_owned(), signed(&second))],
        body: Vec::new(),
    });
    script(&server, &storage);
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    let Err(JobLogError::Failed(error @ GitHubError::Redirected { status: 302, .. })) = &result
    else {
        panic!("{result:?}");
    };
    assert!(error.to_string().contains(&storage.origin()), "{error}");
    assert_eq!((storage.requests(), second.requests()), (1, 0));
}

#[test]
fn a_refused_download_address_gets_no_request() {
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: b"log\n".to_vec(),
        ranges: true,
    });
    let origin = storage.origin();
    let at = origin.trim_start_matches("http://").to_owned();
    let port = at.rsplit_once(':').unwrap().1.to_owned();
    server.script_job(REPOSITORY, JOB, "200 OK", &job_body(HEAD, RUN, "completed"));
    for location in [
        format!("http://storage.example:{port}/x?sig={SIGNATURE}"),
        format!("http://user:{SIGNATURE}@{at}/x"),
        format!("{origin}\\@storage.example/x?sig={SIGNATURE}"),
        format!("https://10.0.0.1/x?sig={SIGNATURE}"),
        format!("https://[::1]:{port}/x?sig={SIGNATURE}"),
        format!("http://127.0.0.2:{port}/x?sig={SIGNATURE}"),
        format!("{origin}/x\ty?sig={SIGNATURE}"),
        format!("{origin}/x y?sig={SIGNATURE}"),
        format!("{origin}/x?sig={SIGNATURE}&pad={}", "a".repeat(5000)),
        format!("ftp://{at}/x?sig={SIGNATURE}"),
        format!("/logs/x?sig={SIGNATURE}"),
    ] {
        redirect_to(&server, &location);
        let (result, matched) = read_log(&server, HEAD, &Cancel::new());
        assert!(
            matches!(
                result,
                Err(JobLogError::Failed(GitHubError::LogDownloadRefused { .. }))
            ),
            "{location}: {result:?}"
        );
        assert_eq!(matched, 1, "{location}");
    }
    assert_eq!(storage.requests(), 0, "no refused address was contacted");
}

#[test]
fn the_download_address_reaches_no_message() {
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: Vec::new(),
        ranges: true,
    });
    script(&server, &storage);
    let echo = format!("<Error>{}</Error>", signed(&storage)).into_bytes();
    storage.set(StorageBehaviour::Answer {
        status: "403 Forbidden",
        headers: Vec::new(),
        body: echo,
    });
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    let error = result.unwrap_err().to_string();
    let at = storage.origin().trim_start_matches("http://").to_owned();
    assert_eq!(
        error,
        format!("download the job log from {at}: the storage host answered HTTP 403")
    );
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    redirect_to(
        &server,
        &format!("http://127.0.0.1:{port}/logs/job-{JOB}.txt?sv=2025&sig={SIGNATURE}"),
    );
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    let Err(JobLogError::Failed(error @ GitHubError::Transport { .. })) = &result else {
        panic!("{result:?}");
    };
    assert!(
        error
            .to_string()
            .contains(&format!("http://127.0.0.1:{port}/…")),
        "{error}"
    );
}

#[test]
fn a_job_that_does_not_match_gets_no_log_request() {
    let missing = json!({ "id": JOB, "run_id": RUN, "name": "x", "status": "completed" });
    let bad_head = json!({ "id": JOB, "run_id": RUN, "head_sha": "not a commit", "name": "x",
        "status": "completed" });
    let other_job = json!({ "id": 1, "run_id": RUN, "head_sha": HEAD, "name": "x",
        "status": "completed" });
    for (status, body, why) in [
        (
            "200 OK",
            job_body(MOVED, RUN, "completed"),
            "ran on commit 89abcde",
        ),
        (
            "200 OK",
            job_body(HEAD, 42, "completed"),
            "belongs to workflow run 42",
        ),
        ("200 OK", other_job.to_string(), "answered for job 1"),
        ("200 OK", missing.to_string(), "lacks"),
        ("200 OK", bad_head.to_string(), "lacks"),
        (
            "404 Not Found",
            r#"{"message":"Not Found"}"#.to_owned(),
            "knows no job 9001",
        ),
    ] {
        let server = server();
        let storage = StorageServer::start(StorageBehaviour::Log {
            body: b"log\n".to_vec(),
            ranges: true,
        });
        script(&server, &storage);
        server.script_job(REPOSITORY, JOB, status, &body);
        let (result, matched) = read_log(&server, HEAD, &Cancel::new());
        let Err(error @ JobLogError::NotMatched { .. }) = &result else {
            panic!("{why}: {result:?}");
        };
        assert!(
            error.to_string().starts_with(
                "this check's log could not be matched to the pull request's head commit: "
            ) && error.to_string().contains(why),
            "{error}"
        );
        assert_eq!(matched, 0, "{why}");
        assert_eq!(server.log_requests(REPOSITORY, JOB), 0, "{why}");
        assert_eq!(storage.requests(), 0, "{why}");
    }
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: b"log\n".to_vec(),
        ranges: true,
    });
    script(&server, &storage);
    server.script_job(
        REPOSITORY,
        JOB,
        "200 OK",
        &job_body(HEAD, RUN, "in_progress"),
    );
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    assert!(
        matches!(result, Err(JobLogError::NotFinished { .. })),
        "{result:?}"
    );
    server.script_job(
        REPOSITORY,
        JOB,
        "401 Unauthorized",
        r#"{"message":"Bad credentials"}"#,
    );
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    assert!(
        matches!(
            result,
            Err(JobLogError::Failed(GitHubError::Unauthorized { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(server.log_requests(REPOSITORY, JOB), 0);
    assert_eq!(storage.requests(), 0);
}

#[test]
fn an_oversized_log_is_kept_to_its_cap_and_says_which_end() {
    let server = server();
    let big = numbered(LOG_BYTES + 100_000);
    let last = String::from_utf8_lossy(&big)
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: big.clone(),
        ranges: true,
    });
    script(&server, &storage);
    let log = read_log(&server, HEAD, &Cancel::new()).0.unwrap();
    let LogCut::Tail { kept } = log.cut else {
        panic!("{log:?}");
    };
    assert!(kept <= LOG_BYTES && kept > LOG_BYTES - 20, "{kept}");
    assert!(log.text.ends_with(&last), "the end of the log is kept");
    assert!(!log.text.contains("line 0000000"));
    assert_eq!(log.lines, LOG_LINES);
    assert!(log.lines_left_out > 0);
    storage.set(StorageBehaviour::Log {
        body: big,
        ranges: false,
    });
    let log = read_log(&server, HEAD, &Cancel::new()).0.unwrap();
    assert_eq!(log.cut, LogCut::Head { kept: LOG_BYTES });
    assert!(log.text.starts_with("line 0000000\nline 0000001\n"));
    storage.set(StorageBehaviour::Streamed {
        bytes: 4 * LOG_BYTES,
    });
    let log = read_log(&server, HEAD, &Cancel::new()).0.unwrap();
    assert_eq!(log.cut, LogCut::Head { kept: LOG_BYTES });
    storage.set(StorageBehaviour::Answer {
        status: "206 Partial Content",
        headers: vec![("Content-Range".to_owned(), "bytes 0-3/100".to_owned())],
        body: b"head".to_vec(),
    });
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    assert!(
        matches!(
            result,
            Err(JobLogError::Failed(GitHubError::LogDownload { .. }))
        ),
        "a range that is not the end is not shown as the log: {result:?}"
    );
}

#[test]
fn hostile_log_text_is_neutralized_and_capped() {
    let server = server();
    let long = "x".repeat(LOG_LINE_CHARS + 50);
    let hostile = format!(
        "\u{1b}[31mred\u{1b}[0m \u{1b}]8;;https://evil.example\u{7}click\u{1b}]8;;\u{7}\r\n\
         bidi \u{202e}cba\u{2066}\u{200f}\n\
         nul \u{0} del \u{7f} c1 \u{9b}31m zwsp \u{200b} tag \u{e0041} sep \u{2028}\rtab\there\n\
         {long}\n"
    );
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: hostile.into_bytes(),
        ranges: true,
    });
    script(&server, &storage);
    let log = read_log(&server, HEAD, &Cancel::new()).0.unwrap();
    let lines: Vec<&str> = log.text.split('\n').collect();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert_eq!(lines[0], "red click");
    assert_eq!(lines[1], "bidi \\u{202e}cba\\u{2066}\\u{200f}");
    assert_eq!(
        lines[2],
        "nul \\u{0} del \\u{7f} c1 \\u{9b}31m zwsp \\u{200b} tag \\u{e0041} sep \\u{2028}"
    );
    assert_eq!(lines[3], "tab\there");
    assert_eq!(lines[4], format!("{}…", "x".repeat(LOG_LINE_CHARS)));
    assert_eq!((log.lines, log.lines_shortened), (5, 1));
    for c in log.text.chars() {
        assert!(
            !super::text::is_escaped_on_one_line(c) || c == '\n' || c == '\t',
            "{:x}",
            u32::from(c)
        );
    }
}

#[test]
fn a_cancel_stops_the_download_and_a_silent_storage_host_ends_at_the_idle_budget() {
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Stall);
    script(&server, &storage);
    let cancel = Cancel::new();
    let raise = cancel.clone();
    let started = Instant::now();
    let raiser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        raise.signal();
    });
    let (result, _) = read_log(&server, HEAD, &cancel);
    raiser.join().unwrap();
    assert!(
        matches!(
            result,
            Err(JobLogError::Failed(GitHubError::Cancelled { .. }))
        ),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let idle = Cancel::new().with_idle_timeout(Duration::from_secs(1));
    let started = Instant::now();
    let (result, _) = read_log(&server, HEAD, &idle);
    assert!(
        matches!(
            result,
            Err(JobLogError::Failed(GitHubError::Transport { .. }))
        ),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(storage.requests(), 2);
}

#[test]
fn a_repository_on_another_host_gets_nothing() {
    let account = server();
    let elsewhere = server();
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: b"log\n".to_vec(),
        ranges: true,
    });
    script(&elsewhere, &storage);
    let host = host_of(&account);
    let token = token();
    let foreign = target(&host_of(&elsewhere));
    let request = JobLogRequest {
        host: &host,
        token: &token,
        target: &foreign,
        head: oid(HEAD),
        run: RunId::new(RUN).unwrap(),
        job: JobId::new(JOB).unwrap(),
        on_matched: &|| {},
    };
    let result = read_job_log(request, &Cancel::new());
    assert!(
        matches!(
            result,
            Err(JobLogError::Failed(GitHubError::ForeignRemote { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(
        account.requests() + elsewhere.requests() + storage.requests(),
        0
    );
}

/// A storage host that refuses suffix ranges (`416`) still gives the log: one more `GET` to the
/// same address without a range, kept to the cap from its start, and nothing else.
#[test]
fn a_storage_host_refusing_the_range_gets_one_more_request_without_one() {
    let server = server();
    let body = numbered(LOG_BYTES + 4096);
    let storage = StorageServer::start(StorageBehaviour::RangeRefused { body });
    script(&server, &storage);
    let (result, matched) = read_log(&server, HEAD, &Cancel::new());
    let log = result.unwrap();
    assert_eq!(log.cut, LogCut::Head { kept: LOG_BYTES });
    assert!(
        log.text.starts_with("line 0000000\nline 0000001\n"),
        "the log's start"
    );
    assert_eq!(matched, 1);
    let downloads = storage.recorded();
    assert_eq!(
        downloads.len(),
        2,
        "the ranged request, then one without a range"
    );
    assert_eq!(
        downloads[0].header("range"),
        Some(format!("bytes=-{LOG_BYTES}").as_str())
    );
    assert_eq!(downloads[1].header("range"), None);
    for download in &downloads {
        assert_eq!(download.method, "GET");
        assert_eq!(
            download.target,
            format!("/logs/job-{JOB}.txt?sv=2025&sig={SIGNATURE}"),
            "the same address"
        );
        assert_eq!(download.header("authorization"), None);
        assert_eq!(download.header("x-github-api-version"), None);
        for (name, value) in &download.headers {
            assert!(
                !value.contains(TOKEN) && !value.contains("Bearer") && !name.contains("auth"),
                "{name}: {value}"
            );
        }
    }
    assert_eq!(
        server.log_requests(REPOSITORY, JOB),
        1,
        "GitHub is asked once"
    );
}

#[test]
fn a_storage_host_answering_416_twice_ends_in_an_error_after_exactly_two_requests() {
    let server = server();
    let storage = StorageServer::start(StorageBehaviour::Answer {
        status: "416 Range Not Satisfiable",
        headers: vec![("Content-Range".to_owned(), "bytes */99".to_owned())],
        body: b"<Error>InvalidRange</Error>".to_vec(),
    });
    script(&server, &storage);
    let (result, _) = read_log(&server, HEAD, &Cancel::new());
    let Err(JobLogError::Failed(error @ GitHubError::LogDownload { .. })) = &result else {
        panic!("{result:?}");
    };
    assert!(error.to_string().contains("HTTP 416"), "{error}");
    assert!(!error.to_string().contains("InvalidRange"), "{error}");
    assert_eq!(storage.requests(), 2, "never a third");
}

#[test]
fn no_other_answer_to_the_ranged_request_sends_a_second() {
    for (status, range, ok) in [
        ("403 Forbidden", None, false),
        ("404 Not Found", None, false),
        ("500 Internal Server Error", None, false),
        ("503 Service Unavailable", Some("bytes */99"), false),
        ("416 Range Not Satisfiable", Some("bytes */0"), true),
        ("200 OK", None, true),
    ] {
        let server = server();
        let headers = range
            .map(|range| vec![("Content-Range".to_owned(), range.to_owned())])
            .unwrap_or_default();
        let storage = StorageServer::start(StorageBehaviour::Answer {
            status,
            headers,
            body: Vec::new(),
        });
        script(&server, &storage);
        let (result, _) = read_log(&server, HEAD, &Cancel::new());
        assert_eq!(result.is_ok(), ok, "{status}: {result:?}");
        assert_eq!(storage.requests(), 1, "{status}: one request only");
        assert_eq!(
            storage.recorded()[0].header("range"),
            Some(format!("bytes=-{LOG_BYTES}").as_str()),
            "{status}"
        );
    }
}
