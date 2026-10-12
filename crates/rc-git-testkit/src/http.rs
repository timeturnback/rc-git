//! A minimal smart-HTTP git server for tests: a TCP listener that CGI-execs
//! `git http-backend` per request. Plain HTTP on loopback only; production code
//! accepts `http://` for loopback hosts exactly so these tests run end to end.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

/// Serves one repository over git's smart HTTP protocol from a background thread.
/// Dropped, it stops listening and joins the thread.
#[derive(Debug)]
pub struct HttpGitServer {
    port: u16,
    mount: String,
    shutdown: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

/// Username and password a server built with [`HttpGitServer::serve_with_auth`] demands.
#[derive(Debug, Clone)]
struct BasicAuth {
    header: String,
}

/// How the server answers each connection.
#[derive(Debug, Clone)]
enum Mode {
    /// Runs `git http-backend`, demanding Basic auth first when set.
    Backend(Option<BasicAuth>),
    /// Reads the request head, then holds the socket open and never writes.
    Stall,
}

impl HttpGitServer {
    /// Serves `git_dir` anonymously.
    ///
    /// # Panics
    /// When the listener cannot bind or `git --exec-path` fails.
    #[must_use]
    pub fn serve(git_dir: &std::path::Path) -> Self {
        Self::start(git_dir, Mode::Backend(None))
    }

    /// Accepts every connection and reads the request head, then holds the socket open
    /// without writing a byte until the server is dropped: a peer that went quiet
    /// mid-request, for testing idle timeouts. `git_dir` only names the mount, so
    /// [`HttpGitServer::url`] has the usual shape.
    ///
    /// # Panics
    /// When the listener cannot bind or `git --exec-path` fails.
    #[must_use]
    pub fn serve_stalling(git_dir: &std::path::Path) -> Self {
        Self::start(git_dir, Mode::Stall)
    }

    /// Serves `git_dir` behind HTTP Basic auth: requests without the exact
    /// `user:pass` credentials get `401` with a `WWW-Authenticate` challenge.
    ///
    /// # Panics
    /// When the listener cannot bind or `git --exec-path` fails.
    #[must_use]
    pub fn serve_with_auth(git_dir: &std::path::Path, user: &str, pass: &str) -> Self {
        let auth = BasicAuth {
            header: format!("Basic {}", base64(format!("{user}:{pass}").as_bytes())),
        };
        Self::start(git_dir, Mode::Backend(Some(auth)))
    }

    /// URL of the served repository, e.g. `http://127.0.0.1:49321/fixture.git`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}", self.port, self.mount)
    }

    /// Connections accepted so far, each one HTTP request. The shutdown connection made
    /// on drop is not counted.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn start(git_dir: &std::path::Path, mode: Mode) -> Self {
        let git_dir = git_dir.canonicalize().expect("canonicalize git dir");
        let project_root = git_dir
            .parent()
            .expect("git dir has a parent")
            .to_path_buf();
        let mount = git_dir
            .file_name()
            .expect("git dir has a name")
            .to_string_lossy()
            .into_owned();
        let backend = git_http_backend();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);
        let requests = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                accepted.fetch_add(1, Ordering::SeqCst);
                let Ok(stream) = stream else { continue };
                let root = project_root.clone();
                let backend = backend.clone();
                let mode = mode.clone();
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || match mode {
                    Mode::Backend(auth) => handle_connection(stream, &root, &backend, auth),
                    Mode::Stall => stall(stream, &stop),
                });
            }
        });
        Self {
            port,
            mount,
            shutdown,
            requests,
            handle: Some(handle),
        }
    }
}

impl Drop for HttpGitServer {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test kit opens a loopback connection to stop its own server"
    )]
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unblock the accept loop with one throwaway connection.
        drop(TcpStream::connect(("127.0.0.1", self.port)));
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

/// Absolute path of `git-http-backend`, resolved once per process via `git --exec-path`
/// because it is not on `PATH`.
fn git_http_backend() -> PathBuf {
    static BACKEND: OnceLock<PathBuf> = OnceLock::new();
    BACKEND.get_or_init(resolve_git_http_backend).clone()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test kit shells out to the git CLI by design"
)]
fn resolve_git_http_backend() -> PathBuf {
    let output = Command::new("git")
        .arg("--exec-path")
        .output()
        .expect("run git --exec-path");
    assert!(output.status.success(), "git --exec-path failed");
    let exec_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    PathBuf::from(exec_path).join("git-http-backend")
}

