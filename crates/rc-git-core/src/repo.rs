//! Opening a repository and sharing it across threads.

use std::ffi::OsString;
use std::num::NonZero;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{Error, ObjectKind, Result, object};
use crate::oid::Oid;

/// Per-handle object cache. Trees and commits decode into it; blob reads bypass it.
const OBJECT_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Upper bound on pooled handles. Each holds its own object cache.
const MAX_POOL_SLOTS: usize = 4;

/// An open repository. Every method takes `&self` and may run from any thread.
///
/// `gix` handles are single-threaded, so this keeps a small pool of them. A call borrows a free
/// handle, or builds a temporary one when all are busy, so no call ever waits on another.
pub struct Repository {
    shared: gix::ThreadSafeRepository,
    pool: Box<[Mutex<gix::Repository>]>,
    git_dir: PathBuf,
    work_dir: Option<PathBuf>,
}

/// The isolated open, plus the user's global ignore file as an API override of
/// `core.excludesFile`: it outranks the repository's own config, so a repository cannot point
/// the ignore rules elsewhere, and gix ranks it below `.git/info/exclude` and `.gitignore`.
fn open_options() -> gix::open::Options {
    let options = gix::open::Options::isolated();
    match global_excludes_file() {
        Some(file) => {
            let mut value = b"core.excludesFile=".to_vec();
            value.extend_from_slice(file.as_os_str().as_bytes());
            options.config_overrides([value])
        }
        None => options,
    }
}

/// Where git looks for the global ignore file when no config names one:
/// `$XDG_CONFIG_HOME/git/ignore` when that variable is set, else `$HOME/.config/git/ignore`.
/// Only an absolute path is used, so gix never has to expand `~` or `%(prefix)` in it.
pub(crate) fn global_excludes_file() -> Option<PathBuf> {
    let non_empty = |value: OsString| (!value.is_empty()).then(|| PathBuf::from(value));
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .and_then(non_empty)
        .or_else(|| {
            std::env::var_os("HOME")
                .and_then(non_empty)
                .map(|home| home.join(".config"))
        })?;
    let file = config_home.join("git").join("ignore");
    file.is_absolute().then_some(file)
}

/// Refuses a repository whose refs are not stored as loose files plus `packed-refs`.
///
/// `ref_storage` is the repository's `extensions.refStorage` value, `None` when unset. gix
/// reads only the files backend, so a reftable repository opens but every ref read then
/// fails on its compatibility stubs. Call this right after opening, with the value read
/// under the isolated open, so the refusal names the format instead of a broken ref.
///
/// # Errors
/// [`Error::Open`] for `path` when `ref_storage` is set to anything other than `files`.
pub fn require_files_ref_storage(path: &Path, ref_storage: Option<&[u8]>) -> Result<()> {
    match ref_storage {
        None | Some(b"files") => Ok(()),
        Some(format) => Err(Error::Open {
            path: path.to_path_buf(),
            source: format!(
                "{} repositories are not supported: extensions.refStorage must be files",
                gix::bstr::BStr::new(format)
            )
            .into(),
        }),
    }
}

/// Refuses a repository whose working tree is not the directory that was opened.
///
/// `opened` is the path handed to the open; `git_dir` and `work_dir` are what gix resolved from
/// it, and `configured` is the repository's own `core.worktree` value, `None` when unset. The
/// working tree must be `opened` itself, or its parent when `opened` is the git directory, so a
/// repository's config cannot aim reads and guard writes at a directory outside it. A submodule
/// whose `core.worktree` leads back to its own checkout still opens. Call this right after
/// opening, with the values read under the isolated open.
///
/// # Errors
/// [`Error::Open`] for `opened` when `work_dir` resolves anywhere else, naming `core.worktree`
/// and its value.
pub fn require_work_tree_at(
    opened: &Path,
    git_dir: &Path,
    work_dir: Option<&Path>,
    configured: Option<&[u8]>,
) -> Result<()> {
    let Some(work_dir) = work_dir else {
        return Ok(());
    };
    // A path that cannot be resolved compares unequal, so the failure refuses the open.
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let opened_real = real(opened);
    let expected = if opened_real.is_some() && opened_real == real(git_dir) {
        opened_real
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    } else {
        opened_real
    };
    let actual = real(work_dir);
    if expected.is_some() && actual == expected {
        return Ok(());
    }
    let setting = configured.map_or_else(
        || "the resolved working tree".to_owned(),
        |value| format!("core.worktree = {}", gix::bstr::BStr::new(value)),
    );
    Err(Error::Open {
        path: opened.to_path_buf(),
        source: format!(
            "{setting} puts the working tree at {}, outside the opened repository; \
             unset core.worktree to open it",
            actual.as_deref().unwrap_or(work_dir).display()
        )
        .into(),
    })
}

