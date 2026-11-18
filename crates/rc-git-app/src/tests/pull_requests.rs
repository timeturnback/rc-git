//! The Pull Requests section and page against the fake service: the section lists what the
//! fake answers, a row opens the pull request's page, a failed read says "couldn't check",
//! and a hostile title and description show as escapes and load nothing.

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use rc_git_kit::fake::{FakePullRequests, oid, page, pull_request, pull_request_detail};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::{CheckList, ReadError, multi_line, single_line};

use crate::files::markdown::MarkdownPreview;
use crate::tests::support::{Harness, harness_reading, open_settled, repo, wait_until};
use crate::window::RepoWindow;

const PATH: &str = "/repo/a";

fn shell(cx: &mut TestAppContext, fake: &FakePullRequests) -> (Harness, Entity<RepoWindow>) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_reading(cx, &[(PATH, repo())], &settings, Arc::new(fake.clone()));
    let window = open_settled(&harness, cx, PATH);
    (harness, window)
}

/// Waits until the window's list read has answered.
fn listed(cx: &mut TestAppContext, window: &Entity<RepoWindow>) {
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let model = window.read(cx).pull_requests().read(cx);
            model.link().is_some()
                && !model.list().is_loading()
                && !matches!(
                    model.list().freshness(),
                    rc_git_kit::pull_requests::state::Freshness::Loading
                )
        })
    });
}

fn section_rows(cx: &TestAppContext, window: &Entity<RepoWindow>) -> Vec<String> {
    cx.read(|cx| {
        let section = window.read(cx).sidebar().read(cx).pull_requests().clone();
        section.read(cx).row_titles(cx)
    })
}

fn section_notes(cx: &TestAppContext, window: &Entity<RepoWindow>) -> Vec<String> {
    cx.read(|cx| {
        let section = window.read(cx).sidebar().read(cx).pull_requests().clone();
        section.read(cx).notes(cx)
    })
}

#[gpui::test]
fn the_section_lists_the_open_pull_requests(cx: &mut TestAppContext) {
    let fake = FakePullRequests::new();
    fake.state().pages.insert(
        None,
        Ok(page(
            vec![
                pull_request(7, "Add the thing", oid(7)),
                pull_request(3, "Fix it", oid(3)),
            ],
            None,
        )),
    );
    let (_harness, window) = shell(cx, &fake);
    listed(cx, &window);
    assert_eq!(section_rows(cx, &window), ["Add the thing", "Fix it"]);
    assert_eq!(fake.state().list_calls, [None], "one page, read once");
}

#[gpui::test]
fn a_hidden_link_reads_no_pull_request(cx: &mut TestAppContext) {
    let fake = FakePullRequests::with_link(RepositoryLink::Hidden);
    let (_harness, window) = shell(cx, &fake);
    wait_until(cx, |cx| {
        cx.read(|cx| window.read(cx).pull_requests().read(cx).link().is_some())
    });
    assert!(section_rows(cx, &window).is_empty());
    assert!(fake.state().list_calls.is_empty());
}

#[gpui::test]
fn selecting_a_row_opens_its_page_and_a_mode_switch_hides_it(cx: &mut TestAppContext) {
    let fake = FakePullRequests::new();
    fake.state().pages.insert(
        None,
        Ok(page(vec![pull_request(7, "Add the thing", oid(7))], None)),
    );
    let detail = pull_request_detail(
        pull_request(7, "Add the thing", oid(7)),
        "",
        CheckList::default(),
    );
    fake.state().details.insert(7, Ok(detail));
    let (_harness, window) = shell(cx, &fake);
    listed(cx, &window);
    window.update(cx, |window, cx| window.open_pull_request(7, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let model = window.read(cx).pull_requests().read(cx);
            model
                .selected_tab()
                .is_some_and(|tab| tab.detail().is_some())
        })
    });
    assert!(cx.read(|cx| window.read(cx).shows_pull_request(cx)));
    assert_eq!(fake.state().detail_calls, [7]);
    window.update(cx, |window, cx| {
        window.switch_mode(rc_git_kit::back_forward::ContentMode::History, cx);
    });
    assert!(!cx.read(|cx| window.read(cx).shows_pull_request(cx)));
    let tabs = cx.read(|cx| window.read(cx).pull_requests().read(cx).tabs().len());
    assert_eq!(tabs, 1, "the tab stays open behind the pane");
}

#[gpui::test]
fn a_failed_read_says_couldnt_check_with_its_cause(cx: &mut TestAppContext) {
    let fake = FakePullRequests::new();
    fake.state().pages.insert(
        None,
        Err(ReadError::Failed("connection refused".to_owned())),
    );
    let (_harness, window) = shell(cx, &fake);
    listed(cx, &window);
    assert_eq!(
        section_notes(cx, &window),
        ["Couldn't check: connection refused"]
    );
    assert!(section_rows(cx, &window).is_empty());
}

/// Waits until the page's description preview is parsed.
fn description(cx: &mut TestAppContext, window: &Entity<RepoWindow>) -> Entity<MarkdownPreview> {
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let page = window.read(cx).pull_request_page().read(cx);
            page.preview()
                .is_some_and(|preview| preview.read(cx).rendered().is_some())
        })
    });
    let preview = cx.read(|cx| {
        window
            .read(cx)
            .pull_request_page()
            .read(cx)
            .preview()
            .cloned()
    });
    preview.unwrap_or_else(|| panic!("the description shows"))
}

#[gpui::test]
fn a_hostile_title_and_description_show_escapes_and_load_nothing(cx: &mut TestAppContext) {
    let title = single_line("evil\u{202e}gpj.exe\n\u{1b}[31mred", 1024);
    let body = multi_line(
        "![pixel](https://tracker.example/p.png)\n\n[run](javascript:alert(1)) \
         [mail](mailto:a@b.example) [file](file:///etc/passwd) \
         [ok](https://github.com/o/r)\u{202e}",
        65_536,
    );
    let fake = FakePullRequests::new();
    fake.state()
        .pages
        .insert(None, Ok(page(vec![pull_request(9, &title, oid(9))], None)));
    let detail = pull_request_detail(pull_request(9, &title, oid(9)), &body, CheckList::default());
    fake.state().details.insert(9, Ok(detail));
    let (harness, window) = shell(cx, &fake);
    listed(cx, &window);
    let shown = section_rows(cx, &window);
    assert_eq!(shown, ["evil\\u{202e}gpj.exe\\u{a}red"]);
    assert!(!shown[0].contains('\u{202e}') && !shown[0].contains('\u{1b}'));
    window.update(cx, |window, cx| window.open_pull_request(9, cx));
    let preview = description(cx, &window);
    assert_eq!(cx.opened_url(), None, "showing the page loads nothing");
    let tab = harness
        .shell
        .read_with(cx, |shell, _| shell.windows()[0].clone());
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    for refused in [
        "javascript:alert(1)",
        "mailto:a@b.example",
        "file:///etc/passwd",
        "/o/r",
    ] {
        visual.update(|window_ref, cx| {
            preview.update(cx, |preview, cx| preview.follow(refused, window_ref, cx));
        });
        assert_eq!(visual.opened_url(), None, "{refused} opens nothing");
        let copied = visual.read(|cx| preview.read(cx).link_copy_text(refused));
        assert_eq!(copied, None, "{refused}");
    }
    visual.update(|window_ref, cx| {
        preview.update(cx, |preview, cx| {
            preview.follow("https://tracker.example/p.png", window_ref, cx);
        });
    });
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("https://tracker.example/p.png"),
        "an image is a link that opens only on a click"
    );
}
