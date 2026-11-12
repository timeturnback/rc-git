//! The markdown preview: what a document builds into, and the pane's wiring of the preview,
//! its links and its `#fragment` reveal against the fakes.

use gpui::{Entity, TestAppContext, VisualTestContext};
use rc_git_core::{EntryKind, TreeEntry};
use rc_git_kit::browser::FileState;
use rc_git_kit::fake::{FakeRepository, oid, text_blob};
use rc_git_kit::markdown::{MarkdownLink, MarkdownRun, MarkdownStyle};

use crate::files::markdown::{Look, MarkdownPreview, Rendered, inline, marker_text};
use crate::tests::support::{MAIN_TREE, README, SRC_TREE, harness, open_settled, repo, wait_until};
use crate::window::RepoWindow;

/// The README the preview tests open: a link to a file, a link to a later heading, rust code,
/// and enough prose that the heading starts off screen.
pub fn guide() -> String {
    let mut text = String::from(
        "# Guide\n\nSee [main](src/main.rs) and [later](#later-part).\n\n\
         ```rust\nfn main() { let x = \"hi\"; }\n```\n\n",
    );
    for _ in 0..60 {
        text.push_str("Paragraph of filler.\n\n");
    }
    text.push_str("## Later part\n\n");
    for _ in 0..60 {
        text.push_str("Closing paragraph.\n\n");
    }
    text
}

const GUIDE: u8 = 0x41;

/// The standard fake with `README.md` holding [`guide`].
pub fn markdown_repo() -> FakeRepository {
    let repo = repo();
    {
        let mut state = repo.state();
        state.blobs.insert(oid(GUIDE), text_blob(&guide()));
        state.blobs.insert(oid(README), text_blob("fn main() {}\n"));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                TreeEntry {
                    name: "src".into(),
                    oid: oid(SRC_TREE),
                    kind: EntryKind::Tree,
                },
                TreeEntry {
                    name: "README.md".into(),
                    oid: oid(GUIDE),
                    kind: EntryKind::Blob,
                },
            ],
        );
    }
    repo
}

fn look() -> Look {
    let black = gpui::black();
    Look {
        font: gpui::font("Helvetica"),
        mono: gpui::font("Menlo"),
        color: black,
        link: gpui::blue(),
        code_background: black.opacity(0.05),
    }
}

#[test]
fn a_document_builds_blocks_anchors_and_colored_fences() {
    let rendered = Rendered::build(&guide());
    let later = rendered.anchor_block("later-part");
    assert!(later.is_some_and(|index| index > 60), "{later:?}");
    assert_eq!(
        rendered.anchor_block("Later Part"),
        later,
        "slugged like a heading"
    );
    assert_eq!(rendered.anchor_block("guide"), Some(0));
    assert_eq!(rendered.anchor_block("missing"), None);
    let code = rendered.fences.get(2).cloned().flatten();
    assert!(
        code.is_some_and(|spans| !spans.is_empty()),
        "a rust fence is colored"
    );
    assert!(rendered.fences.first().cloned().flatten().is_none());
}

#[test]
fn repeated_headings_and_footnotes_take_their_own_anchors() {
    let rendered = Rendered::build("# Fixed\n\n# Fixed\n\nA[^n].\n\n[^n]: note\n");
    assert_eq!(rendered.anchor_block("fixed"), Some(0));
    assert_eq!(rendered.anchor_block("fixed-1"), Some(1));
    assert_eq!(rendered.anchor_block("fn-n"), Some(3));
}

#[test]
fn a_document_with_no_block_shows_its_source() {
    let rendered = Rendered::build("\n\n");
    assert!(rendered.blocks.len() <= 1);
    assert_eq!(Rendered::build("").blocks.len(), 0);
}

#[test]
fn inline_runs_carry_their_links_and_styles() {
    let link = MarkdownRun {
        link: Some(MarkdownLink {
            destination: "src/main.rs".to_owned(),
            title: Some("Main".to_owned()),
        }),
        ..MarkdownRun::plain("main")
    };
    let strong = MarkdownRun {
        style: MarkdownStyle::STRONG,
        ..MarkdownRun::plain("bold")
    };
    let image = MarkdownRun {
        is_image: true,
        link: Some(MarkdownLink::new("logo.png")),
        ..MarkdownRun::plain("logo")
    };
    let built = inline(
        &[
            MarkdownRun::plain("See "),
            link,
            strong,
            image,
            MarkdownRun::plain(""),
        ],
        &look(),
    );
    assert!(built.text.starts_with("See mainbold"));
    assert!(built.text.ends_with(" logo"));
    assert_eq!(built.runs.len(), 4, "an empty run adds nothing");
    let lengths: usize = built.runs.iter().map(|run| run.len).sum();
    assert_eq!(lengths, built.text.len(), "the runs cover the text");
    assert_eq!(built.links.len(), 2);
    assert_eq!(built.links[0].range, 4..8);
    assert_eq!(built.links[0].tooltip, "Main\nsrc/main.rs");
    assert_eq!(built.links[1].destination, "logo.png");
    assert!(built.runs[1].underline.is_some());
    assert_eq!(built.runs[2].font.weight, gpui::FontWeight::BOLD);
}

