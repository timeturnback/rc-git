//! Opening a repository and sharing it across threads.

use std::num::NonZero;
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

impl Repository {
    /// Opens the repository at `path`, either a working directory or a `.git` directory.
    ///
    /// Only the repository's own `config` is read. Global, system, and environment configuration
    /// are ignored and `include` directives are not followed, so opening an untrusted repository
    /// cannot pull in configuration from anywhere else. See `docs/safety.md`.
    ///
    /// # Errors
    /// [`Error::Open`] when `path` is not a git repository or cannot be read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let shared = gix::ThreadSafeRepository::open_opts(path, gix::open::Options::isolated())
            .map_err(|source| Error::Open {
                path: path.to_path_buf(),
                source: Box::new(source),
            })?;
        let probe = shared.to_thread_local();
        let git_dir = probe.git_dir().to_path_buf();
        let work_dir = probe.workdir().map(Path::to_path_buf);
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

    /// Peels any object to a tree: a tag to its commit, a commit to its tree, a tree to itself.
    ///
    /// # Errors
    /// [`Error::WrongKind`] for a blob, [`Error::Object`] when the object is missing.
    pub fn tree_of(&self, id: Oid) -> Result<Oid> {
        self.with(|repo| {
            let obj = repo
                .find_object(*id.as_gix())
                .map_err(object("read object", id))?;
            if obj.kind == gix::objs::Kind::Blob {
                return Err(Error::WrongKind {
                    oid: id,
                    expected: ObjectKind::Tree,
                    actual: ObjectKind::Blob,
                });
            }
            let tree = obj.peel_to_tree().map_err(object("peel to tree", id))?;
            Ok(Oid::from_gix(tree.id))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Repository>();
    }
}
