//! A private `ssh-agent` for tests that drive ssh authentication through an agent. It
//! listens on a socket in its own temporary directory, so a test names it with
//! ssh-config `IdentityAgent` and never reaches the developer's real agent.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// How long `ssh-agent` gets to create its socket before the fixture gives up.
const SOCKET_WAIT: Duration = Duration::from_secs(5);

/// A running `ssh-agent -D`, killed on drop.
pub struct SshAgent {
    child: Child,
    socket: PathBuf,
    _dir: TempDir,
}

impl SshAgent {
    /// Starts `ssh-agent -D -a <tempdir>/agent.sock` holding no keys and waits for the
    /// socket to appear.
    ///
    /// # Panics
    /// When `ssh-agent` cannot be started or its socket does not appear in time.
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test kit runs the OpenSSH agent the code under test talks to"
    )]
    pub fn start() -> Self {
        let dir = tempfile::tempdir().expect("create the agent's temp dir");
        let socket = dir.path().join("agent.sock");
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start ssh-agent");
        let mut agent = Self {
            child,
            socket,
            _dir: dir,
        };
        agent.wait_for_socket();
        agent
    }

    fn wait_for_socket(&mut self) {
        let deadline = Instant::now() + SOCKET_WAIT;
        while !self.socket.exists() {
            if let Some(status) = self.child.try_wait().expect("poll ssh-agent") {
                panic!("ssh-agent exited before listening: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "ssh-agent did not create {} within {SOCKET_WAIT:?}",
                self.socket.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The agent's socket, for ssh-config `IdentityAgent`.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Adds the unencrypted private key at `key` with `ssh-add`. The agent lists keys in
    /// the order they were added. `ssh-add` names a key with an empty comment after its
    /// file, so the agent's copy carries a comment the key file does not.
    ///
    /// # Panics
    /// When `ssh-add` cannot be started or exits non-zero.
    #[expect(
        clippy::disallowed_methods,
        reason = "the test kit loads fixture keys with the OpenSSH ssh-add"
    )]
    pub fn add(&self, key: &Path) {
        let output = Command::new("ssh-add")
            .arg(key)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("SSH_AUTH_SOCK", &self.socket)
            .stdin(Stdio::null())
            .output()
            .expect("run ssh-add");
        assert!(
            output.status.success(),
            "ssh-add {} failed: {}",
            key.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for SshAgent {
    fn drop(&mut self) {
        // A failed kill means the agent already exited; either way reap it so no zombie
        // outlives the test.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