/// Reads the request line and headers, then keeps the connection open and silent until
/// `stop` is set, so the client's reads block for as long as it is willing to wait.
fn stall(stream: TcpStream, stop: &AtomicBool) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.trim_end().is_empty() => break,
            Ok(_) => {}
        }
    }
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(reader);
}

/// The request line and the headers the server acts on.
#[derive(Debug, Default)]
struct RequestHead {
    method: String,
    target: String,
    content_type: String,
    content_length: Option<usize>,
    chunked: bool,
    expects_continue: bool,
    authorization: String,
    /// Every header in arrival order, for the `HTTP_*` CGI variables.
    headers: Vec<(String, String)>,
}

/// What `git http-backend` needs to answer one request, as CGI variables plus the body.
struct CgiRequest<'a> {
    method: &'a str,
    path_info: &'a str,
    query_string: &'a str,
    content_type: &'a str,
    headers: &'a [(String, String)],
    body: &'a [u8],
}

fn handle_connection(
    stream: TcpStream,
    project_root: &std::path::Path,
    backend: &std::path::Path,
    auth: Option<BasicAuth>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;
    let Some(head) = read_head(&mut reader) else {
        return;
    };
    if let Some(auth) = auth
        && head.authorization != auth.header
    {
        refuse_unauthorized(&mut writer);
        return;
    }
    if head.expects_continue {
        drop(writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n"));
    }
    let Some(body) = read_body(&mut reader, &head) else {
        let reason = b"malformed or truncated request body\n";
        write_response(&mut writer, "400 Bad Request", TEXT_PLAIN, reason);
        return;
    };
    let (path_info, query_string) = head.target.split_once('?').unwrap_or((&head.target, ""));
    let request = CgiRequest {
        method: &head.method,
        path_info,
        query_string,
        content_type: &head.content_type,
        headers: &head.headers,
        body: &body,
    };
    let output = run_backend(backend, project_root, &request);
    respond(&mut writer, &output);
}

/// Reads the request line and headers up to the blank line; `None` when the peer sent
/// nothing or the read failed.
fn read_head(reader: &mut BufReader<TcpStream>) -> Option<RequestHead> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let mut head = RequestHead {
        method: parts.next().unwrap_or_default().to_owned(),
        target: parts.next().unwrap_or_default().to_owned(),
        ..RequestHead::default()
    };
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            return Some(head);
        }
        if let Some((name, value)) = line.split_once(':') {
            apply_header(&mut head, name, value.trim());
        }
    }
}

fn apply_header(head: &mut RequestHead, name: &str, value: &str) {
    match name.to_ascii_lowercase().as_str() {
        "content-type" => value.clone_into(&mut head.content_type),
        "content-length" => head.content_length = value.parse().ok(),
        "transfer-encoding" => head.chunked = value.eq_ignore_ascii_case("chunked"),
        "expect" => head.expects_continue = value.eq_ignore_ascii_case("100-continue"),
        "authorization" => value.clone_into(&mut head.authorization),
        _ => {}
    }
    head.headers.push((name.to_owned(), value.to_owned()));
}

/// The CGI variable for a request header (RFC 3875 4.1.18): `Git-Protocol` becomes
/// `HTTP_GIT_PROTOCOL`. `None` for Content-Type and Content-Length, which have their own
/// CGI names, and for a name that is not a plain token.
fn cgi_header_var(name: &str) -> Option<String> {
    let plain = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let excluded =
        name.eq_ignore_ascii_case("content-type") || name.eq_ignore_ascii_case("content-length");
    (plain && !excluded).then(|| format!("HTTP_{}", name.to_ascii_uppercase().replace('-', "_")))
}

/// Answers `401` with a Basic challenge and closes the connection.
fn refuse_unauthorized(writer: &mut TcpStream) {
    let body = b"authentication required";
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"rc-git-testkit\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    drop(writer.write_all(response.as_bytes()));
    drop(writer.write_all(body));
}

/// Reads a chunked or fixed-length body; `None` when it ends early or a chunk size line
/// does not parse.
fn read_body(reader: &mut impl BufRead, head: &RequestHead) -> Option<Vec<u8>> {
    if head.chunked {
        return read_chunked(reader);
    }
    let mut body = vec![0_u8; head.content_length.unwrap_or(0)];
    reader.read_exact(&mut body).ok()?;
    Some(body)
}

