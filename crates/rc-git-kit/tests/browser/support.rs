//! What the browser model suites share: a model on the fakes, event and state recorders, and
//! the waits that order a race the way the Swift tests' `await gate.waitUntilArrived()` did.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::{EntryKind, Oid, TreeEntry, WorktreeEntry, WorktreeStatus};
use rc_git_kit::blocking::blocking;
use rc_git_kit::browser::{BrowserEvent, BrowserModel, FileEntry, FileState};
use rc_git_kit::fake::{CallGate, FakeRepository};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::text_document::TextDocument;

/// A model with no repository, on a test executor that lets blocking work run on threads.
pub fn model(cx: &mut TestAppContext) -> Entity<BrowserModel> {
    cx.executor().allow_parking();
    cx.new(|cx| BrowserModel::new(Arc::new(MemorySettings::new()), cx))
}

/// Attaches `repo` and waits for the attach to land.
pub async fn attach(cx: &mut TestAppContext, model: &Entity<BrowserModel>, repo: &FakeRepository) {
    let repository = Arc::new(repo.clone());
    model
        .update(cx, |model, cx| model.attach(repository, cx))
        .await;
}

/// A tree entry.
pub fn tree(name: &str, oid: Oid, kind: EntryKind) -> TreeEntry {
    TreeEntry {
        name: name.into(),
        oid,
        kind,
    }
}

/// A working-tree entry.
pub fn disk(
    name: &str,
    kind: EntryKind,
    status: WorktreeStatus,
    oid: Option<Oid>,
) -> WorktreeEntry {
    WorktreeEntry {
        name: name.into(),
        kind,
        status,
        oid,
        size: 1,
    }
}

/// Every event the model sends, in order.
pub fn events(
    cx: &mut TestAppContext,
    model: &Entity<BrowserModel>,
) -> Rc<RefCell<Vec<BrowserEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(model, move |_, event: &BrowserEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    seen
}

/// What `read` sees each time the model sends `kind`.
pub fn record<T: 'static>(
    cx: &mut TestAppContext,
    model: &Entity<BrowserModel>,
    kind: BrowserEvent,
    read: impl Fn(&BrowserModel) -> T + 'static,
) -> Rc<RefCell<Vec<T>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(model, move |model, event: &BrowserEvent, cx| {
            if *event == kind {
                sink.borrow_mut().push(read(model.read(cx)));
            }
        })
        .detach();
    });
    seen
}

/// How many times the model sent `kind`.
pub fn count(events: &Rc<RefCell<Vec<BrowserEvent>>>, kind: &BrowserEvent) -> usize {
    events
        .borrow()
        .iter()
        .filter(|event| *event == kind)
        .count()
}

/// The messages of every [`BrowserEvent::Error`].
pub fn errors(events: &Rc<RefCell<Vec<BrowserEvent>>>) -> Vec<String> {
    let mut messages = Vec::new();
    for event in events.borrow().iter() {
        if let BrowserEvent::Error(message) = event {
            messages.push(message.clone());
        }
    }
    messages
}

/// Waits, off the test thread, until a call reaches `gate`, letting the model's tasks run
/// meanwhile.
pub async fn arrived(cx: &mut TestAppContext, gate: &Arc<CallGate>) {
    let gate = Arc::clone(gate);
    let came = blocking(&cx.executor(), move || gate.wait_until_arrived()).await;
    assert!(came, "the call never reached its gate");
}

/// Waits, off the test thread, until `ready` holds, letting the model's tasks run meanwhile.
pub async fn until(cx: &mut TestAppContext, ready: impl Fn() -> bool + Send + 'static) {
    let held = blocking(&cx.executor(), move || {
        let start = Instant::now();
        while !ready() {
            if start.elapsed() > Duration::from_secs(20) {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
        true
    })
    .await;
    assert!(held, "the condition never held");
}

/// The model's file state.
pub fn file_state(cx: &mut TestAppContext, model: &Entity<BrowserModel>) -> FileState {
    model.read_with(cx, |model, _| model.file_state().clone())
}

/// The entry and document of a text file state; `None` for any other state.
pub fn text(state: &FileState) -> Option<(FileEntry, Arc<TextDocument>)> {
    let FileState::Text(entry, document) = state else {
        return None;
    };
    Some((entry.clone(), Arc::clone(document)))
}

/// A shared gate.
pub fn gate() -> Arc<CallGate> {
    Arc::new(CallGate::new())
}
