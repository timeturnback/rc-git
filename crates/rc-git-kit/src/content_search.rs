//! Port of `ContentSearch.swift`: Find in Repository. A search reads file contents a batch at
//! a time over a file list the caller already has (the path index), so nothing new walks the
//! tree; each batch is one core call bounded by its byte budget, run through
//! [`crate::blocking::blocking`], and the search stops at [`MAX_MATCHED_LINES`].
//! [`ContentSearchSession`] is the panel's session: it runs a query, collects hits as batches
//! land, and keeps only the newest query's results.
//!
//! Excerpt highlights stay byte ranges of the excerpt; the UTF-16 mapping has no successor.
//! One rule survives it: an excerpt that did not decode cleanly gets no highlights, since a
//! replacement character shifts every later offset.
//!
//! A search stops when its task is dropped: the next batch is never asked for.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, BackgroundExecutor, Context, EventEmitter, Task};
use rc_git_core::{ContentQuery, EntryKind, FileSearch, LineMatch, Oid, SearchBatch, SearchLimits};

use crate::blocking::blocking;
use crate::path_search::{PathIndex, PathIndexEntry};
use crate::repository::{ReadResult, RepositoryError, RepositoryReading};
use crate::text_document::grouped;

/// Matching lines after which a search stops, so a one-letter query cannot fill memory.
pub const MAX_MATCHED_LINES: usize = 2_000;
/// Files asked for in one core call; the core may stop sooner at its byte budget.
pub const FILES_PER_CALL: usize = 1_024;

/// One line that holds the query, ready to show.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentLineMatch {
    /// Zero-based line number.
    pub line: usize,
    /// Byte offset of the first match from the start of the line.
    pub column: usize,
    /// Length of one match in bytes: the query's UTF-8 length.
    pub match_length: usize,
    /// The line, or a window of a long one, without indentation or line ending; decoded
    /// lossily.
    pub excerpt: String,
    /// Byte ranges of the matches in `excerpt`. Empty when the excerpt was not valid UTF-8.
    pub highlights: Vec<Range<usize>>,
}

impl ContentLineMatch {
    /// A match from the core, for a query `match_length` bytes long.
    #[must_use]
    pub fn new(found: LineMatch, match_length: usize) -> Self {
        let LineMatch {
            line,
            column,
            excerpt,
            ranges,
        } = found;
        let (excerpt, highlights) = match String::from_utf8(excerpt) {
            Ok(text) => {
                let mut kept = Vec::with_capacity(ranges.len());
                for range in ranges {
                    if text.get(range.clone()).is_some() {
                        kept.push(range);
                    }
                }
                (text, kept)
            }
            Err(invalid) => (
                String::from_utf8_lossy(invalid.as_bytes()).into_owned(),
                Vec::new(),
            ),
        };
        Self {
            line,
            column,
            match_length,
            excerpt,
            highlights,
        }
    }
}

/// One file with matching lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentSearchHit {
    /// The file.
    pub entry: PathIndexEntry,
    /// Matching lines in file order.
    pub lines: Vec<ContentLineMatch>,
    /// Whether the file had more matching lines than `lines` holds.
    pub truncated: bool,
}

/// How far a content search got and what it could not read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ContentSearchProgress {
    /// Files the search will read.
    pub total_files: usize,
    /// Files read so far, matching or not.
    pub searched_files: usize,
    /// Files with at least one matching line.
    pub matched_files: usize,
    /// Matching lines found.
    pub matched_lines: usize,
    /// Files skipped as binary or over the size limit.
    pub skipped_files: usize,
    /// Working-tree files that could not be read.
    pub unreadable_files: usize,
    /// Whether the search stopped at [`MAX_MATCHED_LINES`].
    pub stopped_at_limit: bool,
    /// Whether the file list stopped at [`PathIndex::MAX_ENTRIES`], so files may be missing.
    pub index_truncated: bool,
}

impl ContentSearchProgress {
    /// Progress before any file was read.
    #[must_use]
    pub fn new(total_files: usize, index_truncated: bool) -> Self {
        Self {
            total_files,
            index_truncated,
            ..Self::default()
        }
    }

