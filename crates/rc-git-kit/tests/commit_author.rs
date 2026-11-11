//! Ported from the `CommitAuthorModelTests` suite of `CommitIdentityTests.swift`: the author
//! line is decided from the repository's config, read through the guard actions, and the
//! identity saved for the repository. The rest of that file is in `commit_identity.rs`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_guard::actions::{ActionError, Identity};
use rc_git_kit::changes::{AuthorChanged, CommitAuthorModel};
use rc_git_kit::commit_identity::{CommitAuthor, CommitIdentity};
use rc_git_kit::fake::{ActionKind, CallGate, FakeActions};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::CommitIdentityStore;

fn identity(name: &str, email: &str) -> CommitIdentity {
    CommitIdentity {
        name: name.to_owned(),
        email: email.to_owned(),
    }
}

fn configured(name: &str, email: &str) -> Identity {
    Identity {
        name: name.to_owned(),
        email: email.to_owned(),
    }
}

fn model(cx: &mut TestAppContext, actions: &FakeActions) -> Entity<CommitAuthorModel> {
    cx.executor().allow_parking();
    let store = CommitIdentityStore::new(Arc::new(MemorySettings::new()));
    let actions = Arc::new(actions.clone());
    cx.new(|_| CommitAuthorModel::new("/repo", store, actions))
}

fn author(cx: &mut TestAppContext, model: &Entity<CommitAuthorModel>) -> Option<CommitAuthor> {
    model.read_with(cx, |m, _| m.author().cloned())
}

#[gpui::test]
async fn a_stale_config_read_is_dropped(cx: &mut TestAppContext) {
    let actions = FakeActions::new();
    let gate = Arc::new(CallGate::new());
    actions
        .state()
        .gates
        .insert(ActionKind::ConfiguredIdentity, Arc::clone(&gate));
    let model = model(cx, &actions);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(&model, move |model, _: &AuthorChanged, cx| {
            sink.borrow_mut().push(model.read(cx).author().cloned());
        })
        .detach();
    });
    let slow = model.update(cx, CommitAuthorModel::refresh);
    assert!(gate.wait_until_arrived());
    actions.state().identity = Some(configured("New", "new@example.com"));
    model.update(cx, CommitAuthorModel::refresh).await;
    // The held read answers what the config said before the newer read.
    actions.state().identity = Some(configured("Old", "old@example.com"));
    gate.open();
    slow.await;

    let new = CommitAuthor::Configured(identity("New", "new@example.com"));
    assert_eq!(actions.state().count(ActionKind::ConfiguredIdentity), 2);
    assert_eq!(author(cx, &model), Some(new.clone()));
    assert_eq!(*seen.borrow(), [Some(new)]);
}

#[gpui::test]
async fn saving_and_clearing_re_decide_the_author(cx: &mut TestAppContext) {
    let actions = FakeActions::new();
    let model = model(cx, &actions);
    model.update(cx, CommitAuthorModel::refresh).await;
    assert_eq!(author(cx, &model), Some(CommitAuthor::Unset));
    let old = identity("Old", "old@example.com");
    let save = model.update(cx, |m, cx| m.save(&old, cx)).unwrap();
    save.await;
    assert_eq!(author(cx, &model), Some(CommitAuthor::Saved(old.clone())));
    assert_eq!(model.read_with(cx, |m, _| m.saved()), Some(old));
    let clear = model.update(cx, CommitAuthorModel::clear).unwrap();
    clear.await;
    assert_eq!(author(cx, &model), Some(CommitAuthor::Unset));
    assert_eq!(model.read_with(cx, |m, _| m.saved()), None);
}

#[gpui::test]
async fn an_unreadable_config_says_so(cx: &mut TestAppContext) {
    let actions = FakeActions::new();
    actions.state().fail_next.insert(
        ActionKind::ConfiguredIdentity,
        ActionError::Denied {
            reason: "cannot open /repo".to_owned(),
        },
    );
    let model = model(cx, &actions);
    model.update(cx, CommitAuthorModel::refresh).await;
    assert_eq!(
        author(cx, &model),
        Some(CommitAuthor::Unreadable("cannot open /repo".to_owned()))
    );
}

#[gpui::test]
async fn the_repository_config_wins_over_the_saved_identity(cx: &mut TestAppContext) {
    let actions = FakeActions::new();
    actions.state().identity = Some(configured("Repo", "repo@example.com"));
    let model = model(cx, &actions);
    let saved = identity("Saved", "saved@example.com");
    model.update(cx, |m, cx| m.save(&saved, cx)).unwrap().await;
    assert_eq!(
        author(cx, &model),
        Some(CommitAuthor::Configured(identity(
            "Repo",
            "repo@example.com"
        )))
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.saved()),
        Some(saved),
        "still the fallback"
    );
}
