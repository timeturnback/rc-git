//! Throwaway repositories for tests, built with the git CLI.
//!
//! This is the only crate in the workspace that runs `git`; library code never does. Every
//! invocation is hermetic: no global or system config, fixed identity, fixed dates, no signing.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

mod http;

pub use http::HttpGitServer;

/// A temporary directory holding a repository. Deleted on drop.
#[derive(Debug)]
pub struct Fixture {
    dir: TempDir,
}

/// Title of the first commit on `main`.
pub const COMMIT_1_TITLE: &str = "Initial import";
/// Title of the commit on `feature`.
pub const FEATURE_TITLE: &str = "Feature work";
/// Title of the second commit on `main`.
pub const COMMIT_2_TITLE: &str = "Add docs and library";
/// Bytes of `README.md`.
pub const README: &[u8] = b"# fixture\n\nhello\n";
/// Bytes of `src/main.rs`.
pub const MAIN_RS: &[u8] = b"fn main() {\n    println!(\"hi\");\n}\n";
/// Bytes of `bin.dat`; carries a NUL so git treats it as binary.
pub const BIN_DAT: &[u8] = b"\x89PNG\0\0binary";

impl Fixture {
    /// An empty directory that is not a repository.
    ///
    /// # Panics
    /// When the temporary directory cannot be created.
    #[must_use]
    pub fn empty_dir() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        Self { dir }
    }

    /// The standard repository every suite shares. History, oldest first:
    ///
    /// 1. `main`: [`COMMIT_1_TITLE`] with `README.md`, `src/main.rs`, `bin.dat`.
    ///    Lightweight tag `v0.9` points here.
    /// 2. `feature` (from 1): [`FEATURE_TITLE`] adds `feature.txt`.
    /// 3. `main`: [`COMMIT_2_TITLE`] adds `docs/guide.md`, `src/lib.rs`, `alpha.txt`,
    ///    `Zeta.txt`, and the symlink `link -> README.md`. Annotated tag `v1.0` points here,
    ///    and so does the remote-tracking ref `refs/remotes/origin/main`.
    ///
    /// # Panics
    /// When any git command fails.
    #[must_use]
    pub fn standard() -> Self {
        let fixture = Self::empty_dir();
        fixture.git(&["init", "-q", "-b", "main"]);

        fixture.write("README.md", README);
        fixture.write("src/main.rs", MAIN_RS);
        fixture.write("bin.dat", BIN_DAT);
        fixture.commit(COMMIT_1_TITLE, "2024-01-01T00:00:00+0000");
        fixture.git(&["tag", "v0.9"]);

        fixture.git(&["checkout", "-q", "-b", "feature"]);
        fixture.write("feature.txt", b"feature\n");
        fixture.commit(FEATURE_TITLE, "2024-01-02T00:00:00+0000");

        fixture.git(&["checkout", "-q", "main"]);
        fixture.write("docs/guide.md", b"guide\n");
        fixture.write("src/lib.rs", b"pub fn one() -> u32 {\n    1\n}\n");
        fixture.write("alpha.txt", b"alpha\n");
        fixture.write("Zeta.txt", b"zeta\n");
        fixture.symlink("README.md", "link");
        fixture.commit(COMMIT_2_TITLE, "2024-01-03T00:00:00+0000");
        fixture.git(&["tag", "-a", "v1.0", "-m", "Release 1.0"]);
        fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fixture
    }

    /// Path of the working directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Path of the `.git` directory.
    #[must_use]
    pub fn git_dir(&self) -> PathBuf {
        self.dir.path().join(".git")
    }

    /// Runs `git` in the fixture with a hermetic environment, discarding its output.
    ///
    /// # Panics
    /// When git cannot be started or exits non-zero.
    pub fn git(&self, args: &[&str]) {
        self.run(args, &[]);
    }

    /// Runs `git` in the fixture with a hermetic environment and returns trimmed stdout.
    ///
    /// # Panics
    /// When git cannot be started or exits non-zero.
    #[must_use]
    pub fn git_output(&self, args: &[&str]) -> String {
        self.run(args, &[])
    }

    /// Like [`git`](Self::git) with extra environment variables, for commands that need a
    /// fixed date, such as `merge`.
    ///
    /// # Panics
    /// When git cannot be started or exits non-zero.
    pub fn git_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) {
        self.run(args, extra_env);
    }

    /// Runs `git` expecting it to fail, returning its stderr for assertions.
    ///
    /// # Panics
    /// When git cannot be started or exits zero.
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures are built with the git CLI"
    )]
    pub fn git_expect_failure(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.dir.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run git");
        assert!(
            !output.status.success(),
            "git {} unexpectedly succeeded",
            args.join(" ")
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures are built with the git CLI"
    )]
    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> String {
        let output = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .current_dir(self.dir.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "Test Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .envs(extra_env.iter().copied())
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// `git rev-parse <spec>`.
    #[must_use]
    pub fn rev_parse(&self, spec: &str) -> String {
        self.git_output(&["rev-parse", spec])
    }

    /// Serves this repository over smart HTTP, with pushes enabled
    /// (`http.receivepack=true`).
    ///
    /// # Panics
    /// When the server cannot start.
    #[must_use]
    pub fn serve_http(&self) -> HttpGitServer {
        self.git(&["config", "http.receivepack", "true"]);
        HttpGitServer::serve(&self.git_dir())
    }

    /// Like [`serve_http`](Self::serve_http), demanding HTTP Basic credentials.
    ///
    /// # Panics
    /// When the server cannot start.
    #[must_use]
    pub fn serve_http_with_auth(&self, user: &str, pass: &str) -> HttpGitServer {
        self.git(&["config", "http.receivepack", "true"]);
        HttpGitServer::serve_with_auth(&self.git_dir(), user, pass)
    }

    /// Writes a file below the working directory, creating parent directories.
    ///
    /// # Panics
    /// On I/O failure.
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures write their own files"
    )]
    pub fn write(&self, relative: &str, contents: &[u8]) {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write fixture file");
    }

    /// Creates a symlink `relative -> target` in the working directory.
    ///
    /// # Panics
    /// On I/O failure.
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures write their own files"
    )]
    pub fn symlink(&self, target: &str, relative: &str) {
        std::os::unix::fs::symlink(target, self.dir.path().join(relative)).expect("create symlink");
    }

    /// Stages everything and commits with `date` as both author and committer date.
    pub fn commit(&self, title: &str, date: &str) {
        self.run(&["add", "-A"], &[]);
        self.run(
            &["commit", "-q", "-m", title, "--date", date],
            &[("GIT_COMMITTER_DATE", date)],
        );
    }

    /// Like [`commit`](Self::commit), with `body` as a paragraph after the title. Commits even
    /// when nothing is staged.
    pub fn commit_with_body(&self, title: &str, body: &str, date: &str) {
        self.run(&["add", "-A"], &[]);
        self.run(
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                title,
                "-m",
                body,
                "--date",
                date,
            ],
            &[("GIT_COMMITTER_DATE", date)],
        );
    }
}
