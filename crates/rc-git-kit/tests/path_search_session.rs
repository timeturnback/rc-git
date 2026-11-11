//! Ported from `PathSearchSessionTests.swift`: which query's result stands, and when a Return
//! opens a row. The provider holds each query open until the test releases it, so a query is
//! in flight for exactly as long as the test wants.
#![expect(
    clippy::unwrap_used,
    reason = "a release names a query the test just started"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::EntryKind;
use rc_git_kit::oneshot::{self, Sender};
use rc_git_kit::path_search::PathIndexEntry;
use rc_git_kit::path_search_session::{
    PathSearchError, PathSearchEvent, PathSearchResult, PathSearchSession,
};

type Answer = Result<PathSearchResult, PathSearchError>;

#[derive(Default)]
struct Gated {
    waiting: HashMap<String, Sender<Answer>>,
    calls: Vec<String>,
}

#[derive(Clone, Default)]
struct GatedProvider(Rc<RefCell<Gated>>);

impl GatedProvider {
    fn release(&self, query: &str, paths: &[&str]) {
        let entries = paths
            .iter()
            .map(|path| PathIndexEntry::new((*path).to_owned(), None, EntryKind::Blob, false, true))
            .collect();
        let sender = self.0.borrow_mut().waiting.remove(query).unwrap();
        sender.send(Ok(PathSearchResult {
            entries,
            truncated: false,
        }));
    }

    fn fail(&self, query: &str) {
        let sender = self.0.borrow_mut().waiting.remove(query).unwrap();
        sender.send(Err(PathSearchError::Failed("gated failure".to_owned())));
    }

    fn calls(&self) -> Vec<String> {
        self.0.borrow().calls.clone()
    }
}

type Opened = Rc<RefCell<Vec<String>>>;

fn session(cx: &mut TestAppContext) -> (Entity<PathSearchSession>, GatedProvider, Opened) {
    let provider = GatedProvider::default();
    let gated = provider.clone();
    let session = cx.new(|_| {
        let mut session = PathSearchSession::new();
        session.set_provider(Rc::new(move |query, cx| {
            let (sender, receiver) = oneshot::channel();
            let mut state = gated.0.borrow_mut();
            state.calls.push(query.clone());
            state.waiting.insert(query, sender);
            cx.background_executor()
                .spawn(async move { receiver.await.unwrap_or(Err(PathSearchError::Cancelled)) })
        }));
        session
    });
    let opened: Opened = Rc::default();
    let log = Rc::clone(&opened);
    cx.update(|cx| {
        cx.subscribe(&session, move |_, event, _| {
            if let PathSearchEvent::Open(entry) = event {
                log.borrow_mut().push(entry.path.clone());
            }
        })
        .detach();
    });
    (session, provider, opened)
}

fn search(session: &Entity<PathSearchSession>, cx: &mut TestAppContext, query: &str) {
    assert!(session.update(cx, |s, cx| s.search(query, cx)));
}

fn open(session: &Entity<PathSearchSession>, cx: &mut TestAppContext, row: usize) {
    session.update(cx, |s, cx| s.open(row, cx));
}

fn results(session: &Entity<PathSearchSession>, cx: &mut TestAppContext) -> Vec<String> {
    session.read_with(cx, |s, _| {
        s.results().iter().map(|e| e.path.clone()).collect()
    })
}

#[gpui::test]
fn return_during_a_newer_query_opens_its_first_match_not_the_older_list(cx: &mut TestAppContext) {
    let (session, provider, opened) = session(cx);
    search(&session, cx, "");
    provider.release("", &["a/first.txt", "b/compile.rs"]);
    cx.run_until_parked();

    search(&session, cx, "comp");
    open(&session, cx, 0);
    assert!(opened.borrow().is_empty());
    provider.release("comp", &["b/compile.rs"]);
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), ["b/compile.rs"]);
}

#[gpui::test]
fn return_before_the_first_list_lands_opens_its_first_match(cx: &mut TestAppContext) {
    let (session, provider, opened) = session(cx);
    session.update(cx, PathSearchSession::reset);
    search(&session, cx, "");
    open(&session, cx, 0);
    assert!(opened.borrow().is_empty());
    provider.release("", &["README.md"]);
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), ["README.md"]);
}

#[gpui::test]
fn an_older_query_landing_late_is_dropped_and_opens_nothing(cx: &mut TestAppContext) {
    let (session, provider, opened) = session(cx);
    search(&session, cx, "c");
    search(&session, cx, "co");
    open(&session, cx, 0);

    provider.release("c", &["c/stale.txt"]);
    cx.run_until_parked();
    assert!(results(&session, cx).is_empty());
    assert!(opened.borrow().is_empty());

    provider.release("co", &["co/fresh.txt"]);
    cx.run_until_parked();
    assert_eq!(results(&session, cx), ["co/fresh.txt"]);
    assert_eq!(*opened.borrow(), ["co/fresh.txt"]);
}

#[gpui::test]
fn return_on_a_landed_list_opens_the_selected_row(cx: &mut TestAppContext) {
    let (session, provider, opened) = session(cx);
    search(&session, cx, "x");
    provider.release("x", &["x1", "x2"]);
    cx.run_until_parked();
    open(&session, cx, 1);
    open(&session, cx, 5);
    assert_eq!(*opened.borrow(), ["x2"]);
}

#[gpui::test]
fn a_pending_open_is_dropped_on_cancel_or_failure(cx: &mut TestAppContext) {
    let (session, provider, opened) = session(cx);
    search(&session, cx, "a");
    open(&session, cx, 0);
    session.update(cx, |s, _| s.cancel_pending_open());
    provider.release("a", &["a.txt"]);
    cx.run_until_parked();

    search(&session, cx, "b");
    open(&session, cx, 0);
    provider.fail("b");
    cx.run_until_parked();
    assert!(results(&session, cx).is_empty());

    search(&session, cx, "c");
    provider.release("c", &["c.txt"]);
    cx.run_until_parked();
    assert!(opened.borrow().is_empty());
    assert_eq!(provider.calls(), ["a", "b", "c"]);
}

#[gpui::test]
fn reset_drops_an_in_flight_result_and_the_pending_open(cx: &mut TestAppContext) {
    let (session, provider, opened) = session(cx);
    search(&session, cx, "a");
    open(&session, cx, 0);
    session.update(cx, PathSearchSession::reset);
    provider.release("a", &["a.txt"]);
    cx.run_until_parked();
    assert!(results(&session, cx).is_empty());
    assert!(opened.borrow().is_empty());
}

#[gpui::test]
fn a_superseded_query_is_ignored_and_a_failure_is_reported(cx: &mut TestAppContext) {
    let (session, provider, _) = session(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&session, move |_, event: &PathSearchEvent, _| {
            log.borrow_mut().push(event.clone());
        })
        .detach();
    });
    search(&session, cx, "a");
    let sender = provider.0.borrow_mut().waiting.remove("a").unwrap();
    sender.send(Err(PathSearchError::Cancelled));
    cx.run_until_parked();
    assert!(events.borrow().is_empty(), "a cancelled query says nothing");
    search(&session, cx, "b");
    provider.fail("b");
    cx.run_until_parked();
    assert_eq!(
        *events.borrow(),
        [PathSearchEvent::Failed("gated failure".to_owned())]
    );
}
