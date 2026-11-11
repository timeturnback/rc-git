//! Ported from `HighlightingTests.swift` and `ImportLinksTests.swift`: the highlighter and
//! import links through kit, the hit map in bytes, and the model-level span cache
//! (`ModelHighlightTests`), now on `HighlightStore`. The cache's job is saving parses, so its
//! parse counter backs the Swift tests' hit-or-miss probes.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::Oid;
use rc_git_highlight::{Class, ImportLink, Span};
use rc_git_kit::fake::oid;
use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::highlight_store::{DEFAULT_ENTRY_LIMIT, HighlightStore};
use rc_git_kit::highlighting::{
    BYTE_LIMIT, Plain, highlight, highlight_fence, is_first_failure, language_for_file_name,
};
use rc_git_kit::import_links::{Hit, ImportLinkMap, find};
use rc_git_kit::text_document::TextDocument;

/// Rust just under the byte cap with a token every byte or two: one statement after another in
/// one body, so a full parse takes long enough to cancel partway.
static SLOW_RUST: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let statement = "a=(b+c)*d;";
    let count = (BYTE_LIMIT - 10) / statement.len();
    format!("fn f(){{{}}}\n", statement.repeat(count)).into_bytes()
});

fn has(spans: &[Span], class: Class) -> bool {
    spans.iter().any(|span| span.class == class)
}

fn document(text: &str) -> TextDocument {
    TextDocument::new(text.as_bytes().to_vec(), &[0], true)
}

#[test]
fn rust_source_gets_spans_and_unknown_extensions_get_none() {
    let data = b"fn main() { let s = \"hi\"; }\n";
    let spans = highlight("main.rs", data, None).unwrap();
    assert!(has(&spans, Class::Keyword) && has(&spans, Class::String));
    assert_eq!(
        highlight("photo.png", data, None),
        Err(Plain::UnknownLanguage)
    );
    assert_eq!(highlight("noext", data, None), Err(Plain::UnknownLanguage));
    assert_eq!(language_for_file_name("README.md"), Some("markdown"));
    assert_eq!(language_for_file_name("noext"), None);
}

#[test]
fn fence_labels_highlight_only_as_fences() {
    let data = b"fn main() {}\n";
    assert_eq!(highlight("rust", data, None), Err(Plain::UnknownLanguage));
    assert!(has(&highlight_fence("rust", data).unwrap(), Class::Keyword));
    assert_eq!(
        highlight_fence("mermaid", data),
        Err(Plain::UnknownLanguage)
    );
}

#[test]
fn a_signalled_cancel_stops_the_parse() {
    let data = b"fn main() {}\n";
    let cancel = CancelToken::new();
    cancel.signal();
    assert_eq!(
        highlight("main.rs", data, Some(&cancel)),
        Err(Plain::Cancelled)
    );
    assert!(highlight("main.rs", data, Some(&CancelToken::new())).is_ok());
}

#[test]
fn a_cancel_stops_the_parse_well_before_it_would_end() {
    let started = Instant::now();
    highlight("big.rs", &SLOW_RUST, None).unwrap();
    let full = started.elapsed();
    assert!(
        full >= Duration::from_millis(500),
        "a full parse is the slow side: {full:?}"
    );

    let cancel = Arc::new(CancelToken::new());
    let signal = Arc::clone(&cancel);
    let pending = std::thread::spawn(move || {
        let outcome = highlight("big.rs", &SLOW_RUST, Some(&cancel));
        (outcome, Instant::now())
    });
    std::thread::sleep(Duration::from_millis(100));
    let signalled = Instant::now();
    signal.signal();
    let (outcome, finished) = pending.join().unwrap();
    assert_eq!(outcome, Err(Plain::Cancelled));
    let elapsed = finished.duration_since(signalled);
    assert!(
        elapsed < full / 2,
        "stopped after {elapsed:?}; a full parse took {full:?}"
    );
}

#[test]
fn a_failure_message_is_reported_once() {
    let message = format!("highlighting test-{:?}: query", Instant::now());
    assert!(is_first_failure(&message));
    assert!(!is_first_failure(&message));
    assert!(is_first_failure(&format!("{message} again")));
}

#[test]
fn oversized_input_stays_plain() {
    let big = vec![b'a'; BYTE_LIMIT + 1];
    assert_eq!(highlight("big.rs", &big, None), Err(Plain::TooLarge));
    assert!(highlight("big.rs", &big[..BYTE_LIMIT], None).is_ok());
}