/// What `git http-backend` printed and how it exited.
struct BackendOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    success: bool,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test kit shells out to the git CLI by design"
)]
fn run_backend(
    backend: &std::path::Path,
    project_root: &std::path::Path,
    request: &CgiRequest<'_>,
) -> BackendOutput {
    let mut command = Command::new(backend);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", request.method)
        .env("PATH_INFO", request.path_info)
        .env("QUERY_STRING", request.query_string)
        .env("CONTENT_TYPE", request.content_type)
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for (name, value) in request.headers {
        if let Some(var) = cgi_header_var(name) {
            command.env(var, value);
        }
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git-http-backend");
    let stdin = child.stdin.take().expect("child stdin");
    // The body is written from its own thread so a backend that answers before reading all
    // of it cannot deadlock against a full stdout pipe.
    let (output, written) = std::thread::scope(|scope| {
        let writer = scope.spawn(|| write_body(stdin, request.body));
        let output = child.wait_with_output().expect("wait for git-http-backend");
        (output, writer.join().expect("join the request body writer"))
    });
    let mut stderr = output.stderr;
    if let Err(error) = &written {
        stderr.extend_from_slice(
            format!("rc-git-testkit: writing the request body to git-http-backend: {error}\n")
                .as_bytes(),
        );
    }
    BackendOutput {
        stdout: output.stdout,
        stderr,
        success: output.status.success() && written.is_ok(),
    }
}

/// Writes the request body to the backend and closes its stdin. A backend that refuses a
/// request exits without reading it; that broken pipe is its answer, not a failure here.
fn write_body(mut stdin: impl Write, body: &[u8]) -> std::io::Result<()> {
    match stdin.write_all(body) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

const TEXT_PLAIN: &str = "Content-Type: text/plain; charset=utf-8\r\n";

/// Translates CGI output (headers, blank line, body) into an HTTP/1.1 response. A backend
/// that failed answers `500`, and any non-2xx status carries git's stderr as a text/plain
/// body, which the git client prints as `remote: ...`.
fn respond(writer: &mut impl Write, output: &BackendOutput) {
    let split = output
        .stdout
        .windows(4)
        .position(|window| window == b"\r\n\r\n");
    let Some(split) = split.filter(|_| output.success) else {
        let status = "500 Internal Server Error";
        report_backend_failure(status, &output.stderr);
        write_response(writer, status, TEXT_PLAIN, &output.stderr);
        return;
    };
    let headers = String::from_utf8_lossy(&output.stdout[..split]);
    let mut status = "200 OK".to_owned();
    let mut passthrough = Vec::new();
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("status") {
            value.trim().clone_into(&mut status);
        } else {
            passthrough.push((name, line));
        }
    }
    let success = status.starts_with('2');
    let mut headers = String::new();
    for (name, line) in passthrough {
        if success || !name.eq_ignore_ascii_case("content-type") {
            headers.push_str(line);
            headers.push_str("\r\n");
        }
    }
    if success {
        write_response(writer, &status, &headers, &output.stdout[split + 4..]);
        return;
    }
    report_backend_failure(&status, &output.stderr);
    headers.push_str(TEXT_PLAIN);
    write_response(writer, &status, &headers, &output.stderr);
}

/// Writes one complete response; `headers` is zero or more `Name: value\r\n` lines.
fn write_response(writer: &mut impl Write, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // The client may already have hung up; the test it runs reports that, not this thread.
    drop(writer.write_all(head.as_bytes()));
    drop(writer.write_all(body));
}

#[expect(
    clippy::print_stderr,
    reason = "surface git-http-backend's refusal in the failing test's output"
)]
fn report_backend_failure(status: &str, stderr: &[u8]) {
    eprintln!(
        "rc-git-testkit: git-http-backend answered {status}: {}",
        String::from_utf8_lossy(stderr).trim_end()
    );
}

/// Reads a chunked body through its terminal chunk; `None` on a size line that does not
/// parse or on EOF before the terminal chunk.
fn read_chunked(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let size_line = read_crlf_line(reader)?;
        let size_field = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_field, 16).ok()?;
        if size == 0 {
            // No trailers are sent by git; the blank line ends the body.
            read_crlf_line(reader)?;
            return Some(body);
        }
        let mut chunk = vec![0_u8; size];
        reader.read_exact(&mut chunk).ok()?;
        body.extend_from_slice(&chunk);
        if !read_crlf_line(reader)?.is_empty() {
            return None;
        }
    }
}

/// One line without its line ending; `None` on EOF or a read error.
fn read_crlf_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