impl Repository {
    /// Opens the repository at `path`, either a working directory or a `.git` directory.
    ///
    /// Only the repository's own `config` is read. Global, system, and environment configuration
    /// are ignored and `include` directives are not followed, so opening an untrusted repository
    /// cannot pull in configuration from anywhere else. The one exception is the user's global
    /// ignore file at git's default location, `$XDG_CONFIG_HOME/git/ignore` or else
    /// `$HOME/.config/git/ignore`, read with git's lowest ignore precedence. The repository's
    /// own config cannot move the working tree elsewhere either (see [`require_work_tree_at`]);
    /// only `objects/info/alternates` may lend objects from another repository. See
    /// `docs/safety.md`.
    ///
    /// # Errors
    /// [`Error::Open`] when `path` is not a git repository, cannot be read, stores its refs
    /// in a format other than files (see [`require_files_ref_storage`]), or sets a
    /// `core.worktree` outside itself (see [`require_work_tree_at`]).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let shared =
            gix::ThreadSafeRepository::open_opts(path, open_options()).map_err(|source| {
                Error::Open {
                    path: path.to_path_buf(),
                    source: Box::new(source),
                }
            })?;
        let probe = shared.to_thread_local();
        let config = probe.config_snapshot();
        require_files_ref_storage(
            path,
            config
                .string("extensions.refStorage")
                .as_deref()
                .map(Vec::as_slice),
        )?;
        let git_dir = probe.git_dir().to_path_buf();
        let work_dir = probe.workdir().map(Path::to_path_buf);
        require_work_tree_at(
            path,
            &git_dir,
            work_dir.as_deref(),
            config.string("core.worktree").as_deref().map(Vec::as_slice),
        )?;
        let slots = std::thread::available_parallelism()
            .map_or(1, NonZero::get)
            .min(MAX_POOL_SLOTS);
        let mut pool = Vec::with_capacity(slots);
        for _ in 0..slots {
            pool.push(Mutex::new(Self::handle(&shared)));
        }
        Ok(Self {
            shared,
            pool: pool.into_boxed_slice(),
            git_dir,
            work_dir,
        })
    }

    /// The `.git` directory.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The working directory, or `None` for a bare repository.
    #[must_use]
    pub fn work_dir(&self) -> Option<&Path> {
        self.work_dir.as_deref()
    }

    /// Resolves a revision expression to the object it names: a branch, a tag, a remote branch,
    /// `HEAD`, an abbreviated hash, or any other form `git rev-parse` accepts.
    ///
    /// # Errors
    /// [`Error::RevSpec`] when nothing matches or the expression is ambiguous.
    pub fn resolve(&self, spec: &str) -> Result<Oid> {
        self.with(|repo| {
            repo.rev_parse_single(spec)
                .map(|id| Oid::from_gix(id.detach()))
                .map_err(|source| Error::RevSpec {
                    spec: spec.to_owned(),
                    source: Box::new(source),
                })
        })
    }

    /// Resolves `spec` and peels tags and commits down to the tree they contain.
    ///
    /// # Errors
    /// As [`Repository::resolve`] and [`Repository::tree_of`].
    pub fn resolve_tree(&self, spec: &str) -> Result<Oid> {
        let id = self.resolve(spec)?;
        self.tree_of(id)
    }

    /// Peels any object to a tree: a tag to its target, a commit to its tree, a tree to itself.
    /// Reads object headers first, so a blob is rejected without inflating it.
    ///
    /// # Errors
    /// [`Error::WrongKind`] for a blob or a tag that ends at one, [`Error::Object`] when an
    /// object on the way is missing or unreadable.
    pub fn tree_of(&self, id: Oid) -> Result<Oid> {
        self.with(|repo| {
            let (id, kind) = peel_tags(repo, id)?;
            match kind {
                gix::objs::Kind::Tree => Ok(id),
                gix::objs::Kind::Commit => {
                    let commit = repo
                        .find_commit(*id.as_gix())
                        .map_err(object("read commit", id))?;
                    let tree = commit.tree_id().map_err(object("decode commit", id))?;
                    Ok(Oid::from_gix(tree.detach()))
                }
                actual => Err(Error::WrongKind {
                    op: "peel to tree",
                    oid: id,
                    expected: ObjectKind::Tree,
                    actual: ObjectKind::from_gix(actual),
                }),
            }
        })
    }

    fn handle(shared: &gix::ThreadSafeRepository) -> gix::Repository {
        let mut repo = shared.to_thread_local();
        repo.object_cache_size_if_unset(OBJECT_CACHE_BYTES);
        repo
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&gix::Repository) -> T) -> T {
        for slot in &self.pool {
            if let Ok(repo) = slot.try_lock() {
                return f(&repo);
            }
        }
        f(&Self::handle(&self.shared))
    }
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("git_dir", &self.git_dir)
            .field("work_dir", &self.work_dir)
            .field("pool_slots", &self.pool.len())
            .finish_non_exhaustive()
    }
}

/// Follows annotated tags from `id` to the first object that is not a tag, reading headers so
/// only tags are decoded. Returns that object and its kind. Tags cannot form cycles: a tag's
/// id hashes its target's id, so the loop ends.
pub(crate) fn peel_tags(repo: &gix::Repository, mut id: Oid) -> Result<(Oid, gix::objs::Kind)> {
    loop {
        let header = repo
            .find_header(*id.as_gix())
            .map_err(object("read object header", id))?;
        if header.kind() != gix::objs::Kind::Tag {
            return Ok((id, header.kind()));
        }
        let tag = repo
            .find_tag(*id.as_gix())
            .map_err(object("read tag", id))?;
        let target = tag.target_id().map_err(object("decode tag", id))?;
        id = Oid::from_gix(target.detach());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Repository>();
    }
}