#[test]
fn markdown_and_typescript_highlight() {
    let markdown = b"# Title\n\n```rust\nfn f() {}\n```\n";
    let spans = highlight("README.md", markdown, None).unwrap();
    assert!(has(&spans, Class::Heading));
    assert!(has(&spans, Class::Keyword), "fenced rust keeps its color");
    let shape = b"interface Shape { side: number }\n";
    let spans = highlight("shape.ts", shape, None).unwrap();
    assert!(has(&spans, Class::Keyword) && has(&spans, Class::Type));
    assert!(!highlight("App.tsx", shape, None).unwrap().is_empty());
}

fn clicked(link: &ImportLink, text: &str) -> String {
    text[link.start as usize..link.end as usize].to_owned()
}

#[test]
fn typescript_imports_and_components_are_links() {
    let text = "import Panel from './Panel';\nconst view = <Panel />;\n";
    let links = find("App.tsx", text.as_bytes()).unwrap();
    let names: Vec<String> = links.iter().map(|link| clicked(link, text)).collect();
    assert_eq!(names, ["Panel", "./Panel", "Panel"]);
    assert!(
        links
            .iter()
            .all(|l| l.specifier_end - l.specifier_start == 7)
    );
}

#[test]
fn other_languages_have_no_links_and_unknown_ones_have_no_answer() {
    assert_eq!(find("main.rs", b"use std::fmt;\n"), Ok(Vec::new()));
    assert_eq!(find("plain.ts", b"const x = 1;\n"), Ok(Vec::new()));
    assert_eq!(
        find("photo.png", b"import a from './a';\n"),
        Err(Plain::UnknownLanguage)
    );
}

#[test]
fn text_over_the_limit_is_not_parsed_and_text_at_it_is() {
    assert_eq!(
        find("big.ts", &vec![b'a'; BYTE_LIMIT + 1]),
        Err(Plain::TooLarge)
    );
    let head = "import a from './a';\n";
    let text = format!("{head}{}", " ".repeat(BYTE_LIMIT - head.len()));
    assert_eq!(find("big.ts", text.as_bytes()).unwrap().len(), 2);
}

#[test]
fn the_map_answers_inside_a_link_and_nowhere_else() {
    let text: Arc<str> = Arc::from("import a from './mod';\n");
    let link = |start, end| ImportLink {
        start,
        end,
        specifier_start: 15,
        specifier_end: 20,
    };
    let map = ImportLinkMap::new(vec![link(7, 8), link(15, 20)], Arc::clone(&text));
    assert!(!map.is_empty());
    assert_eq!(map.hit(6), None);
    assert_eq!(
        map.hit(7),
        Some(Hit {
            range: 7..8,
            specifier: "./mod".to_owned()
        })
    );
    assert_eq!(map.hit(8), None, "ranges are half-open");
    assert_eq!(map.hit(19).unwrap().range, 15..20);
    assert_eq!(map.hit(20), None);
    assert_eq!(map.hit(10_000), None);
    assert_eq!(ImportLinkMap::new(Vec::new(), text).hit(0), None);
}

#[test]
fn the_map_counts_in_bytes_past_multibyte_text() {
    // "é" is two bytes and "🙂" four, so every offset after them is a byte offset.
    let text = "// é🙂\nimport a from './é';\n";
    let links = find("a.ts", text.as_bytes()).unwrap();
    let map = ImportLinkMap::new(links, Arc::from(text));
    let name = text.find("a from").unwrap();
    assert_eq!(
        map.hit(name),
        Some(Hit {
            range: name..name + 1,
            specifier: "./é".to_owned()
        })
    );
    let quote = text.find('\'').unwrap();
    assert_eq!(map.hit(quote), None, "the quote is not part of the link");
    assert_eq!(map.hit(quote + 1).unwrap().range, quote + 1..quote + 5);
}

#[test]
fn a_specifier_that_cuts_a_character_is_no_hit() {
    let links = vec![ImportLink {
        start: 3,
        end: 4,
        specifier_start: 1,
        specifier_end: 2,
    }];
    assert_eq!(ImportLinkMap::new(links, Arc::from("é = 1")).hit(3), None);
}

fn store(cx: &mut TestAppContext, entries: usize, spans: usize) -> Entity<HighlightStore> {
    cx.executor().allow_parking();
    cx.new(|_| HighlightStore::new(entries, spans))
}

async fn spans_of(
    store: &Entity<HighlightStore>,
    cx: &mut TestAppContext,
    name: &str,
    oid: Option<Oid>,
    document: &TextDocument,
) -> Option<Arc<[Span]>> {
    let task = store.update(cx, |store, cx| store.highlights(name, oid, document, cx));
    task.await
}

fn parses(store: &Entity<HighlightStore>, cx: &mut TestAppContext) -> usize {
    store.read_with(cx, |store, _| store.parses())
}

