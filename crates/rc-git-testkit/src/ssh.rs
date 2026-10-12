//! A loopback SSH server that speaks just enough of the protocol to serve git: public
//! key auth against fixture keys, then `git-upload-pack`/`git-receive-pack` spawned as
//! real git child processes (the test kit shells out by design). The counterpart of
//! [`crate::HttpGitServer`] for the ssh transport.

use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::keys::ssh_key;
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::io::AsyncWriteExt;
use tokio::runtime::Runtime;

/// Seed for the deterministic host key. Tests never talk to a real host, so a fixed
/// key keeps fixtures reproducible.
const HOST_KEY_SEED: [u8; 32] = [7; 32];
/// Seed for the deterministic client key `write_client_key` writes.
const CLIENT_KEY_SEED: [u8; 32] = [42; 32];
/// Seed for a valid key the server does not accept.
const UNAUTHORIZED_KEY_SEED: [u8; 32] = [13; 32];
/// Seed for a host key that contradicts the server's real one.
const MISMATCHED_HOST_KEY_SEED: [u8; 32] = [9; 32];

/// An OpenSSH private key encrypted with [`ENCRYPTED_CLIENT_KEY_PASSPHRASE`], for
/// exercising the passphrase path. Generated once with
/// `ssh-keygen -t ed25519 -N <passphrase>`.
const ENCRYPTED_CLIENT_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABCCPgzi4v
g6PFwFgKS8QUE0AAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIBCg6n01Kgq0r8iC
Oc4Y/wDGwM3l/a/8tj2lTVsgkVTIAAAAoDLc2OAjyFWezLgGLFs6QUE9vN5rMYSJ9fYSY0
ZNjv7uWfoALsL2ym+wfH4wjWduYe/c2RKHMoi9gRK+FKT9H0jsDDADhNZiLt5UQq+f8uzq
voKrQdyMynAlth5QGDSAuZBD2J+LPvULVwkq1zPMEumT0URRk7/lOZ7umQ0KL7k7E5+VX5
kSg4N213wWeUQUEx5qmJrcMB/UoZyW+CtmZow=
-----END OPENSSH PRIVATE KEY-----
";

/// The passphrase [`ENCRYPTED_CLIENT_KEY`] is encrypted with.
const ENCRYPTED_CLIENT_KEY_PASSPHRASE: &str = "rc-git-test-passphrase";

/// Public half of [`ENCRYPTED_CLIENT_KEY`].
const ENCRYPTED_CLIENT_KEY_PUB: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBCg6n01Kgq0r8iCOc4Y/wDGwM3l/a/8tj2lTVsgkVTI";

/// A running loopback SSH server serving one bare repository.
pub struct SshGitServer {
    addr: SocketAddr,
    git_dir: PathBuf,
    host_key: ssh_key::PrivateKey,
    runtime: Option<Runtime>,
}

impl SshGitServer {
    /// Starts a server for the bare repository (or `.git` directory) at `git_dir`.
    /// Publickey auth accepts the fixture client keys; the exec handler runs real git.
    ///
    /// # Panics
    /// On any setup failure — a broken fixture must fail loudly.
    #[must_use]
    pub fn serve(git_dir: &Path) -> Self {
        Self::start(git_dir, Mode::Serve)
    }

    /// Starts a server that authenticates like [`SshGitServer::serve`] and then never
    /// answers a session-channel open, for exercising the client's connect budget.
    ///
    /// # Panics
    /// On any setup failure.
    #[must_use]
    pub fn serve_never_confirming_channels(git_dir: &Path) -> Self {
        Self::start(git_dir, Mode::StallChannels)
    }

    /// Starts a server that authenticates like [`SshGitServer::serve`] and then answers
    /// every git command the way GitHub answers a key whose account cannot see the
    /// repository: `ERROR: Repository not found.` on stderr and exit status 1.
    ///
    /// # Panics
    /// On any setup failure.
    #[must_use]
    pub fn serve_refusing_repositories(git_dir: &Path) -> Self {
        Self::start(git_dir, Mode::RefuseRepositories)
    }

