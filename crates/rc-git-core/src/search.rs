//! Content search: which lines of a set of files hold a literal string.
//!
//! Nothing here walks a tree. The caller names the blobs or working-tree files to read, a
//! batch at a time, and each call stops once it has read `max_bytes`, so no call holds a pool
//! handle through a whole repository and the caller can stop between batches.

use std::ops::Range;

use memchr::memmem::Finder;

use crate::blob::{BlobContent, read_blob_in};
use crate::error::Result;
use crate::oid::Oid;
use crate::repo::Repository;

/// Largest file a search reads: 1 MiB. A bigger one costs a header read or a `stat` and is
/// reported as [`FileSearch::TooLarge`].
pub const DEFAULT_SEARCH_FILE_LIMIT: u64 = 1024 * 1024;

/// Bytes one search call reads before it stops: 8 MiB. The file that crosses it is finished.
pub const DEFAULT_SEARCH_BATCH_BYTES: u64 = 8 * 1024 * 1024;

/// Matching lines kept per file; a file with more reports [`FileSearch::Searched`] `truncated`.
pub const MAX_LINES_PER_FILE: usize = 100;

/// Longest excerpt of a matching line, in bytes. A longer line is cut to a window that starts
/// a little before its first match.
pub const EXCERPT_BYTES: usize = 200;

/// Bytes of a long line kept before its first match in the excerpt.
const EXCERPT_LEAD_BYTES: usize = EXCERPT_BYTES / 4;

/// What to look for: a literal byte string, with smart case. A query with no ASCII uppercase
/// letter matches ASCII letters in either case; one with an uppercase letter matches exactly.
/// Letters outside ASCII always match exactly.
#[derive(Debug, Clone)]
pub struct ContentQuery {
    finder: Finder<'static>,
    ignore_case: bool,
}

impl ContentQuery {
    /// The query for `text`, or `None` when it is empty, which would match everywhere.
    #[must_use]
    pub fn new(text: &[u8]) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        let ignore_case = !text.iter().any(u8::is_ascii_uppercase);
        Some(Self {
            finder: Finder::new(text).into_owned(),
            ignore_case,
        })
    }

    fn needle_len(&self) -> usize {
        self.finder.needle().len()
    }
}

/// Bounds for one search call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    /// Largest file read, clamped to [`crate::MAX_READ_LIMIT`]; a bigger one is
    /// [`FileSearch::TooLarge`].
    pub file_limit: u64,
    /// Bytes read before the call stops; the file that crosses it is finished.
    pub max_bytes: u64,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            file_limit: DEFAULT_SEARCH_FILE_LIMIT,
            max_bytes: DEFAULT_SEARCH_BATCH_BYTES,
        }
    }
}

/// One line that holds the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    /// Zero-based line number, counted as [`BlobContent::Text`] counts lines.
    pub line: usize,
    /// Byte offset of the first match from the start of the line.
    pub column: usize,
    /// The line, or a window of it when it is longer than [`EXCERPT_BYTES`], without its
    /// leading indentation or line ending. Cut only between UTF-8 characters.
    pub excerpt: Vec<u8>,
    /// Byte ranges of the matches inside `excerpt`, in order and not overlapping. A match the
    /// window cut is clipped to it; one outside it is left out.
    pub ranges: Vec<Range<usize>>,
}

/// What searching one file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSearch {
    /// Text that was searched; `lines` is empty when nothing matched.
    Searched {
        /// Matching lines in file order, at most [`MAX_LINES_PER_FILE`].
        lines: Vec<LineMatch>,
        /// Whether more lines matched than `lines` holds.
        truncated: bool,
    },
    /// A NUL byte near the start; not searched.
    Binary,
    /// Larger than the limit; not read.
    TooLarge {
        /// Size in bytes.
        size: u64,
        /// The limit that was exceeded.
        limit: u64,
    },
    /// A working-tree file that could not be read, such as one deleted since it was listed.
    Unreadable {
        /// Which read failed and why.
        error: String,
    },
}

