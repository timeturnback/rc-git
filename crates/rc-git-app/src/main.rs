//! rc-git on GPUI. While the migration runs, the binary opens the file named on the command
//! line in the source viewer; the repository browser replaces this in phase 3.

mod palette;
mod viewer;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, AppContext, KeyBinding, Menu, MenuItem, WindowOptions, actions};
use rc_git_highlight::Language;
use rc_git_kit::text_document::TextDocument;

use crate::viewer::FileViewer;

actions!(
    rc_git,
    [
        /// Quits the app.
        Quit
    ]
);

/// Shared by every window, so macOS gathers them into one tab group.
const TABBING_IDENTIFIER: &str = "rc-git.repository";

fn open_file(path: &std::path::Path, cx: &mut App) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    let starts = line_starts(&bytes);
    let valid = std::str::from_utf8(&bytes).is_ok();
    let document = Arc::new(TextDocument::new(bytes, &starts, valid));
    let language = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(Language::from_file_name);
    let options = WindowOptions {
        tabbing_identifier: Some(TABBING_IDENTIFIER.into()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        let viewer = cx.new(|cx| {
            if let Some(language) = language {
                let source = Arc::clone(&document);
                cx.spawn(async move |this, cx| {
                    let spans = cx
                        .background_spawn(async move {
                            rc_git_highlight::highlight(source.text.as_bytes(), language, None)
                        })
                        .await;
                    if let Ok(spans) = spans {
                        this.update(cx, |viewer: &mut FileViewer, cx| {
                            viewer.set_spans(Arc::new(spans), cx);
                        })
                        .ok();
                    }
                })
                .detach();
            }
            FileViewer::new(document)
        });
        cx.new(|cx| gpui_component::Root::new(viewer, window, cx))
    })?;
    Ok(())
}

/// Byte offsets where lines start, by the core's rule: none for empty input, and a trailing
/// newline does not start a line.
fn line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    if bytes.is_empty() {
        return starts;
    }
    starts.push(0);
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && index + 1 < bytes.len() {
            starts.push(index + 1);
        }
    }
    starts
}

#[expect(
    clippy::print_stderr,
    reason = "no window exists to show the failure in; the phase 3 browser reports it in-window"
)]
fn report_open_failure(path: &std::path::Path, error: &anyhow::Error) {
    eprintln!("cannot open {}: {error:#}", path.display());
}

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus([Menu::new("RCGit").items([MenuItem::action("Quit RCGit", Quit)])]);
        cx.activate(true);
        if let Some(path) = path
            && let Err(error) = open_file(&path, cx)
        {
            report_open_failure(&path, &error);
        }
    });
}

#[cfg(test)]
mod tests;