    #[expect(
        clippy::print_stderr,
        reason = "the test double's only channel into the failing test's captured output"
    )]
    fn start(git_dir: &Path, mode: Mode) -> Self {
        let host_key = key_from_seed(&HOST_KEY_SEED);
        let allowed = vec![
            key_from_seed(&CLIENT_KEY_SEED).public_key().clone(),
            encrypted_client_public_key(),
        ];
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .build()
            .expect("start tokio for the ssh server");
        let config = Arc::new(russh::server::Config {
            keys: vec![host_key.clone()],
            auth_rejection_time: std::time::Duration::from_millis(10),
            auth_rejection_time_initial: Some(std::time::Duration::ZERO),
            ..Default::default()
        });
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("bind the ssh listener");
        let addr = listener.local_addr().expect("read the bound address");
        let mut server = GitSshServer {
            git_dir: git_dir.to_path_buf(),
            allowed,
            mode,
        };
        runtime.spawn(async move {
            let running = server.run_on_socket(config, &listener);
            if let Err(error) = running.await {
                eprintln!("ssh test server: accept loop stopped: {error}");
            }
        });
        Self {
            addr,
            git_dir: git_dir.to_path_buf(),
            host_key,
            runtime: Some(runtime),
        }
    }

    /// The ssh URL of the served repository, e.g. `ssh://git@127.0.0.1:49152/tmp/x.git`.
    #[must_use]
    pub fn url(&self) -> String {
        format!(
            "ssh://git@127.0.0.1:{}{}",
            self.addr.port(),
            self.git_dir.display()
        )
    }

    /// The port the server listens on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The host key's `SHA256:...` fingerprint, as rc-git reports it to the user.
    #[must_use]
    pub fn host_fingerprint(&self) -> String {
        self.host_key
            .public_key()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
    }

    /// Writes the plain fixture client key as `<dir>/id_ed25519`.
    ///
    /// # Panics
    /// On I/O failure.
    pub fn write_client_key(&self, dir: &Path) {
        let key = key_from_seed(&CLIENT_KEY_SEED);
        let pem = key
            .to_openssh(ssh_key::LineEnding::LF)
            .expect("encode the client key");
        write_private(&dir.join("id_ed25519"), pem.as_bytes());
    }

    /// Writes the passphrase-protected fixture client key as `<dir>/id_ed25519` and
    /// returns the passphrase that opens it.
    ///
    /// # Panics
    /// On I/O failure.
    pub fn write_encrypted_client_key(&self, dir: &Path) -> &'static str {
        write_private(&dir.join("id_ed25519"), ENCRYPTED_CLIENT_KEY.as_bytes());
        ENCRYPTED_CLIENT_KEY_PASSPHRASE
    }

    /// Appends this server's host key to `<dir>/known_hosts` in OpenSSH format.
    ///
    /// # Panics
    /// On I/O failure.
    pub fn write_known_hosts(&self, dir: &Path) {
        append_known_hosts_line(dir, self.addr.port(), self.host_key.public_key());
    }

    /// Appends a `known_hosts` entry for this server's address that names a
    /// *different* key, so connecting must fail as a host-key mismatch.
    ///
    /// # Panics
    /// On I/O failure.
    pub fn write_mismatched_known_hosts(&self, dir: &Path) {
        let wrong = key_from_seed(&MISMATCHED_HOST_KEY_SEED);
        append_known_hosts_line(dir, self.addr.port(), wrong.public_key());
    }

    /// Writes a valid ed25519 key the server rejects as `<dir>/id_ed25519`.
    ///
    /// # Panics
    /// On I/O failure.
    pub fn write_unauthorized_client_key(&self, dir: &Path) {
        let key = key_from_seed(&UNAUTHORIZED_KEY_SEED);
        let pem = key
            .to_openssh(ssh_key::LineEnding::LF)
            .expect("encode the client key");
        write_private(&dir.join("id_ed25519"), pem.as_bytes());
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test kit builds the fixture's ssh directory; the library never writes one"
)]
fn append_known_hosts_line(dir: &Path, port: u16, public: &ssh_key::PublicKey) {
    let public = public.to_openssh().expect("encode the host key");
    let line = format!("[127.0.0.1]:{port} {public}\n");
    let path = dir.join("known_hosts");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("open known_hosts");
    file.write_all(line.as_bytes()).expect("write known_hosts");
}

impl Drop for SshGitServer {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn key_from_seed(seed: &[u8; 32]) -> ssh_key::PrivateKey {
    ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(seed))
}

fn encrypted_client_public_key() -> ssh_key::PublicKey {
    ENCRYPTED_CLIENT_KEY_PUB
        .parse()
        .expect("parse the embedded public key")
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test kit writes fixture files by design"
)]
fn write_private(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write the key file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict the key file");
    }
}

/// How the server answers once a client has authenticated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Run the requested git service against the repository.
    Serve,
    /// Never answer a session-channel open.
    StallChannels,
    /// Refuse every git command with GitHub's "Repository not found" on stderr.
    RefuseRepositories,
}

/// Accepts connections; every client gets a fresh handler over the shared state.
struct GitSshServer {
    git_dir: PathBuf,
    allowed: Vec<ssh_key::PublicKey>,
    mode: Mode,
}

impl russh::server::Server for GitSshServer {
    type Handler = GitSshHandler;

    fn new_client(&mut self, _peer: Option<SocketAddr>) -> Self::Handler {
        GitSshHandler {
            git_dir: self.git_dir.clone(),
            allowed: self.allowed.clone(),
            mode: self.mode,
            channels: HashMap::new(),
            env: HashMap::new(),
        }
    }

    #[expect(
        clippy::print_stderr,
        reason = "the test double's only channel into the failing test's captured output"
    )]
    fn handle_session_error(&mut self, error: russh::Error) {
        eprintln!("ssh test server: session failed: {error:?}");
    }
}

struct GitSshHandler {
    git_dir: PathBuf,
    allowed: Vec<ssh_key::PublicKey>,
    mode: Mode,
    channels: HashMap<ChannelId, Channel<Msg>>,
    env: HashMap<ChannelId, String>,
}

