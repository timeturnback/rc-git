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

/// The page's view and the selected tab's changes phase.
fn changes_phase(
    cx: &TestAppContext,
    window: &Entity<RepoWindow>,
) -> Option<rc_git_kit::pull_requests::changes::ChangesPhase> {
    cx.read(|cx| {
        let model = window.read(cx).pull_requests().read(cx);
        model
            .selected_changes()
            .map(|changes| changes.phase().clone())
    })
}

#[gpui::test]
fn the_changes_view_reads_local_objects_and_fetches_only_on_its_button(cx: &mut TestAppContext) {
    use rc_git_core::{BlobDiff, ChangeKind, DiffHunk, EntryKind, TreeChange};
    use rc_git_kit::pull_requests::changes::{ChangesPhase, ChangesRead, HeadFetched};

    use crate::diff::DiffContent;
    use crate::pull_requests::page::PageView;

    let fake = FakePullRequests::new();
    fake.state()
        .pages
        .insert(None, Ok(page(vec![pull_request(7, "Ship", oid(7))], None)));
    let detail = pull_request_detail(pull_request(7, "Ship", oid(7)), "", CheckList::default());
    fake.state().details.insert(7, Ok(detail));
    let change = TreeChange {
        kind: ChangeKind::Modified,
        path: "src/lib.rs".into(),
        source_path: None,
        old: Some(oid(1)),
        new: Some(oid(2)),
        entry: EntryKind::Blob,
        old_entry: Some(EntryKind::Blob),
    };
    let ready = ChangesRead::Files {
        base: oid(3),
        compared: rc_git_kit::pull_requests::changes::ComparedWith::PullRequestBase(oid(2)),
        files: vec![change],
    };
    fake.state()
        .changes
        .insert(oid(7), [Ok(ChangesRead::HeadMissing), Ok(ready)].into());
    fake.state()
        .fetches
        .insert(oid(7), Ok(HeadFetched::Fetched));
    let hunk = DiffHunk {
        old_start: 1,
        old_lines: 0,
        new_start: 1,
        new_lines: 0,
        lines: Vec::new(),
    };
    fake.state().file_diffs.insert(
        "src/lib.rs".to_owned(),
        Ok(BlobDiff::Text { hunks: vec![hunk] }),
    );
    let (_harness, window) = shell(cx, &fake);
    listed(cx, &window);
    window.update(cx, |window, cx| window.open_pull_request(7, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let model = window.read(cx).pull_requests().read(cx);
            model
                .selected_tab()
                .is_some_and(|tab| tab.stamp().is_some())
        })
    });
    assert_eq!(
        changes_phase(cx, &window),
        None,
        "Overview reads no changes"
    );
    let page = cx.read(|cx| window.read(cx).pull_request_page().clone());
    page.update(cx, |page, cx| page.set_view(PageView::Changes, cx));
    assert_eq!(cx.read(|cx| page.read(cx).view()), PageView::Changes);
    wait_until(cx, |cx| {
        changes_phase(cx, &window) == Some(ChangesPhase::NeedsFetch { problem: None })
    });
    assert!(
        fake.state().fetch_calls.is_empty(),
        "showing changes fetches nothing"
    );

    let model = cx.read(|cx| window.read(cx).pull_requests().clone());
    model.update(
        cx,
        rc_git_kit::pull_requests::model::PullRequestsModel::fetch_changes,
    );
    wait_until(cx, |cx| {
        matches!(changes_phase(cx, &window), Some(ChangesPhase::Ready { .. }))
    });
    assert_eq!(fake.state().fetch_calls.len(), 1);
    model.update(cx, |model, cx| model.select_changed_file(0, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let diff = page.read(cx).changes_diff().read(cx);
            matches!(diff.content(), DiffContent::Hunks(hunks) if hunks.len() == 1)
        })
    });
}

/// The challenge of pull request 7's merge, once the window shows it.
fn merge_sheet(
    cx: &mut TestAppContext,
    window: &Entity<RepoWindow>,
) -> Entity<crate::sheets::challenge::ChallengeSheet> {
    use crate::sheets::OpenSheet;
    wait_until(cx, |cx| {
        cx.read(|cx| {
            matches!(
                window.read(cx).sheet(),
                Some(OpenSheet::Merge { number: 7, .. })
            )
        })
    });
    cx.read(|cx| match window.read(cx).sheet() {
        Some(OpenSheet::Merge { sheet, .. }) => sheet.upgrade(),
        Some(_) | None => None,
    })
    .unwrap_or_else(|| panic!("the merge challenge is on screen"))
}