/// Standard base64, for the `Authorization: Basic` comparison. Hand-rolled so the kit
/// takes no new dependency for a few lines of test support.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[usize::try_from((triple >> 18) & 63).expect("in range")] as char);
        out.push(ALPHABET[usize::try_from((triple >> 12) & 63).expect("in range")] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[usize::try_from((triple >> 6) & 63).expect("in range")] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[usize::try_from(triple & 63).expect("in range")] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        BackendOutput, CgiRequest, base64, git_http_backend, read_chunked, respond, run_backend,
    };
    use crate::Fixture;

    fn output(stdout: &[u8], stderr: &[u8], success: bool) -> BackendOutput {
        BackendOutput {
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
            success,
        }
    }

    fn response_to(output: &BackendOutput) -> String {
        let mut written = Vec::new();
        respond(&mut written, output);
        String::from_utf8(written).unwrap()
    }

    #[test]
    fn read_chunked_joins_chunks_through_the_terminal_chunk() {
        let mut input = Cursor::new(b"3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\n\r\n".to_vec());
        assert_eq!(read_chunked(&mut input).unwrap(), b"abcde");
    }

    #[test]
    fn read_chunked_rejects_a_size_line_that_does_not_parse() {
        let mut input = Cursor::new(b"3\r\nabc\r\nzz\r\nmore\r\n0\r\n\r\n".to_vec());
        assert_eq!(read_chunked(&mut input), None);
    }

    #[test]
    fn read_chunked_rejects_eof_before_the_terminal_chunk() {
        let mut input = Cursor::new(b"3\r\nabc\r\n".to_vec());
        assert_eq!(read_chunked(&mut input), None);
    }

    #[test]
    fn output_without_a_header_block_answers_500_with_stderr() {
        let response = response_to(&output(b"", b"fatal: broken\n", false));
        assert!(response.starts_with("HTTP/1.1 500 Internal Server Error\r\n"));
        assert!(response.contains("Content-Type: text/plain"));
        assert!(response.ends_with("\r\n\r\nfatal: broken\n"), "{response}");
    }

    #[test]
    fn a_failed_exit_after_a_200_header_answers_500_with_stderr() {
        let stdout = b"Content-Type: application/x-git-upload-pack-result\r\n\r\npartial";
        let stderr = b"fatal: protocol error: bad line length character\n";
        let response = response_to(&output(stdout, stderr, false));
        assert!(response.starts_with("HTTP/1.1 500 Internal Server Error\r\n"));
        assert!(
            response.ends_with("bad line length character\n"),
            "{response}"
        );
        assert!(!response.contains("partial"));
    }

    #[test]
    fn a_non_2xx_status_carries_stderr_as_plain_text() {
        let stdout =
            b"Status: 403 Forbidden\r\nContent-Type: text/html\r\nPragma: no-cache\r\n\r\n";
        let stderr = b"Service not enabled: 'receive-pack'\n";
        let response = response_to(&output(stdout, stderr, true));
        assert!(response.starts_with("HTTP/1.1 403 Forbidden\r\n"));
        assert!(response.contains("Pragma: no-cache\r\n"));
        assert!(!response.contains("text/html"));
        assert!(response.contains("Content-Type: text/plain"));
        assert!(response.ends_with("\r\n\r\nService not enabled: 'receive-pack'\n"));
    }

    #[test]
    fn a_2xx_status_passes_the_cgi_body_through() {
        let stdout = b"Content-Type: application/x-git-upload-pack-advertisement\r\n\r\nrefs";
        let response = response_to(&output(stdout, b"warning: ignored\n", true));
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("application/x-git-upload-pack-advertisement"));
        assert!(response.contains("Content-Length: 4\r\n"));
        assert!(response.ends_with("\r\n\r\nrefs"));
    }

    #[test]
    fn a_refused_push_body_is_not_a_panic_and_keeps_the_reason() {
        let origin = Fixture::standard();
        let (bare, bare_path) = origin.bare_copy();
        bare.git(&[
            "--git-dir",
            "remote.git",
            "config",
            "http.receivepack",
            "false",
        ]);
        let body = vec![b'0'; 4 << 20];
        let request = CgiRequest {
            method: "POST",
            path_info: "/remote.git/git-receive-pack",
            query_string: "",
            content_type: "application/x-git-receive-pack-request",
            headers: &[],
            body: &body,
        };

        let output = run_backend(&git_http_backend(), bare_path.parent().unwrap(), &request);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Service not enabled"), "{stderr}");
        assert!(response_to(&output).starts_with("HTTP/1.1 403 Forbidden\r\n"));
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:sekret"), "dXNlcjpzZWtyZXQ=");
    }
}