    /// Counts one file's result, returning its hit when it matched.
    pub fn count(
        &mut self,
        result: &FileSearch,
        entry: &PathIndexEntry,
        match_length: usize,
    ) -> Option<ContentSearchHit> {
        self.searched_files += 1;
        match result {
            FileSearch::Searched { lines, truncated } => {
                if lines.is_empty() {
                    return None;
                }
                self.matched_files += 1;
                self.matched_lines += lines.len();
                let mut matches = Vec::with_capacity(lines.len());
                for line in lines {
                    matches.push(ContentLineMatch::new(line.clone(), match_length));
                }
                Some(ContentSearchHit {
                    entry: entry.clone(),
                    lines: matches,
                    truncated: *truncated,
                })
            }
            FileSearch::Binary | FileSearch::TooLarge { .. } => {
                self.skipped_files += 1;
                None
            }
            FileSearch::Unreadable { .. } => {
                self.unreadable_files += 1;
                None
            }
        }
    }
}

/// The files a search reads: regular files, and hidden ones only when `include_hidden`.
/// Symlinks hold a path, not text, and are left out.
#[must_use]
pub fn searchable(entries: &[PathIndexEntry], include_hidden: bool) -> Vec<PathIndexEntry> {
    let mut files = Vec::new();
    for entry in entries {
        let regular = matches!(entry.kind, EntryKind::Blob | EntryKind::ExecutableBlob);
        if regular && (include_hidden || !entry.hidden) {
            files.push(entry.clone());
        }
    }
    files
}

/// What one search reads: the query and the searchable files, in the order hits are
/// reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSearchRequest {
    /// The literal text to find, with smart case.
    pub query: String,
    /// The files to read, already passed through [`searchable`].
    pub files: Vec<PathIndexEntry>,
    /// Whether the index the files came from stopped at its cap.
    pub index_truncated: bool,
}

/// Searches blob contents, each blob read once however many paths share it. Hits reach
/// `report` a batch at a time, in file order.
///
/// # Errors
/// When the query is empty, a core read fails, or a call reads none of the files it was
/// asked for, which would never end.
pub async fn search_blobs(
    executor: &BackgroundExecutor,
    repository: Arc<dyn RepositoryReading>,
    request: ContentSearchRequest,
    report: impl FnMut(Vec<ContentSearchHit>, &ContentSearchProgress),
) -> ReadResult<ContentSearchProgress> {
    let query = content_query(&request.query)?;
    let match_length = request.query.len();
    let mut files = Vec::with_capacity(request.files.len());
    for entry in request.files {
        if let Some(oid) = entry.oid {
            files.push((oid, entry));
        }
    }
    let search = move |oids: &[Oid]| repository.search_blobs(oids, &query, SearchLimits::default());
    let plan = Plan {
        files,
        index_truncated: request.index_truncated,
        match_length,
    };
    run(executor, plan, Arc::new(search), report).await
}

/// Searches working-tree files by path, reporting unreadable ones rather than failing.
///
/// # Errors
/// As [`search_blobs`].
pub async fn search_worktree(
    executor: &BackgroundExecutor,
    repository: Arc<dyn RepositoryReading>,
    request: ContentSearchRequest,
    report: impl FnMut(Vec<ContentSearchHit>, &ContentSearchProgress),
) -> ReadResult<ContentSearchProgress> {
    let query = content_query(&request.query)?;
    let match_length = request.query.len();
    let mut files = Vec::with_capacity(request.files.len());
    for entry in request.files {
        files.push((entry.path.clone(), entry));
    }
    let search = move |paths: &[String]| {
        repository.search_worktree_files(paths, &query, SearchLimits::default())
    };
    let plan = Plan {
        files,
        index_truncated: request.index_truncated,
        match_length,
    };
    run(executor, plan, Arc::new(search), report).await
}

fn content_query(query: &str) -> ReadResult<ContentQuery> {
    ContentQuery::new(query.as_bytes())
        .ok_or_else(|| RepositoryError::new("content search: the query is empty"))
}

/// The files of one search keyed by what the core reads them by.
struct Plan<K> {
    files: Vec<(K, PathIndexEntry)>,
    index_truncated: bool,
    match_length: usize,
}

type Search<K> = Arc<dyn Fn(&[K]) -> ReadResult<SearchBatch> + Send + Sync>;