#[gpui::test]
fn a_merge_runs_only_from_its_challenge_and_once(cx: &mut TestAppContext) {
    use rc_git_kit::fake::merge_plan;
    use rc_git_kit::pull_requests::merge::{MergeButton, MergeEnd, MergePhase, MergeStop};
    use rc_git_kit::pull_requests::{MergeMethod, MergeOutcome, Settled};

    let fake = FakePullRequests::new();
    fake.state()
        .pages
        .insert(None, Ok(page(vec![pull_request(7, "Ship", oid(7))], None)));
    let detail = pull_request_detail(pull_request(7, "Ship", oid(7)), "", CheckList::default());
    fake.state().details.insert(7, Ok(detail.clone()));
    fake.state().plan = Ok(merge_plan(&detail, MergeMethod::Squash));
    fake.state().merge = Ok(MergeOutcome::Merged {
        merge_commit: None,
        base: Some("main".to_owned()),
        settled: Settled::Answer,
    });
    let (harness, window) = shell(cx, &fake);
    listed(cx, &window);
    window.update(cx, |window, cx| window.open_pull_request(7, cx));
    let model = cx.read(|cx| window.read(cx).pull_requests().clone());
    wait_until(cx, |cx| {
        model.read_with(cx, |model, _| {
            matches!(model.merge_button(7), MergeButton::Enabled { .. })
        })
    });
    let handle = harness.shell.read_with(cx, |shell, _| {
        shell
            .windows()
            .first()
            .map_or_else(|| panic!("a window"), |tab| tab.handle)
    });
    let phase = |cx: &mut TestAppContext| {
        model.read_with(cx, |model, _| {
            model.merge_flow(7).map(|flow| flow.phase().clone())
        })
    };

    model.update(cx, |model, cx| model.start_merge(MergeMethod::Squash, cx));
    let sheet = merge_sheet(cx, &window);
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    sheet.update_in(
        &mut visual,
        crate::sheets::challenge::ChallengeSheet::cancel,
    );
    cx.run_until_parked();
    assert_eq!(
        phase(cx),
        Some(MergePhase::Done(MergeEnd::NotSent(MergeStop::Cancelled))),
        "a cancelled challenge ends the merge"
    );
    assert!(fake.state().merge_calls.is_empty());

    model.update(cx, |model, cx| {
        model.dismiss_flow(7, cx);
        model.start_merge(MergeMethod::Squash, cx);
    });
    let sheet = merge_sheet(cx, &window);
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        assert!(sheet.can_confirm(cx), "a plain confirm");
        sheet.confirm(window, cx);
        sheet.confirm(window, cx);
    });
    wait_until(cx, |cx| {
        matches!(phase(cx), Some(MergePhase::Done(MergeEnd::Merged { .. })))
    });
    let state = fake.state();
    assert_eq!((state.merge_calls.len(), state.merges_sent), (1, 1));
    assert_eq!(state.merge_calls[0].stamp, detail.stamp());
}