impl russh::server::Handler for GitSshHandler {
    type Error = russh::Error;

    // The trait's contract is async; these checks are synchronous, so they return
    // ready futures instead of carrying an unused `async`.

    // Rejecting unknown keys at the offer stage keeps a developer's real ssh-agent from
    // ever being asked to sign during a test run.
    fn auth_publickey_offered(
        &mut self,
        _user: &str,
        public_key: &ssh_key::PublicKey,
    ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
        let auth = if self.allowed.iter().any(|key| key == public_key) {
            Auth::Accept
        } else {
            Auth::reject()
        };
        std::future::ready(Ok(auth))
    }

    fn auth_publickey(
        &mut self,
        _user: &str,
        public_key: &ssh_key::PublicKey,
    ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
        let auth = if self.allowed.iter().any(|key| key == public_key) {
            Auth::Accept
        } else {
            Auth::reject()
        };
        std::future::ready(Ok(auth))
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.mode == Mode::StallChannels {
            std::future::pending::<()>().await;
        }
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        if name == "GIT_PROTOCOL" {
            self.env.insert(channel, value.to_owned());
        }
        std::future::ready(session.channel_success(channel))
    }

    fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(self.exec(channel_id, data, session))
    }
}

impl GitSshHandler {
    #[expect(
        clippy::print_stderr,
        reason = "the test double's only channel into the failing test's captured output"
    )]
    fn exec(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        let Some(channel) = self.channels.remove(&channel_id) else {
            return Err(russh::Error::WrongChannel);
        };
        let Some(subcommand) = std::str::from_utf8(data)
            .ok()
            .and_then(|command| parse_git_command(command, &self.git_dir))
        else {
            eprintln!(
                "ssh test server: refused exec {:?}; serving {}",
                String::from_utf8_lossy(data),
                self.git_dir.display()
            );
            return Err(russh::Error::Inconsistent);
        };
        let git_protocol = self.env.remove(&channel_id);
        let child = if self.mode == Mode::RefuseRepositories {
            spawn_refusal()
        } else {
            spawn_git(subcommand, &self.git_dir, git_protocol)
        };
        session.channel_success(channel_id)?;
        let handle = session.handle();
        tokio::spawn(pump(channel, child, handle, channel_id));
        Ok(())
    }
}

/// Validates `git-upload-pack '<path>'` / `git-receive-pack '<path>'` against the one
/// served repository and returns the git subcommand to run.
fn parse_git_command(command: &str, git_dir: &Path) -> Option<&'static str> {
    let (subcommand, rest) = command
        .strip_prefix("git-upload-pack ")
        .map(|rest| ("upload-pack", rest))
        .or_else(|| {
            command
                .strip_prefix("git-receive-pack ")
                .map(|rest| ("receive-pack", rest))
        })?;
    let path = rest.strip_prefix('\'')?.strip_suffix('\'')?;
    (Path::new(path) == git_dir).then_some(subcommand)
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test kit shells out to the git CLI by design"
)]
fn spawn_git(
    subcommand: &str,
    git_dir: &Path,
    git_protocol: Option<String>,
) -> tokio::process::Child {
    let mut command = tokio::process::Command::new("git");
    command
        .arg(subcommand)
        .arg(git_dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", git_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(value) = git_protocol {
        command.env("GIT_PROTOCOL", value);
    }
    command.spawn().expect("spawn git for the ssh server")
}

/// A child that refuses the way GitHub's git service does for a repository the
/// authenticated account cannot see.
#[expect(
    clippy::disallowed_methods,
    reason = "the test kit shells out by design"
)]
fn spawn_refusal() -> tokio::process::Child {
    tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("printf 'ERROR: Repository not found.\\n' >&2; exit 1")
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the refusing command for the ssh server")
}

/// Pipes the channel to the child and back, the child's stderr as extended data type 1
/// like sshd. The child's stdout and stderr EOF — not the client's — drive completion:
/// exit status, channel EOF, and close follow immediately, so a client blocked in
/// `read_to_end` is released even while it holds its own write side open.
async fn pump(
    channel: Channel<Msg>,
    mut child: tokio::process::Child,
    handle: russh::server::Handle,
    id: ChannelId,
) {
    let Some(mut stdin) = child.stdin.take() else {
        return;
    };
    let Some(mut stdout) = child.stdout.take() else {
        return;
    };
    let Some(mut stderr) = child.stderr.take() else {
        return;
    };
    let mut stderr_to_client = channel.make_writer_ext(Some(1));
    let (mut from_client, mut to_client) = tokio::io::split(channel.into_stream());
    let uplink = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut from_client, &mut stdin).await;
        // Dropping stdin closes the child's input so it can finish.
    });
    let _ = tokio::join!(
        tokio::io::copy(&mut stdout, &mut to_client),
        tokio::io::copy(&mut stderr, &mut stderr_to_client),
    );
    let status = child.wait().await;
    let code = status
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1)
        .unsigned_abs();
    let _ = handle.exit_status_request(id, code).await;
    let _ = to_client.shutdown().await;
    let _ = handle.close(id).await;
    uplink.abort();
}
