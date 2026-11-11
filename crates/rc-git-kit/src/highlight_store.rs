//! The highlight and import-link caches `BrowserModel.swift` kept, as an entity of their own
//! so the browser model owns one and the tests reach it without a repository: syntax spans
//! per blob and file extension, parsed off the UI thread, cached so a file parses once
//! however often it is reopened, and bounded, since span arrays for large files run to
//! megabytes.
//!
//! A blob's bytes never change, so the cache keys on its id plus the part of the name the
//! highlighter picks a language from. A working-tree file has no id until it is hashed and is
//! never cached. Import links live and die with their entry's spans and need no bookkeeping
//! of their own.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use gpui::{Context, EventEmitter, Task};
use rc_git_core::Oid;
use rc_git_highlight::{ImportLink, Span};

use crate::blocking::blocking;
use crate::guard_bridge::CancelToken;
use crate::highlighting::{self, BYTE_LIMIT, Plain};
use crate::import_links;
use crate::text_document::TextDocument;

/// Files the span cache holds by default.
pub const DEFAULT_ENTRY_LIMIT: usize = 16;
/// Spans the span cache holds by default, across every file.
pub const DEFAULT_SPAN_LIMIT: usize = 1_500_000;

/// A cache key: the blob plus the lowercased text after the last `.` of its name. Identical
/// bytes under `LICENSE.md` and `LICENSE` share an id but not a grammar, so the id alone would
/// hand one the other's colors.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    oid: Oid,
    extension: String,
}

impl Key {
    /// The key for a blob `oid` named `name`; `None` for a file with no id, which is never
    /// cached.
    fn new(oid: Option<Oid>, name: &str) -> Option<Self> {
        let extension = name
            .rsplit_once('.')
            .map_or_else(String::new, |(_, extension)| extension.to_lowercase());
        oid.map(|oid| Self { oid, extension })
    }
}

/// One parse in flight: its slot in the running table, where its spans go, and its cancel.
struct Run {
    id: u64,
    key: Option<Key>,
    cancel: Arc<CancelToken>,
}

/// What the store tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HighlightEvent {
    /// A grammar or parse failed with a message not seen before in this process: log it.
    Failed(String),
}

/// The bounded span and import-link caches, and the parses still running.
#[derive(Debug)]
pub struct HighlightStore {
    entry_limit: usize,
    span_limit: usize,
    spans: HashMap<Key, Arc<[Span]>>,
    /// Least recently used first.
    order: VecDeque<Key>,
    span_count: usize,
    links: HashMap<Key, Arc<[ImportLink]>>,
    /// The cancel tokens of highlights still parsing, signalled when the open file changes so
    /// a 2 MiB parse stops instead of running to the end for a file nobody is looking at.
    running: HashMap<u64, Arc<CancelToken>>,
    next_run: u64,
    parses: usize,
}

impl EventEmitter<HighlightEvent> for HighlightStore {}

impl Default for HighlightStore {
    fn default() -> Self {
        Self::new(DEFAULT_ENTRY_LIMIT, DEFAULT_SPAN_LIMIT)
    }
}

impl HighlightStore {
    /// A store holding at most `entry_limit` files and `span_limit` spans, except that the
    /// newest entry stays however many spans it has.
    #[must_use]
    pub fn new(entry_limit: usize, span_limit: usize) -> Self {
        Self {
            entry_limit,
            span_limit,
            spans: HashMap::new(),
            order: VecDeque::new(),
            span_count: 0,
            links: HashMap::new(),
            running: HashMap::new(),
            next_run: 0,
            parses: 0,
        }
    }

    /// Highlight parses started so far: a cache hit starts none.
    #[must_use]
    pub fn parses(&self) -> usize {
        self.parses
    }