#[gpui::test]
fn an_admin_merge_runs_only_from_its_own_challenge_with_the_number_typed(cx: &mut TestAppContext) {
    use rc_git_kit::fake::{admin_merge_plan, blocked_detail};
    use rc_git_kit::pull_requests::MergeKind;
    use rc_git_kit::pull_requests::merge::{AdminMergeButton, MergeButton, MergeEnd, MergePhase};
    use rc_git_kit::pull_requests::{MergeMethod, MergeOutcome, Settled};

    let fake = FakePullRequests::new();
    fake.state()
        .pages
        .insert(None, Ok(page(vec![pull_request(7, "Ship", oid(7))], None)));
    let detail = blocked_detail(pull_request_detail(
        pull_request(7, "Ship", oid(7)),
        "",
        CheckList::default(),
    ));
    fake.state().details.insert(7, Ok(detail.clone()));
    fake.state().plan = Ok(admin_merge_plan(&detail, MergeMethod::Squash));
    fake.state().merge = Ok(MergeOutcome::Merged {
        merge_commit: None,
        base: Some("main".to_owned()),
        settled: Settled::Answer,
    });
    let (harness, window) = shell(cx, &fake);
    listed(cx, &window);
    window.update(cx, |window, cx| window.open_pull_request(7, cx));
    let model = cx.read(|cx| window.read(cx).pull_requests().clone());
    wait_until(cx, |cx| {
        model.read_with(cx, |model, _| {
            matches!(model.admin_merge_button(7), AdminMergeButton::Shown { .. })
        })
    });
    assert!(model.read_with(cx, |model, _| matches!(
        model.merge_button(7),
        MergeButton::Disabled { .. }
    )));
    let handle = harness.shell.read_with(cx, |shell, _| {
        shell
            .windows()
            .first()
            .map_or_else(|| panic!("a window"), |tab| tab.handle)
    });

    model.update(cx, |model, cx| {
        model.start_admin_merge(MergeMethod::Squash, cx);
    });
    let sheet = merge_sheet(cx, &window);
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        assert!(!sheet.can_confirm(cx), "nothing typed yet");
        sheet.confirm(window, cx);
        sheet.type_phrase("#7", window, cx);
        assert!(!sheet.can_confirm(cx), "#7 is not the number");
        sheet.confirm(window, cx);
    });
    cx.run_until_parked();
    assert!(
        fake.state().merge_calls.is_empty(),
        "no merge without the number"
    );
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.type_phrase(" 7 ", window, cx);
        assert!(sheet.can_confirm(cx));
        sheet.confirm(window, cx);
        sheet.confirm(window, cx);
    });
    wait_until(cx, |cx| {
        model.read_with(cx, |model, _| {
            matches!(
                model.merge_flow(7).map(|flow| flow.phase().clone()),
                Some(MergePhase::Done(MergeEnd::Merged { .. }))
            )
        })
    });
    let state = fake.state();
    assert_eq!((state.merge_calls.len(), state.merges_sent), (1, 1));
    assert_eq!(state.merge_calls[0].kind, MergeKind::Admin);
    assert_eq!(state.merge_calls[0].typed, " 7 ", "handed on as typed");
}

#[gpui::test]
fn auto_merge_and_a_rerun_run_only_from_their_challenges_and_once(cx: &mut TestAppContext) {
    use rc_git_kit::fake::{auto_merge_plan, auto_mergeable_detail, failing_detail, rerun_plan};
    use rc_git_kit::pull_requests::auto_merge::AutoMergeControl;
    use rc_git_kit::pull_requests::flow::TabFlow;
    use rc_git_kit::pull_requests::rerun::RerunControl;
    use rc_git_kit::pull_requests::{
        AutoMergeChange, AutoMergeOutcome, MergeMethod, RerunReport, RunId, RunOutcome, RunReport,
        Settled,
    };

    let fake = FakePullRequests::new();
    fake.state()
        .pages
        .insert(None, Ok(page(vec![pull_request(7, "Ship", oid(7))], None)));
    let detail = failing_detail(
        auto_mergeable_detail(pull_request_detail(
            pull_request(7, "Ship", oid(7)),
            "",
            CheckList::default(),
        )),
        &[41],
    );
    let change = AutoMergeChange::Enable(MergeMethod::Squash);
    fake.state().details.insert(7, Ok(detail.clone()));
    fake.state().action_plan = Ok(auto_merge_plan(&detail, change));
    fake.state().auto_merge = Ok(AutoMergeOutcome::Done {
        settled: Settled::Answer,
    });
    fake.state().rerun = Ok(RerunReport {
        runs: vec![RunReport {
            run: RunId::new(41).unwrap(),
            outcome: RunOutcome::Requested,
        }],
    });
    let (harness, window) = shell(cx, &fake);
    listed(cx, &window);
    window.update(cx, |window, cx| window.open_pull_request(7, cx));
    let model = cx.read(|cx| window.read(cx).pull_requests().clone());
    wait_until(cx, |cx| {
        model.read_with(cx, |model, _| {
            matches!(model.auto_merge_control(7), AutoMergeControl::Offer { .. })
                && matches!(model.rerun_control(7), RerunControl::Shown { runs: 1, .. })
        })
    });
    let handle = harness.shell.read_with(cx, |shell, _| {
        shell
            .windows()
            .first()
            .map_or_else(|| panic!("a window"), |tab| tab.handle)
    });
    let done = |cx: &mut TestAppContext| {
        model.read_with(cx, |model, _| {
            model.tab_flow(7).is_some_and(|flow| !flow.is_running())
        })
    };

    model.update(cx, |model, cx| {
        model.start_enable_auto_merge(MergeMethod::Squash, cx);
    });
    let sheet = merge_sheet(cx, &window);
    assert!(
        fake.state().action_calls.is_empty(),
        "nothing before the confirm"
    );
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.confirm(window, cx);
        sheet.confirm(window, cx);
    });
    wait_until(cx, done);
    assert_eq!(fake.state().actions_sent, 1);
    assert_eq!(fake.state().action_calls[0].change, Some(change));

    fake.state().action_plan = Ok(rerun_plan(&detail));
    model.update(cx, |model, cx| {
        model.dismiss_flow(7, cx);
        model.start_rerun(cx);
    });
    let sheet = merge_sheet(cx, &window);
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    sheet.update_in(
        &mut visual,
        crate::sheets::challenge::ChallengeSheet::confirm,
    );
    wait_until(cx, done);
    let state = fake.state();
    assert_eq!((state.action_calls.len(), state.actions_sent), (2, 2));
    assert_eq!(state.action_calls[1].change, None);
    drop(state);
    assert!(model.read_with(cx, |model, _| matches!(
        model.tab_flow(7),
        Some(TabFlow::Rerun(_))
    )));
}