const PLAIN_A: &str = "fn main() {}\n";
const PLAIN_B: &str = "let s = \"hi\";\n";

fn rust_name(seed: u8) -> String {
    format!("f{seed}.rs")
}

#[gpui::test]
async fn highlights_are_cached_by_blob_and_skip_lossy_text(cx: &mut TestAppContext) {
    let store = store(cx, DEFAULT_ENTRY_LIMIT, 1_500_000);
    let first = spans_of(&store, cx, "main.rs", Some(oid(0x42)), &document(PLAIN_A)).await;
    assert!(!first.clone().unwrap().is_empty());
    // Same blob id, different text: only a cache hit answers with the first parse.
    let other = spans_of(&store, cx, "main.rs", Some(oid(0x42)), &document("x\n")).await;
    assert_eq!(other, first);
    assert_eq!(parses(&store, cx), 1);

    let lossy = TextDocument::new(vec![0x66, 0xFF, 0x6F], &[0], false);
    assert!(lossy.is_lossy);
    let none = spans_of(&store, cx, "bad.rs", Some(oid(0x43)), &lossy).await;
    assert_eq!(none, None, "misaligned offsets are worse than plain text");
    assert_eq!(parses(&store, cx), 1, "lossy text is never parsed");
}

#[gpui::test]
async fn a_blob_under_another_extension_is_highlighted_for_that_name(cx: &mut TestAppContext) {
    let store = store(cx, DEFAULT_ENTRY_LIMIT, 1_500_000);
    let doc = document("fn f() {}\n");
    let rust = spans_of(&store, cx, "a.rs", Some(oid(0x51)), &doc).await;
    assert!(rust.is_some());
    let backup = spans_of(&store, cx, "a.bak", Some(oid(0x51)), &doc).await;
    assert_eq!(
        backup, None,
        "the same blob under an unknown extension has no language"
    );
    let upper = spans_of(&store, cx, "A.RS", Some(oid(0x51)), &doc).await;
    assert_eq!(upper, rust);
    assert_eq!(
        parses(&store, cx),
        2,
        "the upper-case extension shares the entry"
    );
}

#[gpui::test]
async fn the_cache_evicts_the_least_recently_used_past_the_entry_cap(cx: &mut TestAppContext) {
    let limit = 4;
    let store = store(cx, limit, 1_500_000);
    for seed in 1..=4 {
        spans_of(
            &store,
            cx,
            &rust_name(seed),
            Some(oid(seed)),
            &document(PLAIN_A),
        )
        .await;
    }
    // Touch the first, so the second is the least recently used.
    spans_of(&store, cx, &rust_name(1), Some(oid(1)), &document(PLAIN_B)).await;
    spans_of(&store, cx, &rust_name(5), Some(oid(5)), &document(PLAIN_A)).await;
    assert_eq!(parses(&store, cx), 5);
    assert_eq!(store.read_with(cx, |store, _| store.len()), limit);

    let touched = spans_of(&store, cx, &rust_name(1), Some(oid(1)), &document(PLAIN_B)).await;
    let first_parse = highlight("f.rs", PLAIN_A.as_bytes(), None).unwrap();
    assert_eq!(
        touched.as_deref(),
        Some(first_parse.as_slice()),
        "touched stays"
    );
    assert_eq!(parses(&store, cx), 5);
    spans_of(&store, cx, &rust_name(2), Some(oid(2)), &document(PLAIN_B)).await;
    assert_eq!(
        parses(&store, cx),
        6,
        "the least recently used entry went instead"
    );
}

#[gpui::test]
async fn the_cache_evicts_past_the_span_cap_but_keeps_one_entry(cx: &mut TestAppContext) {
    let small = document("fn f() {}\n");
    let count = highlight("f.rs", b"fn f() {}\n", None).unwrap().len();
    assert!(
        count > 5 && count <= 10,
        "two entries overflow a cap of 10: {count}"
    );
    let store = store(cx, 16, 10);
    spans_of(&store, cx, &rust_name(1), Some(oid(1)), &small).await;
    spans_of(&store, cx, &rust_name(2), Some(oid(2)), &small).await;
    spans_of(&store, cx, &rust_name(2), Some(oid(2)), &small).await;
    assert_eq!(parses(&store, cx), 2);
    spans_of(&store, cx, &rust_name(1), Some(oid(1)), &small).await;
    assert_eq!(
        parses(&store, cx),
        3,
        "the older entry was evicted for the span cap"
    );

    let tiny = store_with(cx, 16, 1);
    spans_of(&tiny, cx, &rust_name(1), Some(oid(1)), &small).await;
    spans_of(&tiny, cx, &rust_name(1), Some(oid(1)), &small).await;
    assert_eq!(
        parses(&tiny, cx),
        1,
        "a single entry over the cap is still cached"
    );
}

