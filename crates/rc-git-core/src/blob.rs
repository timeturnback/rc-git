//! File contents.

use crate::error::{Error, ObjectKind, Result, object};
use crate::oid::Oid;
use crate::repo::Repository;

/// Git's own rule: a file is binary when a NUL byte appears in its first 8000 bytes.
const GIT_BINARY_PROBE_BYTES: usize = 8000;

/// Default size cap for [`Repository::read_blob`]: 8 MiB.
pub const DEFAULT_BLOB_LIMIT: u64 = 8 * 1024 * 1024;

/// What a blob read produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobContent {
    /// Text, as stored. `line_starts` comes from `line_starts`.
    Text {
        /// The bytes as stored.
        data: Vec<u8>,
        /// Byte offset where each line begins.
        line_starts: Vec<usize>,
        /// Whether `data` is valid UTF-8, checked here once so no display layer has to
        /// re-scan the bytes to learn whether decoding was lossy.
        valid_utf8: bool,
    },
    /// Contains a NUL byte near the start; not decoded for display.
    Binary {
        /// Size in bytes.
        size: u64,
    },
    /// Larger than the caller's limit; nothing was decoded.
    TooLarge {
        /// Size in bytes.
        size: u64,
        /// The limit that was exceeded.
        limit: u64,
    },
}

impl Repository {
    /// Size of a blob in bytes, read from the object header without decoding the content.
    ///
    /// # Errors
    /// [`Error::Object`] when missing, [`Error::WrongKind`] when the id is not a blob.
    pub fn blob_size(&self, oid: Oid) -> Result<u64> {
        self.with(|repo| {
            let header = repo
                .find_header(*oid.as_gix())
                .map_err(object("read blob header", oid))?;
            expect_blob("read blob size", oid, header.kind())?;
            Ok(header.size())
        })
    }

    /// Reads a blob for display. Checks the size against `limit` before decoding, so a huge
    /// blob costs one header read and nothing more.
    ///
    /// # Errors
    /// [`Error::Object`] when missing, [`Error::WrongKind`] when the id is not a blob.
    pub fn read_blob(&self, oid: Oid, limit: u64) -> Result<BlobContent> {
        self.with(|repo| {
            let header = repo
                .find_header(*oid.as_gix())
                .map_err(object("read blob header", oid))?;
            expect_blob("read blob", oid, header.kind())?;
            let size = header.size();
            if size > limit {
                return Ok(BlobContent::TooLarge { size, limit });
            }
            let mut blob = repo
                .find_blob(*oid.as_gix())
                .map_err(object("read blob", oid))?;
            Ok(classify(blob.take_data()))
        })
    }
}

fn expect_blob(op: &'static str, oid: Oid, kind: gix::objs::Kind) -> Result<()> {
    if kind == gix::objs::Kind::Blob {
        return Ok(());
    }
    Err(Error::WrongKind {
        op,
        oid,
        expected: ObjectKind::Blob,
        actual: ObjectKind::from_gix(kind),
    })
}

/// Sorts bytes already read into text or binary. Callers check the size limit before reading.
pub(crate) fn classify(data: Vec<u8>) -> BlobContent {
    if is_binary(&data) {
        return BlobContent::Binary {
            size: u64::try_from(data.len()).unwrap_or(u64::MAX),
        };
    }
    let line_starts = line_starts(&data);
    let valid_utf8 = std::str::from_utf8(&data).is_ok();
    BlobContent::Text {
        data,
        line_starts,
        valid_utf8,
    }
}

/// Applies git's binary heuristic: a NUL byte within the first
/// [`GIT_BINARY_PROBE_BYTES`] bytes.
#[must_use]
pub(crate) fn is_binary(data: &[u8]) -> bool {
    let probe = &data[..data.len().min(GIT_BINARY_PROBE_BYTES)];
    memchr::memchr(0, probe).is_some()
}

/// Byte offsets at which lines start. Empty input has no lines; otherwise the first line starts
/// at 0 and each `\n` that is not the last byte starts another, so `"a\nb\n"` and `"a\nb"` both
/// have two lines. A `\r` before the `\n` stays inside its line for the caller to trim.
#[must_use]
pub(crate) fn line_starts(data: &[u8]) -> Vec<usize> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut starts = Vec::with_capacity(data.len() / 32 + 1);
    starts.push(0);
    for newline in memchr::memchr_iter(b'\n', data) {
        let next = newline + 1;
        if next < data.len() {
            starts.push(next);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_needs_a_nul_in_the_probe_window() {
        assert!(!is_binary(b""));
        assert!(!is_binary(b"plain text\n"));
        assert!(is_binary(b"\0"));
        assert!(is_binary(b"abc\0def"));
        let mut late_nul = vec![b'x'; GIT_BINARY_PROBE_BYTES];
        late_nul.push(0);
        assert!(
            !is_binary(&late_nul),
            "a NUL after the window does not count, as in git"
        );
        let mut edge_nul = vec![b'x'; GIT_BINARY_PROBE_BYTES - 1];
        edge_nul.push(0);
        assert!(is_binary(&edge_nul));
    }

    #[test]
    fn line_starts_follow_the_documented_rule() {
        assert_eq!(line_starts(b""), Vec::<usize>::new());
        assert_eq!(line_starts(b"\n"), [0]);
        assert_eq!(line_starts(b"a"), [0]);
        assert_eq!(line_starts(b"a\n"), [0]);
        assert_eq!(line_starts(b"a\nb"), [0, 2]);
        assert_eq!(line_starts(b"a\nb\n"), [0, 2]);
        assert_eq!(line_starts(b"\n\n"), [0, 1]);
        assert_eq!(line_starts(b"a\r\nb\r\n"), [0, 3]);
    }

    #[test]
    fn classify_splits_text_and_binary() {
        assert_eq!(
            classify(b"x\ny".to_vec()),
            BlobContent::Text {
                data: b"x\ny".to_vec(),
                line_starts: vec![0, 2],
                valid_utf8: true,
            }
        );
        assert_eq!(classify(vec![1, 0, 2]), BlobContent::Binary { size: 3 });
    }

    #[test]
    fn classify_flags_invalid_utf8_that_is_still_text() {
        // Latin-1 "café\n": no NUL, so it is text, but 0xE9 is not valid UTF-8.
        let latin1 = b"caf\xE9\n".to_vec();
        assert_eq!(
            classify(latin1.clone()),
            BlobContent::Text {
                data: latin1,
                line_starts: vec![0],
                valid_utf8: false,
            }
        );
    }
}