#[test]
fn list_markers_read_as_swift_draws_them() {
    use rc_git_kit::markdown::MarkdownListMarker;
    assert_eq!(marker_text(MarkdownListMarker::Bullet), "\u{2022}");
    assert_eq!(marker_text(MarkdownListMarker::Ordered(3)), "3.");
    assert_eq!(
        marker_text(MarkdownListMarker::Task { done: true }),
        "\u{2611}"
    );
}

/// Opens the guide and waits until its preview is built.
fn open_guide(
    cx: &mut TestAppContext,
    window: &Entity<RepoWindow>,
    fragment: Option<&str>,
) -> Entity<MarkdownPreview> {
    let model = cx.read(|cx| window.read(cx).model().clone());
    let fragment = fragment.map(str::to_owned);
    model
        .update(cx, |model, cx| model.open_path("README.md", fragment, cx))
        .detach();
    let pane = cx.read(|cx| window.read(cx).pane().clone());
    wait_until(cx, |cx| {
        cx.read(|cx| {
            pane.read(cx)
                .preview()
                .is_some_and(|preview| preview.read(cx).rendered().is_some())
        })
    });
    let preview = cx.read(|cx| pane.read(cx).preview().cloned());
    preview.unwrap_or_else(|| panic!("README.md shows as the preview"))
}

#[gpui::test]
fn markdown_opens_as_the_preview_and_a_fragment_link_scrolls_it(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", markdown_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let preview = open_guide(cx, &window, None);
    assert_eq!(cx.read(|cx| preview.read(cx).top_block()), 0);
    let tab = harness
        .shell
        .read_with(cx, |shell, _| shell.windows()[0].clone());
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        preview.update(cx, |preview, cx| {
            preview.follow("#later-part", window_ref, cx);
        });
    });
    let later = cx.read(|cx| {
        preview
            .read(cx)
            .rendered()
            .and_then(|rendered| rendered.anchor_block("later-part"))
    });
    assert_eq!(cx.read(|cx| Some(preview.read(cx).top_block())), later);
}

#[gpui::test]
fn a_repository_link_with_a_fragment_scrolls_once_built(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", markdown_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let preview = open_guide(cx, &window, Some("later-part"));
    wait_until(cx, |cx| cx.read(|cx| preview.read(cx).top_block() > 0));
    let pending = cx.read(|cx| window.read(cx).model().read(cx).pending_reveal().cloned());
    assert_eq!(pending, None, "the pane took the reveal");
}

#[gpui::test]
fn a_repository_link_opens_the_file_it_names(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", markdown_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let preview = open_guide(cx, &window, None);
    let tab = harness
        .shell
        .read_with(cx, |shell, _| shell.windows()[0].clone());
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        preview.update(cx, |preview, cx| {
            preview.follow("src/main.rs", window_ref, cx);
        });
    });
    let model = cx.read(|cx| window.read(cx).model().clone());
    wait_until(cx, |cx| {
        cx.read(|cx| {
            model.read(cx).opened_file_path() == Some("src/main.rs")
                && matches!(model.read(cx).file_state(), FileState::Text(..))
        })
    });
    let pane = cx.read(|cx| window.read(cx).pane().clone());
    assert!(
        cx.read(|cx| pane.read(cx).preview().is_none()),
        "rust shows as source"
    );
    assert!(cx.read(|cx| pane.read(cx).viewer().is_some()));
}

#[gpui::test]
fn copy_link_resolves_against_the_file_on_screen(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", markdown_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let preview = open_guide(cx, &window, None);
    let copied = cx.read(|cx| preview.read(cx).link_copy_text("src/../src/main.rs#top"));
    assert_eq!(copied.as_deref(), Some("src/main.rs#top"));
    let none = cx.read(|cx| preview.read(cx).link_copy_text("../../outside.md"));
    assert_eq!(none, None);
}

#[gpui::test]
fn the_source_picker_switches_and_holds_for_the_next_file(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", markdown_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    open_guide(cx, &window, None);
    let pane = cx.read(|cx| window.read(cx).pane().clone());
    let tab = harness
        .shell
        .read_with(cx, |shell, _| shell.windows()[0].clone());
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_prefers_preview(false, window_ref, cx);
        });
    });
    assert!(cx.read(|cx| pane.read(cx).preview().is_none()));
    assert!(
        cx.read(|cx| pane.read(cx).viewer().is_some()),
        "the source shows"
    );
    visual.update(|window_ref, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_prefers_preview(true, window_ref, cx);
        });
    });
    assert!(cx.read(|cx| pane.read(cx).preview().is_some()));
    assert!(cx.read(|cx| pane.read(cx).viewer().is_none()));
}