#[test]
fn a_changed_file_s_name_reaches_the_diff_pane_only_neutralized() {
    use rc_git_core::{BlobDiff, ChangeKind, EntryKind, TreeChange};
    use rc_git_kit::models::FileDiffState;

    use crate::diff::DiffContent;
    use crate::pull_requests::changes::diff_content;

    let change = TreeChange {
        kind: ChangeKind::Modified,
        path: "evil\u{202e}gnp.exe\u{9b}".into(),
        source_path: None,
        old: Some(oid(1)),
        new: Some(oid(2)),
        entry: EntryKind::Blob,
        old_entry: Some(EntryKind::Blob),
    };
    let binary = BlobDiff::Binary {
        old_size: 1,
        new_size: 2,
    };
    for state in [
        FileDiffState::Loaded(change.clone(), binary),
        FileDiffState::Failed(change.clone(), "evil\u{202e}gnp.exe: gone".to_owned()),
    ] {
        let DiffContent::Placeholder(line) = diff_content(&state) else {
            panic!("{state:?}");
        };
        assert!(line.contains("evil\\u{202e}gnp.exe"), "{line}");
        assert!(
            !line.contains('\u{202e}') && !line.contains('\u{9b}'),
            "{line}"
        );
    }
}

#[gpui::test]
fn a_check_s_log_shows_in_the_text_view_only_from_its_click(cx: &mut TestAppContext) {
    use rc_git_kit::fake::failing_detail;
    use rc_git_kit::pull_requests::{JobId, JobLog, LogCut, RunId};

    let detail = failing_detail(
        pull_request_detail(pull_request(7, "Ship", oid(7)), "", CheckList::default()),
        &[41],
    );
    let fake = FakePullRequests::new();
    fake.state()
        .pages
        .insert(None, Ok(page(vec![detail.summary.clone()], None)));
    fake.state().details.insert(7, Ok(detail.clone()));
    let job = JobId::new(1041).unwrap();
    fake.state().log = Ok(JobLog {
        job,
        run: RunId::new(41).unwrap(),
        head: oid(7),
        name: "job 0".to_owned(),
        text: "error: https://evil.example/x \\u{202e}\nexit 1".to_owned(),
        lines: 2,
        cut: LogCut::Whole,
        lines_left_out: 0,
        lines_shortened: 0,
    });
    let (_harness, window) = shell(cx, &fake);
    listed(cx, &window);
    window.update(cx, |window, cx| window.open_pull_request(7, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let model = window.read(cx).pull_requests().read(cx);
            model.log_check(7, &detail.checks.checks[0]).is_some()
        })
    });
    assert!(
        fake.state().log_calls.is_empty(),
        "showing the page reads no log"
    );
    let model = cx.read(|cx| window.read(cx).pull_requests().clone());
    model.update(cx, |model, cx| model.view_log(job, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            window
                .read(cx)
                .pull_request_page()
                .read(cx)
                .log_viewer()
                .is_some()
        })
    });
    let text = cx.read(|cx| {
        let page = window.read(cx).pull_request_page().read(cx);
        let viewer = page.log_viewer().cloned().unwrap();
        viewer.read(cx).document().text.clone()
    });
    assert_eq!(text, "error: https://evil.example/x \\u{202e}\nexit 1");
    assert_eq!(fake.state().log_calls.len(), 1);
    assert_eq!(cx.opened_url(), None, "a log opens nothing");
    model.update(cx, |model, cx| model.close_log(7, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            window
                .read(cx)
                .pull_request_page()
                .read(cx)
                .log_viewer()
                .is_none()
        })
    });
}