/// One search call's results: one entry per file it reached, in the order asked. Fewer
/// entries than files asked for means the call stopped at `max_bytes`; ask again for the rest.
pub type SearchBatch = Vec<FileSearch>;

impl Repository {
    /// Searches the blobs `oids`, in order, until `limits.max_bytes` have been read. Each
    /// blob's size comes from its header first, so one over `limits.file_limit` is never
    /// decoded. A blob listed twice is searched twice; dedupe before calling.
    ///
    /// # Errors
    /// [`crate::Error::Object`] when a blob is missing, [`crate::Error::WrongKind`] when an
    /// id is not a blob.
    pub fn search_blobs(
        &self,
        oids: &[Oid],
        query: &ContentQuery,
        limits: SearchLimits,
    ) -> Result<SearchBatch> {
        self.with(|repo| {
            let mut batch = Vec::new();
            let mut read = 0u64;
            let mut folded = Vec::new();
            for &oid in oids {
                if read >= limits.max_bytes {
                    break;
                }
                let content = read_blob_in(repo, oid, limits.file_limit)?;
                read = read.saturating_add(bytes_read(&content));
                batch.push(search_content(&content, query, &mut folded));
            }
            Ok(batch)
        })
    }

    /// Searches the working-tree files at the repository-relative `paths`, in order, until
    /// `limits.max_bytes` have been read. Each size comes from a `stat` first, as in
    /// [`Repository::worktree_file`]. A file that cannot be read is reported as
    /// [`FileSearch::Unreadable`] and does not stop the search.
    ///
    /// # Errors
    /// [`crate::Error::Worktree`] for a bare repository.
    pub fn search_worktree_files(
        &self,
        paths: &[impl AsRef<str>],
        query: &ContentQuery,
        limits: SearchLimits,
    ) -> Result<SearchBatch> {
        self.require_work_dir("search working tree files", "")?;
        let mut batch = Vec::new();
        let mut read = 0u64;
        let mut folded = Vec::new();
        for path in paths {
            if read >= limits.max_bytes {
                break;
            }
            match self.worktree_file(path.as_ref(), limits.file_limit) {
                Ok(content) => {
                    read = read.saturating_add(bytes_read(&content));
                    batch.push(search_content(&content, query, &mut folded));
                }
                Err(err) => batch.push(FileSearch::Unreadable {
                    error: err.to_string(),
                }),
            }
        }
        Ok(batch)
    }
}

/// Bytes a read pulled into memory: nothing for a file over the limit.
fn bytes_read(content: &BlobContent) -> u64 {
    match content {
        BlobContent::Text { data, .. } => u64::try_from(data.len()).unwrap_or(u64::MAX),
        BlobContent::Binary { size } => *size,
        BlobContent::TooLarge { .. } => 0,
    }
}

/// What `content` holds of the query. `folded` is scratch space, reused across files.
fn search_content(content: &BlobContent, query: &ContentQuery, folded: &mut Vec<u8>) -> FileSearch {
    match content {
        BlobContent::Text {
            data, line_starts, ..
        } => search_text(data, line_starts, query, folded),
        BlobContent::Binary { .. } => FileSearch::Binary,
        BlobContent::TooLarge { size, limit } => FileSearch::TooLarge {
            size: *size,
            limit: *limit,
        },
    }
}

