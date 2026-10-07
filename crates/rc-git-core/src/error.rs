//! The single error type for this crate.

use std::fmt;
use std::path::PathBuf;

use crate::oid::Oid;

/// Source error from `gix`, boxed so callers never depend on gix types.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The four kinds of git object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// File contents.
    Blob,
    /// A directory listing.
    Tree,
    /// A commit.
    Commit,
    /// An annotated tag.
    Tag,
}

impl ObjectKind {
    pub(crate) fn from_gix(kind: gix::objs::Kind) -> Self {
        match kind {
            gix::objs::Kind::Blob => Self::Blob,
            gix::objs::Kind::Tree => Self::Tree,
            gix::objs::Kind::Commit => Self::Commit,
            gix::objs::Kind::Tag => Self::Tag,
        }
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        };
        f.write_str(name)
    }
}

/// Every failure this crate reports. Each variant names the operation that failed and carries
/// the cause, so the message is actionable on its own.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The path is not a git repository or cannot be read.
    #[error("cannot open git repository at {}: {}", path.display(), Causes(.source))]
    Open {
        /// The path that was opened.
        path: PathBuf,
        /// The underlying cause.
        #[source]
        source: BoxError,
    },
    /// An object lookup or decode failed.
    #[error("{op}: object {oid} is missing or unreadable: {}", Causes(.source))]
    Object {
        /// What was being done.
        op: &'static str,
        /// The object involved.
        oid: Oid,
        /// The underlying cause.
        #[source]
        source: BoxError,
    },
    /// Any other repository-level failure.
    #[error("{op}: {}", Causes(.source))]
    Git {
        /// What was being done.
        op: &'static str,
        /// The underlying cause.
        #[source]
        source: BoxError,
    },
    /// A revision expression did not resolve.
    #[error("'{spec}' is not a revision in this repository: {}", Causes(.source))]
    RevSpec {
        /// The expression as given.
        spec: String,
        /// The underlying cause.
        #[source]
        source: BoxError,
    },
    /// The object exists but is not the kind the operation needs.
    #[error("object {oid} is a {actual}, expected a {expected}")]
    WrongKind {
        /// The object involved.
        oid: Oid,
        /// What the operation needed.
        expected: ObjectKind,
        /// What the object is.
        actual: ObjectKind,
    },
    /// A string is not a hexadecimal object id.
    #[error("'{input}' is not a hexadecimal object id")]
    InvalidOid {
        /// The rejected input.
        input: String,
    },
    /// A working-tree read failed.
    #[error("{op}: '{path}': {}", Causes(.source))]
    Worktree {
        /// What was being done.
        op: &'static str,
        /// The repository-relative path involved.
        path: String,
        /// The underlying cause.
        #[source]
        source: BoxError,
    },
}

/// Renders a cause and every error below it once each, in alternate form.
///
/// gix errors append `, at <file>:<line>` from the build machine unless formatted with `{:#}`,
/// and many of them repeat their own source in their message, so a frame whose text is
/// already in the line is skipped.
struct Causes<'a>(&'a BoxError);

impl fmt::Display for Causes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut line = format!("{:#}", self.0);
        let mut frame = self.0.source();
        while let Some(err) = frame {
            let text = format!("{err:#}");
            if !text.is_empty() && !line.contains(&text) {
                line.push_str(": ");
                line.push_str(&text);
            }
            frame = err.source();
        }
        f.write_str(&line)
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn git<E>(op: &'static str) -> impl FnOnce(E) -> Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    move |source| Error::Git {
        op,
        source: Box::new(source),
    }
}

pub(crate) fn object<E>(op: &'static str, oid: Oid) -> impl FnOnce(E) -> Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    move |source| Error::Object {
        op,
        oid,
        source: Box::new(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_kind_displays_git_names() {
        assert_eq!(ObjectKind::Blob.to_string(), "blob");
        assert_eq!(ObjectKind::Tree.to_string(), "tree");
        assert_eq!(ObjectKind::Commit.to_string(), "commit");
        assert_eq!(ObjectKind::Tag.to_string(), "tag");
    }

    #[test]
    fn messages_name_the_operation_and_object() {
        let oid = Oid::from_hex("0123456789abcdef0123456789abcdef01234567").unwrap();
        let err = Error::WrongKind {
            oid,
            expected: ObjectKind::Tree,
            actual: ObjectKind::Blob,
        };
        assert_eq!(
            err.to_string(),
            "object 0123456789abcdef0123456789abcdef01234567 is a blob, expected a tree"
        );
        let err = Error::InvalidOid { input: "zz".into() };
        assert_eq!(err.to_string(), "'zz' is not a hexadecimal object id");
        let err = Error::Worktree {
            op: "read working tree file",
            path: "src/gone.rs".into(),
            source: "missing".into(),
        };
        assert_eq!(
            err.to_string(),
            "read working tree file: 'src/gone.rs': missing"
        );
    }

    #[derive(Debug)]
    struct Frame {
        text: &'static str,
        source: Option<Box<Frame>>,
    }

    impl fmt::Display for Frame {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.text)?;
            if !f.alternate() {
                f.write_str(", at /build/src/lib.rs:1")?;
            }
            Ok(())
        }
    }

    impl std::error::Error for Frame {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source
                .as_deref()
                .map(|frame| frame as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn causes_list_each_frame_once_without_source_locations() {
        let leaf = Frame {
            text: "ref 'nope' could not be found",
            source: None,
        };
        let repeat = Frame {
            text: "ref 'nope' could not be found",
            source: Some(Box::new(leaf)),
        };
        let top = Frame {
            text: "couldn't parse revision",
            source: Some(Box::new(repeat)),
        };
        let err = Error::RevSpec {
            spec: "nope".into(),
            source: Box::new(top),
        };
        assert_eq!(
            err.to_string(),
            "'nope' is not a revision in this repository: couldn't parse revision: \
             ref 'nope' could not be found"
        );
    }
}