/// Searches `plan.files` in order, [`FILES_PER_CALL`] keys per call to `search`, and hands
/// each batch's hits to `report` in file order. Files sharing a key are read once.
async fn run<K>(
    executor: &BackgroundExecutor,
    plan: Plan<K>,
    search: Search<K>,
    mut report: impl FnMut(Vec<ContentSearchHit>, &ContentSearchProgress),
) -> ReadResult<ContentSearchProgress>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
{
    let Plan {
        files,
        index_truncated,
        match_length,
    } = plan;
    let mut progress = ContentSearchProgress::new(files.len(), index_truncated);
    let keys = unique_keys(&files);
    let mut found: HashMap<K, FileSearch> = HashMap::new();
    let mut next_key = 0;
    let mut next_file = 0;
    while next_key < keys.len() && !progress.stopped_at_limit {
        let asked = keys[next_key..keys.len().min(next_key + FILES_PER_CALL)].to_vec();
        let count = asked.len();
        let call = Arc::clone(&search);
        let results = blocking(executor, move || {
            call(&asked).map(|results| (asked, results))
        })
        .await?;
        let (asked, results) = results;
        if results.is_empty() {
            return Err(RepositoryError::new(format!(
                "content search read none of {count} files"
            )));
        }
        next_key += results.len().min(count);
        for (key, result) in asked.into_iter().zip(results) {
            found.insert(key, result);
        }
        let mut hits = Vec::new();
        while let Some((key, entry)) = files.get(next_file) {
            let Some(result) = found.get(key) else {
                break;
            };
            hits.extend(progress.count(result, entry, match_length));
            next_file += 1;
            if progress.matched_lines >= MAX_MATCHED_LINES {
                progress.stopped_at_limit = true;
                break;
            }
        }
        report(hits, &progress);
    }
    Ok(progress)
}

fn unique_keys<K: Clone + Eq + Hash>(files: &[(K, PathIndexEntry)]) -> Vec<K> {
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    for (key, _) in files {
        if seen.insert(key.clone()) {
            keys.push(key.clone());
        }
    }
    keys
}

/// The panel's status line for `progress`: the count while searching, the totals and what was
/// left out once done.
#[must_use]
pub fn status_text(progress: &ContentSearchProgress, finished: bool) -> String {
    if !finished {
        if progress.total_files == 0 {
            return "Searching…".to_owned();
        }
        return format!(
            "Searching {} of {} files…",
            count_text(progress.searched_files),
            count_text(progress.total_files)
        );
    }
    let mut parts = Vec::new();
    if progress.matched_lines == 0 {
        parts.push("No matches".to_owned());
    } else {
        let lines = plural(progress.matched_lines, "line");
        parts.push(format!(
            "{lines} in {}",
            plural(progress.matched_files, "file")
        ));
    }
    if progress.stopped_at_limit {
        parts.push(format!(
            "stopped at {} lines",
            count_text(MAX_MATCHED_LINES)
        ));
    }
    if progress.index_truncated {
        let cap = count_text(PathIndex::MAX_ENTRIES);
        parts.push(format!("only the first {cap} entries searched"));
    }
    if progress.skipped_files > 0 {
        let files = plural(progress.skipped_files, "file");
        parts.push(format!("{files} binary or over 1 MB skipped"));
    }
    if progress.unreadable_files > 0 {
        parts.push(format!(
            "{} unreadable",
            plural(progress.unreadable_files, "file")
        ));
    }
    parts.join(" · ")
}

fn plural(count: usize, noun: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{} {noun}{suffix}", count_text(count))
}

fn count_text(count: usize) -> String {
    grouped(u64::try_from(count).unwrap_or(u64::MAX))
}

/// How a session hands a batch of hits back while its search runs.
pub type ContentReport = Rc<dyn Fn(Vec<ContentSearchHit>, ContentSearchProgress, &mut App)>;

/// What runs a query for a session: the browser model's content search, or a test's stand-in.
pub type ContentSearchProvider =
    Rc<dyn Fn(String, ContentReport, &mut App) -> Task<ReadResult<ContentSearchProgress>>>;

/// What the panel shows below the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentSearchStatus {
    /// Nothing searched.
    Idle,
    /// A search is running; this far so far.
    Searching(ContentSearchProgress),
    /// The search ended; this is where.
    Finished(ContentSearchProgress),
    /// The search failed; the message says why.
    Failed(String),
}

/// One row of the result list: a file, or one of its matching lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentSearchRow {
    /// The file at this index of the hits.
    File(usize),
    /// One line of a file.
    Line {
        /// Index of the file in the hits.
        file: usize,
        /// Index of the line in the file's lines.
        line: usize,
    },
}

/// What the session tells the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentSearchEvent {
    /// The hits, rows or status changed.
    Updated,
    /// Open `entry`, at `line` when one is given, else at the top.
    Open {
        /// The file to open.
        entry: PathIndexEntry,
        /// The match to select once it paints.
        line: Option<ContentLineMatch>,
    },
}