/// Every line of `data` holding the query, up to [`MAX_LINES_PER_FILE`]. Folding ASCII case
/// into `folded` keeps every byte at its offset, so offsets into it are offsets into `data`.
fn search_text(
    data: &[u8],
    line_starts: &[usize],
    query: &ContentQuery,
    folded: &mut Vec<u8>,
) -> FileSearch {
    let haystack = if query.ignore_case {
        folded.clear();
        folded.extend_from_slice(data);
        folded.make_ascii_lowercase();
        folded.as_slice()
    } else {
        data
    };
    let mut lines = Vec::new();
    let mut current: Option<(usize, Vec<usize>)> = None;
    for start in query.finder.find_iter(haystack) {
        let line = line_starts
            .partition_point(|&s| s <= start)
            .saturating_sub(1);
        if let Some((open, starts)) = &mut current
            && *open == line
        {
            starts.push(start);
            continue;
        }
        if let Some((open, starts)) = current.replace((line, vec![start])) {
            lines.push(line_match(data, line_starts, open, &starts, query));
            if lines.len() == MAX_LINES_PER_FILE {
                return FileSearch::Searched {
                    lines,
                    truncated: true,
                };
            }
        }
    }
    if let Some((open, starts)) = current {
        lines.push(line_match(data, line_starts, open, &starts, query));
    }
    FileSearch::Searched {
        lines,
        truncated: false,
    }
}

/// The match record for `line`, whose matches begin at the file offsets `starts`.
fn line_match(
    data: &[u8],
    line_starts: &[usize],
    line: usize,
    starts: &[usize],
    query: &ContentQuery,
) -> LineMatch {
    let begin = line_starts.get(line).copied().unwrap_or(0);
    let end = line_starts.get(line + 1).copied().unwrap_or(data.len());
    let mut text = &data[begin..end];
    text = text.strip_suffix(b"\n").unwrap_or(text);
    text = text.strip_suffix(b"\r").unwrap_or(text);
    let mut ranges = Vec::with_capacity(starts.len());
    for &start in starts {
        let from = start - begin;
        ranges.push(from..(from + query.needle_len()).min(text.len()));
    }
    let column = ranges.first().map_or(0, |range| range.start);
    let (excerpt, ranges) = excerpt(text, &ranges);
    LineMatch {
        line,
        column,
        excerpt,
        ranges,
    }
}

/// The part of `line` to show and the match ranges moved into it: the line without its
/// indentation, or, past [`EXCERPT_BYTES`], a window opening [`EXCERPT_LEAD_BYTES`] before
/// the first match.
fn excerpt(line: &[u8], ranges: &[Range<usize>]) -> (Vec<u8>, Vec<Range<usize>>) {
    let first = ranges.first().map_or(0, |range| range.start);
    let indent = line
        .iter()
        .take_while(|&&byte| byte == b' ' || byte == b'\t')
        .count();
    let mut start = indent.min(first);
    if line.len() - start > EXCERPT_BYTES {
        start = start.max(first.saturating_sub(EXCERPT_LEAD_BYTES));
        while start < first && is_continuation(line[start]) {
            start += 1;
        }
    }
    let mut end = line.len().min(start + EXCERPT_BYTES);
    while end > start && end < line.len() && is_continuation(line[end]) {
        end -= 1;
    }
    let mut moved = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.start < end && range.end > start {
            moved.push(range.start.max(start) - start..range.end.min(end) - start);
        }
    }
    (line[start..end].to_vec(), moved)
}