fn store_with(cx: &mut TestAppContext, entries: usize, spans: usize) -> Entity<HighlightStore> {
    cx.new(|_| HighlightStore::new(entries, spans))
}

#[gpui::test]
async fn unhashed_working_tree_files_each_get_their_own_spans(cx: &mut TestAppContext) {
    let store = store(cx, DEFAULT_ENTRY_LIMIT, 1_500_000);
    let short = "fn a() {}\n";
    let long = "// note\nfn b() { let s = \"hi\"; }\n";
    let first = spans_of(&store, cx, "a.rs", None, &document(short))
        .await
        .unwrap();
    let second = spans_of(&store, cx, "b.rs", None, &document(long))
        .await
        .unwrap();
    assert_eq!(*first, *highlight("a.rs", short.as_bytes(), None).unwrap());
    assert_eq!(*second, *highlight("b.rs", long.as_bytes(), None).unwrap());
    assert_ne!(
        first, second,
        "two disk files must not share one cache slot"
    );
    assert!(store.read_with(cx, |store, _| store.is_empty()));
}

#[gpui::test]
async fn a_superseded_highlight_resolves_to_nothing_and_caches_nothing(cx: &mut TestAppContext) {
    let store = store(cx, DEFAULT_ENTRY_LIMIT, 1_500_000);
    let big = TextDocument::new(SLOW_RUST.clone(), &[0], true);
    let pending = store.update(cx, |store, cx| {
        store.highlights("big.rs", Some(oid(0x61)), &big, cx)
    });
    std::thread::sleep(Duration::from_millis(100));
    store.update(cx, |store, _| store.cancel_running());
    assert_eq!(pending.await, None, "the file it was for is no longer open");
    let again = spans_of(&store, cx, "big.rs", Some(oid(0x61)), &document(PLAIN_A)).await;
    assert_eq!(
        again.as_deref(),
        Some(
            highlight("big.rs", PLAIN_A.as_bytes(), None)
                .unwrap()
                .as_slice()
        ),
        "the stopped parse left nothing in the cache"
    );
}

#[gpui::test]
async fn a_parse_that_finished_before_the_cancel_is_cached_but_not_shown(cx: &mut TestAppContext) {
    let store = store(cx, DEFAULT_ENTRY_LIMIT, 1_500_000);
    let expected = highlight("f.rs", PLAIN_A.as_bytes(), None).unwrap();
    // The parse runs on its own thread and nothing can gate it from here, so whether it
    // finishes before the cancel is a race. Each attempt uses a fresh blob; an attempt whose
    // parse the cancel caught mid-flight caches nothing and is retried. Whatever the timing,
    // a cancelled parse is never shown.
    for attempt in 0..50u8 {
        let blob = oid(0x40 + attempt);
        let pending = store.update(cx, |store, cx| {
            store.highlights("f.rs", Some(blob), &document(PLAIN_A), cx)
        });
        std::thread::sleep(Duration::from_millis(20 * u64::from(attempt + 1)));
        store.update(cx, |store, _| store.cancel_running());
        assert_eq!(pending.await, None, "another file opened meanwhile");
        let before = parses(&store, cx);
        let cached = spans_of(&store, cx, "f.rs", Some(blob), &document(PLAIN_B)).await;
        if parses(&store, cx) == before {
            assert_eq!(
                cached.as_deref(),
                Some(expected.as_slice()),
                "the finished parse was cached for the next open"
            );
            return;
        }
    }
    panic!("no parse finished before its cancel in 50 attempts");
}

#[gpui::test]
async fn import_links_live_as_long_as_their_spans(cx: &mut TestAppContext) {
    let store = store(cx, 1, 1_500_000);
    let doc = document("import a from './a';\n");
    let links_of = |store: &Entity<HighlightStore>, cx: &mut TestAppContext, seed: u8| {
        store.update(cx, |store, cx| {
            store.import_links("a.ts", Some(oid(seed)), &doc, cx)
        })
    };
    let unspanned = links_of(&store, cx, 1).await.unwrap();
    assert_eq!(unspanned.len(), 2);
    spans_of(&store, cx, "a.ts", Some(oid(1)), &doc).await;
    links_of(&store, cx, 1).await;
    let cached = links_of(&store, cx, 1);
    assert!(
        cached.is_ready(),
        "links found while the spans were held are a cache hit"
    );
    cached.await;
    spans_of(&store, cx, "a.ts", Some(oid(2)), &doc).await;
    assert!(
        !links_of(&store, cx, 1).is_ready(),
        "evicting the spans took the links too"
    );
}