    /// Files the span cache holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether the span cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Syntax spans for the text file `name` (blob `oid`, `None` on disk), from the cache or
    /// parsed off the UI thread. Resolves to `None` for an unknown language, lossy text (its
    /// offsets would misalign), text over the highlighter's cap, a failed parse, and a parse
    /// [`Self::cancel_running`] stopped or overtook.
    pub fn highlights(
        &mut self,
        name: &str,
        oid: Option<Oid>,
        document: &TextDocument,
        cx: &mut Context<Self>,
    ) -> Task<Option<Arc<[Span]>>> {
        if document.is_lossy || document.byte_count > BYTE_LIMIT {
            return Task::ready(None);
        }
        let key = Key::new(oid, name);
        if let Some(key) = &key
            && let Some(cached) = self.spans.get(key).cloned()
        {
            self.touch(key);
            return Task::ready(Some(cached));
        }
        let cancel = Arc::new(CancelToken::new());
        let run = self.next_run;
        self.next_run += 1;
        self.running.insert(run, Arc::clone(&cancel));
        self.parses += 1;
        let name = name.to_owned();
        let text = document.text.clone();
        let token = Arc::clone(&cancel);
        let parse = blocking(cx.background_executor(), move || {
            highlighting::highlight(&name, text.as_bytes(), Some(&token))
        });
        let running = Run {
            id: run,
            key,
            cancel,
        };
        cx.spawn(async move |this, cx| {
            let outcome = parse.await;
            this.update(cx, |store, cx| store.finish(running, outcome, cx))
                .ok()
                .flatten()
        })
    }

    /// Asks every highlight still parsing to stop: the file they were for is no longer open.
    pub fn cancel_running(&mut self) {
        for (_, cancel) in self.running.drain() {
            cancel.signal();
        }
    }

    /// The import links of the text file `name`, from the cache or found off the UI thread,
    /// kept for as long as the file's spans are. `None` under the same conditions as
    /// [`Self::highlights`].
    pub fn import_links(
        &mut self,
        name: &str,
        oid: Option<Oid>,
        document: &TextDocument,
        cx: &mut Context<Self>,
    ) -> Task<Option<Arc<[ImportLink]>>> {
        if document.is_lossy || document.byte_count > BYTE_LIMIT {
            return Task::ready(None);
        }
        let key = Key::new(oid, name);
        if let Some(cached) = key.as_ref().and_then(|key| self.links.get(key)) {
            return Task::ready(Some(Arc::clone(cached)));
        }
        let name = name.to_owned();
        let text = document.text.clone();
        let find = blocking(cx.background_executor(), move || {
            import_links::find(&name, text.as_bytes())
        });
        cx.spawn(async move |this, cx| {
            let found = find.await;
            this.update(cx, |store, cx| {
                let links: Arc<[ImportLink]> = Self::settle(found, cx)?.into();
                if let Some(key) = key
                    && store.spans.contains_key(&key)
                {
                    store.links.insert(key, Arc::clone(&links));
                }
                Some(links)
            })
            .ok()
            .flatten()
        })
    }

    fn finish(
        &mut self,
        run: Run,
        outcome: Result<Vec<Span>, Plain>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<[Span]>> {
        self.running.remove(&run.id);
        let spans: Arc<[Span]> = Self::settle(outcome, cx)?.into();
        if let Some(key) = run.key {
            self.store(key, Arc::clone(&spans));
        }
        // Another file opened meanwhile: a parse that finished first is cached, not shown.
        (!run.cancel.is_signalled()).then_some(spans)
    }

    /// The value of `outcome`, reporting a failure not seen before.
    fn settle<T>(outcome: Result<T, Plain>, cx: &mut Context<Self>) -> Option<T> {
        match outcome {
            Ok(value) => Some(value),
            Err(Plain::Failed(message)) => {
                if highlighting::is_first_failure(&message) {
                    cx.emit(HighlightEvent::Failed(message));
                }
                None
            }
            Err(Plain::UnknownLanguage | Plain::TooLarge | Plain::Cancelled) => None,
        }
    }

    fn touch(&mut self, key: &Key) {
        if let Some(index) = self.order.iter().position(|held| held == key) {
            self.order.remove(index);
            self.order.push_back(key.clone());
        }
    }

    /// Inserts one entry and evicts from the least recently used end until both caps hold,
    /// keeping at least the newest entry.
    fn store(&mut self, key: Key, spans: Arc<[Span]>) {
        if !self.spans.contains_key(&key) {
            self.span_count += spans.len();
            self.order.push_back(key.clone());
            self.spans.insert(key, spans);
        }
        while (self.order.len() > self.entry_limit || self.span_count > self.span_limit)
            && self.order.len() > 1
        {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(spans) = self.spans.remove(&evicted) {
                self.span_count -= spans.len();
            }
            self.links.remove(&evicted);
        }
    }
}