/// Whether `byte` continues a UTF-8 character, so a cut before it would split one.
fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::classify;

    fn search(data: &[u8], query: &str) -> FileSearch {
        let query = ContentQuery::new(query.as_bytes()).unwrap();
        search_content(&classify(data.to_vec()), &query, &mut Vec::new())
    }

    fn lines(found: FileSearch) -> Vec<LineMatch> {
        let FileSearch::Searched { lines, .. } = found else {
            panic!("expected a searched file, got {found:?}");
        };
        lines
    }

    #[test]
    fn an_empty_query_is_refused() {
        assert!(ContentQuery::new(b"").is_none());
    }

    #[test]
    fn lowercase_queries_ignore_ascii_case_and_uppercase_ones_do_not() {
        let data = b"Hello\nhello\nHELLO\n";
        let found: Vec<_> = lines(search(data, "hello"))
            .iter()
            .map(|m| m.line)
            .collect();
        assert_eq!(found, [0, 1, 2]);
        let found: Vec<_> = lines(search(data, "Hello"))
            .iter()
            .map(|m| m.line)
            .collect();
        assert_eq!(found, [0]);
        assert!(!ContentQuery::new(b"Hello").unwrap().ignore_case);
    }

    #[test]
    fn letters_outside_ascii_match_exactly() {
        assert!(lines(search("Đường\n".as_bytes(), "đường")).is_empty());
        assert_eq!(lines(search("Đường\n".as_bytes(), "Đường")).len(), 1);
    }

    #[test]
    fn every_match_on_a_line_is_one_record_with_a_range_each() {
        let found = lines(search(b"a\n  foo bar foo\nb\n", "foo"));
        assert_eq!(
            found,
            [LineMatch {
                line: 1,
                column: 2,
                excerpt: b"foo bar foo".to_vec(),
                ranges: vec![0..3, 8..11],
            }]
        );
    }

    #[test]
    fn the_line_ending_stays_out_of_the_excerpt() {
        let found = lines(search(b"x foo\r\ny\n", "foo"));
        assert_eq!(found[0].excerpt, b"x foo");
        let found = lines(search(b"last foo", "foo"));
        assert_eq!(found[0].excerpt, b"last foo");
    }

    #[test]
    fn a_long_line_is_cut_to_a_window_before_its_first_match() {
        let mut line = vec![b'x'; 500];
        line.extend_from_slice(b"needle");
        line.extend(vec![b'y'; 500]);
        let found = lines(search(&line, "needle"));
        let first = &found[0];
        assert_eq!(first.column, 500);
        assert_eq!(first.excerpt.len(), EXCERPT_BYTES);
        assert_eq!(first.ranges.len(), 1);
        assert_eq!(first.ranges[0], EXCERPT_LEAD_BYTES..EXCERPT_LEAD_BYTES + 6);
        assert_eq!(&first.excerpt[first.ranges[0].clone()], b"needle");
    }

    #[test]
    fn the_window_never_splits_a_character() {
        let mut line = "é".repeat(200).into_bytes();
        line.extend_from_slice(b"needle");
        line.extend("é".repeat(200).into_bytes());
        let found = lines(search(&line, "needle"));
        let excerpt = &found[0].excerpt;
        assert!(std::str::from_utf8(excerpt).is_ok(), "{excerpt:?}");
        assert!(excerpt.len() <= EXCERPT_BYTES);
        assert_eq!(&excerpt[found[0].ranges[0].clone()], b"needle");
    }

    #[test]
    fn a_match_the_window_leaves_out_is_dropped_from_the_ranges() {
        let mut line = b"needle".to_vec();
        line.extend(vec![b' '; 400]);
        line.extend_from_slice(b"needle");
        let found = lines(search(&line, "needle"));
        assert_eq!(
            found[0].ranges.len(),
            1,
            "the second match is past the window"
        );
        assert_eq!(found[0].ranges[0], 0..6);
    }

    #[test]
    fn lines_stop_at_the_cap_and_say_so() {
        let data = "hit\n".repeat(MAX_LINES_PER_FILE + 1);
        let FileSearch::Searched { lines, truncated } = search(data.as_bytes(), "hit") else {
            panic!("expected a searched file");
        };
        assert_eq!(lines.len(), MAX_LINES_PER_FILE);
        assert!(truncated);
        let data = "hit\n".repeat(MAX_LINES_PER_FILE);
        let FileSearch::Searched { lines, truncated } = search(data.as_bytes(), "hit") else {
            panic!("expected a searched file");
        };
        assert_eq!(lines.len(), MAX_LINES_PER_FILE);
        assert!(!truncated);
    }

    #[test]
    fn binary_and_oversized_content_is_not_searched() {
        assert_eq!(search(b"hit\0hit", "hit"), FileSearch::Binary);
        let query = ContentQuery::new(b"hit").unwrap();
        let too_large = BlobContent::TooLarge { size: 9, limit: 4 };
        assert_eq!(
            search_content(&too_large, &query, &mut Vec::new()),
            FileSearch::TooLarge { size: 9, limit: 4 }
        );
    }
}