/// One Find in Repository panel session. Return searches when the field holds a query not yet
/// searched and opens the selected row otherwise, so a content search never runs per
/// keystroke.
#[derive(Default)]
pub struct ContentSearchSession {
    provider: Option<ContentSearchProvider>,
    hits: Vec<ContentSearchHit>,
    rows: Vec<ContentSearchRow>,
    status: Option<ContentSearchStatus>,
    /// The query whose results are on screen or loading; `None` before the first search.
    searched_query: Option<String>,
    generation: u64,
    running: Option<Task<()>>,
}

impl EventEmitter<ContentSearchEvent> for ContentSearchSession {}

impl ContentSearchSession {
    /// A session with no provider; set one with [`Self::set_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets what runs a query.
    pub fn set_provider(&mut self, provider: ContentSearchProvider) {
        self.provider = Some(provider);
    }

    /// The files with hits, in report order.
    #[must_use]
    pub fn hits(&self) -> &[ContentSearchHit] {
        &self.hits
    }

    /// The rows of the result list.
    #[must_use]
    pub fn rows(&self) -> &[ContentSearchRow] {
        &self.rows
    }

    /// What the panel shows below the field.
    #[must_use]
    pub fn status(&self) -> ContentSearchStatus {
        self.status.clone().unwrap_or(ContentSearchStatus::Idle)
    }

    /// The query whose results are on screen or loading.
    #[must_use]
    pub fn searched_query(&self) -> Option<&str> {
        self.searched_query.as_deref()
    }

    /// Stops any search and empties the list.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.running = None;
        self.generation += 1;
        self.hits.clear();
        self.rows.clear();
        self.status = None;
        self.searched_query = None;
        cx.notify();
    }

    /// Return in the field: searches `query` when it is not the query on screen, else opens
    /// `selected_row`, or the first row when none is selected. `true` when it searched.
    pub fn submit(
        &mut self,
        query: &str,
        selected_row: Option<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.searched_query.as_deref() == Some(query) {
            self.open(selected_row.unwrap_or(0), cx);
            return false;
        }
        self.search(query, cx)
    }

    /// Starts `query`, stopping any search still running. An empty query clears the list.
    /// `true` when it searched.
    pub fn search(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        self.reset(cx);
        let Some(provider) = self.provider.clone().filter(|_| !query.is_empty()) else {
            cx.emit(ContentSearchEvent::Updated);
            return false;
        };
        let generation = self.generation;
        self.searched_query = Some(query.to_owned());
        self.status = Some(ContentSearchStatus::Searching(
            ContentSearchProgress::default(),
        ));
        cx.emit(ContentSearchEvent::Updated);
        let session = cx.weak_entity();
        // Deferred, so a provider may report from inside this very call, while the session
        // is still being updated.
        let report: ContentReport = Rc::new(move |hits, progress, cx: &mut App| {
            let session = session.clone();
            cx.defer(move |cx| {
                let _ = session.update(cx, |session, cx| {
                    if session.generation != generation {
                        return;
                    }
                    session.append(hits);
                    session.status = Some(ContentSearchStatus::Searching(progress));
                    cx.emit(ContentSearchEvent::Updated);
                    cx.notify();
                });
            });
        });
        let search = provider(query.to_owned(), report, cx);
        self.running = Some(cx.spawn(async move |this, cx| {
            let outcome = search.await;
            let _ = this.update(cx, |session, cx| session.finish(generation, outcome, cx));
        }));
        true
    }

    fn finish(
        &mut self,
        generation: u64,
        outcome: ReadResult<ContentSearchProgress>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.status = Some(match outcome {
            Ok(progress) => ContentSearchStatus::Finished(progress),
            Err(error) => ContentSearchStatus::Failed(error.message().to_owned()),
        });
        cx.emit(ContentSearchEvent::Updated);
        cx.notify();
    }

    /// Opens `row`: a line at its match, a file at its first match. A row outside the list
    /// opens nothing.
    pub fn open(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(row) else {
            return;
        };
        let (file, line) = match *row {
            ContentSearchRow::File(file) => (file, 0),
            ContentSearchRow::Line { file, line } => (file, line),
        };
        let Some(hit) = self.hits.get(file) else {
            return;
        };
        cx.emit(ContentSearchEvent::Open {
            entry: hit.entry.clone(),
            line: hit.lines.get(line).cloned(),
        });
    }

    fn append(&mut self, batch: Vec<ContentSearchHit>) {
        for hit in batch {
            let file = self.hits.len();
            self.rows.push(ContentSearchRow::File(file));
            for line in 0..hit.lines.len() {
                self.rows.push(ContentSearchRow::Line { file, line });
            }
            self.hits.push(hit);
        }
    }
}
