//! Throwaway repositories for tests, built with the git CLI.
//!
//! This is the only crate in the workspace that runs `git`; library code never does. Every
//! invocation is hermetic: no global or system config, fixed identity, fixed dates, no signing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tempfile::TempDir;

mod http;
mod ssh;

pub use http::HttpGitServer;
pub use ssh::SshGitServer;

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
    ///
    /// The repository is built once per test binary and each call returns a fresh copy of it
    /// (see [`clone_of`](Self::clone_of)). The template lives in a static, which never drops,
    /// so each test binary leaves one small template directory in the system temp dir.
    #[must_use]
    pub fn standard() -> Self {
        static TEMPLATE: OnceLock<Fixture> = OnceLock::new();
        Self::clone_of(TEMPLATE.get_or_init(Self::build_standard))
    }

    fn build_standard() -> Self {
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
        fixture.git_with_env(
            &["tag", "-a", "v1.0", "-m", "Release 1.0"],
            &[("GIT_COMMITTER_DATE", "2024-01-03T00:00:00+0000")],
        );
        fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fixture
    }

    /// A fresh copy of `template`, `.git` included, in a new temporary directory. Symlinks
    /// are recreated as symlinks and file permission bits are kept, then the index's stat
    /// data is refreshed so the copy reads as clean wherever the template did.
    ///
    /// Build a template once per test binary in a `static OnceLock<Fixture>` and hand each
    /// test a copy, instead of re-running the git commands that built it.
    ///
    /// # Panics
    /// On I/O failure, or when `git update-index --refresh` fails.
    #[must_use]
    pub fn clone_of(template: &Self) -> Self {
        let copy = Self::empty_dir();
        copy_tree(template.path(), copy.path());
        copy.git(&["update-index", "-q", "--refresh"]);
        copy
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

    /// Runs `git` in the fixture with a hermetic environment and returns stdout with trailing
    /// whitespace trimmed.
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

    /// Runs `git` expecting it to fail, returning its stderr followed by its stdout for
    /// assertions. Both, because some failures (`commit` with nothing staged, a conflicting
    /// `merge`) report only on stdout.
    ///
    /// # Panics
    /// When git cannot be started or exits zero.
    #[must_use]
    pub fn git_expect_failure(&self, args: &[&str]) -> String {
        let output = self.hermetic_git(args).output().expect("run git");
        assert!(
            !output.status.success(),
            "git {} unexpectedly succeeded",
            args.join(" ")
        );
        let mut report = String::from_utf8_lossy(&output.stderr).into_owned();
        report.push_str(&String::from_utf8_lossy(&output.stdout));
        report
    }

    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> String {
        let output = self
            .hermetic_git(args)
            .envs(extra_env.iter().copied())
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed in {} ({}):\nstderr: {}\nstdout: {}",
            args.join(" "),
            self.path().display(),
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    }

    /// A `git` command in the fixture with only the variables listed here: no global or system
    /// config, `HOME` inside the fixture, a fixed identity, signing off, and no credential or
    /// host-key prompt, so a test fails instead of waiting on the terminal.
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures are built with the git CLI"
    )]
    fn hermetic_git(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .current_dir(self.dir.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "Test Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com");
        command
    }

    /// `git rev-parse <spec>`.
    #[must_use]
    pub fn rev_parse(&self, spec: &str) -> String {
        self.git_output(&["rev-parse", spec])
    }

    /// Bare-clones this repository into a sibling temp dir, with pushes enabled
    /// (`http.receivepack=true`), for a network test to serve. Returns the holder, which
    /// deletes the copy on drop, and the path of the bare `.git` directory.
    ///
    /// The copy is never placed inside this working tree, where the next
    /// [`commit`](Self::commit) would sweep it in.
    ///
    /// # Panics
    /// When any git command fails.
    #[must_use]
    pub fn bare_copy(&self) -> (Self, PathBuf) {
        let bare = Self::empty_dir();
        let path = bare.path().join("remote.git");
        self.git(&["clone", "--bare", "--quiet", ".", &path.to_string_lossy()]);
        bare.git(&[
            "--git-dir",
            "remote.git",
            "config",
            "http.receivepack",
            "true",
        ]);
        (bare, path)
    }

    /// Pushes this repository's `main` into the bare repository at `bare_git_dir`.
    ///
    /// # Panics
    /// When the push fails.
    pub fn push_main_to(&self, bare_git_dir: &Path) {
        self.git(&["push", "--quiet", &bare_git_dir.to_string_lossy(), "main"]);
    }

    /// Clones the bare repository at `bare_git_dir` from its local path, re-points `origin`
    /// at `server`, and writes the server's client key into a fresh ssh directory. The git
    /// CLI never speaks to the test server. Returns the clone and the ssh directory.
    ///
    /// # Panics
    /// When any git command or key write fails.
    #[must_use]
    pub fn ssh_clone(bare_git_dir: &Path, server: &SshGitServer) -> (Self, Self) {
        let clone = Self::empty_dir();
        clone.git(&["clone", "--quiet", &bare_git_dir.to_string_lossy(), "."]);
        clone.git(&["remote", "set-url", "origin", &server.url()]);
        let ssh_home = Self::empty_dir();
        server.write_client_key(ssh_home.path());
        (clone, ssh_home)
    }

    /// Clones from an HTTP Basic-auth `server` with `user:pass` in the URL, then re-points
    /// `origin` at the bare URL so the code under test must supply credentials itself.
    ///
    /// # Panics
    /// When any git command fails.
    #[must_use]
    pub fn basic_auth_clone(server: &HttpGitServer, user: &str, pass: &str) -> Self {
        let clone = Self::empty_dir();
        let with_creds = server
            .url()
            .replace("http://", &format!("http://{user}:{pass}@"));
        clone.git(&[
            "-c",
            "credential.helper=",
            "clone",
            "--quiet",
            &with_creds,
            ".",
        ]);
        clone.git(&["remote", "set-url", "origin", &server.url()]);
        clone
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

    /// Removes a file, symlink, or directory tree below the working directory. An empty
    /// `relative` removes the whole working directory.
    ///
    /// # Panics
    /// On I/O failure, including when nothing exists at `relative`.
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures remove their own files"
    )]
    pub fn remove(&self, relative: &str) {
        let path = self.dir.path().join(relative);
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("stat {} before removing: {e}", path.display()));
        if meta.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
    }

    /// Sets the Unix permission bits of a file below the working directory, e.g. `0o755`.
    ///
    /// # Panics
    /// On I/O failure.
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixtures set their own file modes"
    )]
    pub fn set_mode(&self, relative: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.dir.path().join(relative);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
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

    /// A commit on top of `HEAD` whose tree is `HEAD`'s plus `contents` at `path` with `mode`
    /// (`100644`, `100755`, `120000`). Built with `git mktree`, which skips the index's path
    /// checks, so it can store what a hostile remote could send: `.git/hooks/pre-commit`,
    /// `.GIT/config`. The first segment of `path` must not already be in `HEAD`'s tree. No ref
    /// moves; returns the commit id.
    ///
    /// # Panics
    /// When `path` is empty or any git command fails.
    #[must_use]
    pub fn commit_raw_path(&self, path: &str, contents: &[u8], mode: &str) -> String {
        let blob = self.run_with_stdin(&["hash-object", "-w", "--stdin"], contents);
        let mut segments: Vec<&str> = path.split('/').collect();
        let leaf = segments.pop().expect("a path has a last segment");
        let mut entry = format!("{mode} blob {blob}\t{leaf}\n");
        for dir in segments.iter().rev() {
            let tree = self.run_with_stdin(&["mktree"], entry.as_bytes());
            entry = format!("040000 tree {tree}\t{dir}\n");
        }
        let mut root = self.git_output(&["ls-tree", "HEAD"]);
        root.push('\n');
        root.push_str(&entry);
        let tree = self.run_with_stdin(&["mktree"], root.as_bytes());
        let date = "2024-02-01T00:00:00+0000";
        let output = self
            .hermetic_git(&["commit-tree", &tree, "-p", "HEAD", "-m", "raw path"])
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .expect("run git commit-tree");
        assert!(output.status.success(), "git commit-tree failed");
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    }

    fn run_with_stdin(&self, args: &[&str], input: &[u8]) -> String {
        use std::io::Write;
        let mut child = self
            .hermetic_git(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("start git");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(input)
            .expect("write git stdin");
        let output = child.wait_with_output().expect("wait for git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    }
}

/// Copies the contents of `from` into the existing directory `to`, recreating symlinks
/// rather than following them.
#[expect(
    clippy::disallowed_methods,
    reason = "test fixtures copy their own template trees"
)]
fn copy_tree(from: &Path, to: &Path) {
    let entries =
        std::fs::read_dir(from).unwrap_or_else(|e| panic!("list {}: {e}", from.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("list {}: {e}", from.display()));
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("stat {}: {e}", source.display()));
        if kind.is_symlink() {
            let link = std::fs::read_link(&source)
                .unwrap_or_else(|e| panic!("read link {}: {e}", source.display()));
            std::os::unix::fs::symlink(&link, &target)
                .unwrap_or_else(|e| panic!("create symlink {}: {e}", target.display()));
        } else if kind.is_dir() {
            std::fs::create_dir(&target)
                .unwrap_or_else(|e| panic!("create dir {}: {e}", target.display()));
            copy_tree(&source, &target);
        } else {
            std::fs::copy(&source, &target).unwrap_or_else(|e| {
                panic!("copy {} to {}: {e}", source.display(), target.display())
            });
        }
    }
}
