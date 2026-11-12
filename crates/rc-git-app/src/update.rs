//! The self-update (`UpdateWindowController`, `AppDelegate.showUpdate`/`rebuildNow`): kit's
//! [`UpdateModel`] over rc-git-mac's [`LiveUpdateEnvironment`], and the window that shows it —
//! what is installed, what the update will do, a live build log, and one button. Every string
//! and control state comes from [`UpdateCopy`]; the window decides nothing.
//!
//! It is a window rather than a sheet because the update outlives every repository window:
//! the last step quits the app and launches the new bundle, which reopens the tabs. The model
//! lives in the shell, so closing the window neither stops an update nor loses its log; the
//! next Show Update opens a window over the same model.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, AsyncApp, Bounds, Context, Entity, FocusHandle, Global, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, UniformListScrollHandle,
    Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::FluentBuilder, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Disableable, Root, Sizable, h_flex, v_flex};
use rc_git_kit::app_build::{AppBuild, UpdateMode};
use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::sync::{SyncModel, SyncState};
use rc_git_kit::update::{
    DiagnosticLevel, LogChange, LogMark, UpdateEvent, UpdateModel, UpdateSyncResult, sync_result,
};
use rc_git_kit::update_copy::{UpdateCopy, installed_line};
use rc_git_mac::update::{LiveUpdateEnvironment, SyncRunner, UpdateHooks};

use crate::app::{Services, Shell};
use crate::log;
use crate::sheets;
use crate::sheets::credentials::{self, CredentialSheet};
use crate::sheets::host_key::HostKeySheet;
use crate::style;

/// How often a running pull looks at the update's cancel: the token has no wake-up of its own.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// The running bundle's stamp, read once at launch.
pub struct BuildInfo {
    /// What `scripts/build-app.sh` stamped.
    pub build: AppBuild,
}

impl Global for BuildInfo {}

impl BuildInfo {
    /// The stamp, when one was read.
    #[must_use]
    pub fn build(cx: &App) -> Option<&AppBuild> {
        cx.try_global::<Self>().map(|info| &info.build)
    }

    /// The stamp of a bundle that can update itself, `None` for an unstamped one.
    #[must_use]
    pub fn updatable(cx: &App) -> Option<AppBuild> {
        let info = cx.try_global::<Self>()?;
        info.build
            .update_mode()
            .is_some()
            .then(|| info.build.clone())
    }

    /// What the menus offer: the stamped mode, whether or not it can run here.
    #[must_use]
    pub fn update_mode(cx: &App) -> Option<UpdateMode> {
        Self::build(cx).and_then(AppBuild::update_mode)
    }
}

/// The update window, once open: the credential and host-key sheets of the update's pull go
/// on it.
pub type WindowSlot = Rc<RefCell<Option<WindowHandle<Root>>>>;

/// The update model the shell keeps, and the window showing it.
pub struct Updater {
    build: AppBuild,
    model: Entity<UpdateModel>,
    window: WindowSlot,
    /// The window's view, while it lives.
    view: Rc<RefCell<Option<gpui::WeakEntity<UpdateWindow>>>>,
    _diagnostics: Subscription,
}

impl Updater {
    /// The model over the live environment for `build`, or `None` for a build with no
    /// checkout stamp. Its diagnostics go to the app's log.
    pub fn new(
        build: AppBuild,
        hooks: UpdateHooks,
        window: WindowSlot,
        cx: &mut Context<Shell>,
    ) -> Option<Self> {
        let environment = LiveUpdateEnvironment::new(&build, hooks)?;
        let stamp = build.clone();
        let model = cx.new(|cx| UpdateModel::new(stamp, Rc::new(environment), cx));
        let diagnostics = cx.subscribe(&model, |_, _, event: &UpdateEvent, _| {
            if let UpdateEvent::Diagnostic(diagnostic) = event {
                write_diagnostic(diagnostic.level, &diagnostic.message);
            }
        });
        Some(Self {
            build,
            model,
            window,
            view: Rc::default(),
            _diagnostics: diagnostics,
        })
    }

    /// The update model.
    #[must_use]
    pub fn model(&self) -> Entity<UpdateModel> {
        self.model.clone()
    }

    /// Brings the window forward, or opens a new one over the model.
    pub fn show(&self, cx: &mut App) {
        let open = *self.window.borrow();
        if let Some(handle) = open
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        let copy = UpdateCopy::new(&self.build, self.model.read(cx).state());
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                gpui::size(style::UPDATE_WIDTH, style::UPDATE_HEIGHT),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(copy.title.into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_min_size: Some(gpui::size(
                style::UPDATE_MIN_WIDTH,
                style::UPDATE_MIN_HEIGHT,
            )),
            ..WindowOptions::default()
        };
        let (build, model) = (self.build.clone(), self.model.clone());
        let shown = Rc::clone(&self.view);
        let opened = cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| UpdateWindow::new(build, model, cx));
            *shown.borrow_mut() = Some(view.downgrade());
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            cx.new(|cx| Root::new(view, window, cx))
        });
        match opened {
            Ok(handle) => *self.window.borrow_mut() = Some(handle),
            Err(error) => log::error(&format!("open update window: {error:#}")),
        }
    }

    /// The window and its view, while open.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> Option<(WindowHandle<Root>, Entity<UpdateWindow>)> {
        let handle = (*self.window.borrow())?;
        let view = self.view.borrow().as_ref()?.upgrade()?;
        Some((handle, view))
    }
}

/// Writes one update diagnostic at the level the model gave it.
fn write_diagnostic(level: DiagnosticLevel, message: &str) {
    let line = format!("update: {message}");
    match level {
        DiagnosticLevel::Info => log::info(&line),
        DiagnosticLevel::Notice => log::notice(&line),
        DiagnosticLevel::Error => log::error(&line),
    }
}

/// The update's fetch and fast-forward: a [`SyncModel`] over the checkout the shell's opener
/// opens, its credential and host-key sheets on the update window, and the update's cancel
/// passed on to it. Runs `refresh_status` then `pull`, the guarded pull the Changes tab runs.
#[must_use]
pub fn sync_runner(services: &Services, window: WindowSlot) -> SyncRunner {
    let opener = Rc::clone(&services.opener);
    let credentials = Arc::clone(&services.credentials);
    let host_keys = Arc::clone(&services.host_keys);
    Rc::new(
        move |root: String, cancel: Arc<CancelToken>, cx: &mut App| {
            let actions = match opener(&root) {
                Ok(opened) => opened.actions,
                Err(error) => {
                    return gpui::Task::ready(UpdateSyncResult::Failure(format!(
                        "cannot open the checkout at {root}: {error}"
                    )));
                }
            };
            let Some(actions) = actions else {
                return gpui::Task::ready(UpdateSyncResult::Failure(format!(
                    "the checkout at {root} cannot be written to"
                )));
            };
            let (credentials, host_keys) = (Arc::clone(&credentials), Arc::clone(&host_keys));
            let sync = cx.new(|_| SyncModel::new(actions, credentials, host_keys));
            set_prompters(&sync, &window, cx);
            cx.spawn(async move |cx| pull(sync, cancel, &root, cx).await)
        },
    )
}

async fn pull(
    sync: Entity<SyncModel>,
    cancel: Arc<CancelToken>,
    root: &str,
    cx: &mut AsyncApp,
) -> UpdateSyncResult {
    let finished = Rc::new(Cell::new(false));
    watch_cancel(&sync, &cancel, &finished, cx);
    sync.update(cx, SyncModel::refresh_status).await;
    if !cancel.is_signalled() {
        sync.update(cx, SyncModel::pull).await;
    }
    finished.set(true);
    let state: SyncState = sync.read_with(cx, |sync, _| sync.state().clone());
    sync_result(&state, root)
}

/// Passes a signalled `cancel` on to the pull, the same stop the Changes tab's Cancel uses,
/// until `finished`.
fn watch_cancel(
    sync: &Entity<SyncModel>,
    cancel: &Arc<CancelToken>,
    finished: &Rc<Cell<bool>>,
    cx: &mut AsyncApp,
) {
    let (sync, cancel, finished) = (sync.downgrade(), Arc::clone(cancel), Rc::clone(finished));
    cx.spawn(async move |cx| {
        while !finished.get() {
            if cancel.is_signalled() {
                sync.update(cx, SyncModel::cancel_running).ok();
                return;
            }
            cx.background_executor().timer(CANCEL_POLL).await;
        }
    })
    .detach();
}

/// Puts the pull's prompts on the update window. With no window open the sender is dropped,
/// which the model reads as nobody to ask.
fn set_prompters(sync: &Entity<SyncModel>, window: &WindowSlot, cx: &mut App) {
    let (credential_window, host_window) = (Rc::clone(window), Rc::clone(window));
    sync.update(cx, |sync, _| {
        sync.set_credential_prompter(Rc::new(move |request, sender, cx| {
            let Some(handle) = *credential_window.borrow() else {
                return;
            };
            handle
                .update(cx, |_, window, cx| {
                    let title = credentials::title(&request);
                    let sheet = cx.new(|cx| CredentialSheet::new(request, sender, window, cx));
                    sheets::open(title.into(), &sheet, window, cx);
                })
                .ok();
        }));
        sync.set_host_key_prompter(Rc::new(move |request, sender, cx| {
            let Some(handle) = *host_window.borrow() else {
                return;
            };
            handle
                .update(cx, |_, window, cx| {
                    let sheet = cx.new(|_| HostKeySheet::new(request, sender));
                    let title = sheet.read(cx).title();
                    sheets::open(title.into(), &sheet, window, cx);
                })
                .ok();
        }));
    });
}

/// Applies one [`LogChange`] to the lines a view shows.
pub fn apply_log(lines: &mut VecDeque<SharedString>, change: LogChange) {
    match change {
        LogChange::None => {}
        LogChange::Replace(replaced) => {
            *lines = replaced.into_iter().map(SharedString::from).collect();
        }
        LogChange::Append {
            drop_head,
            lines: added,
        } => {
            lines.drain(..drop_head.min(lines.len()));
            lines.extend(added.into_iter().map(SharedString::from));
        }
    }
}

/// The update window's content.
pub struct UpdateWindow {
    build: AppBuild,
    model: Entity<UpdateModel>,
    /// The header: what runs now, with its version and build date. Fixed for the process.
    installed: SharedString,
    /// The log as shown, patched by [`LogChange`] rather than rebuilt.
    lines: VecDeque<SharedString>,
    /// How far `lines` reaches.
    mark: Option<LogMark>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _changes: Subscription,
}

impl UpdateWindow {
    /// The window over `model`, showing its log as it stands.
    pub fn new(build: AppBuild, model: Entity<UpdateModel>, cx: &mut Context<Self>) -> Self {
        let copy = UpdateCopy::new(&build, model.read(cx).state());
        let installed = installed_line(&build, copy.heading, rc_git_mac::local_utc_offset);
        let changes = cx.subscribe(&model, |this, _, event: &UpdateEvent, cx| {
            if let UpdateEvent::Changed { .. } = event {
                this.catch_up(cx);
            }
        });
        let mut view = Self {
            build,
            model,
            installed: installed.into(),
            lines: VecDeque::new(),
            mark: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _changes: changes,
        };
        view.catch_up(cx);
        view
    }

    /// The log lines on screen.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<String> {
        self.lines.iter().map(ToString::to_string).collect()
    }

    /// Brings the shown log up to the model: within a run it drops only the head lines the
    /// model trimmed and appends the new tail, so a line costs the same at the end of a long
    /// build as at the start. The mark kept is the model's now, not the event's, which may
    /// trail a later change already applied.
    fn catch_up(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let change = model.log_change(self.mark);
        self.mark = Some(model.log_mark());
        apply_log(&mut self.lines, change);
        if let Some(last) = self.lines.len().checked_sub(1) {
            self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
        }
        cx.notify();
    }

    /// The button: cancels the running fetch or build, otherwise starts, rebuilds anyway, or
    /// tries again, as the model decides.
    pub fn press(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, UpdateModel::press_action).detach();
    }

    /// Return: starts as the button does, but never cancels; Escape cancels while the fetch
    /// or the build runs, as the button's key equivalent did.
    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let cancels = UpdateCopy::new(&self.build, self.model.read(cx).state()).action_cancels;
        match event.keystroke.key.as_str() {
            "enter" if !cancels => self.model.update(cx, UpdateModel::proceed).detach(),
            "escape" if cancels => self.press(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn rows(&self, range: Range<usize>, _cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut rows = Vec::with_capacity(range.len());
        for index in range {
            let text = self.lines.get(index).cloned().unwrap_or_default();
            rows.push(
                div()
                    .id(index)
                    .whitespace_nowrap()
                    .child(text)
                    .into_any_element(),
            );
        }
        rows
    }

    fn footer(copy: &UpdateCopy, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        let tip = copy.status_tool_tip.clone();
        let status = div()
            .id("update-status")
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_size(style::TEXT_CAPTION)
            .text_color(muted)
            .child(SharedString::from(copy.status.clone()))
            .when_some(tip, |status, tip| {
                status.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            });
        let button = Button::new("update-action")
            .label(copy.action_title)
            .disabled(!copy.action_enabled)
            .when(!copy.action_cancels, ButtonVariants::primary)
            .on_click(cx.listener(|this, _, _, cx| this.press(cx)));
        h_flex()
            .gap(style::ROW_GAP)
            .when(copy.spinning, |row| row.child(Spinner::new().small()))
            .child(status)
            .child(button)
    }
}

impl Render for UpdateWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let copy = UpdateCopy::new(&self.build, self.model.read(cx).state());
        let theme = cx.theme();
        let (muted, border, background) = (theme.muted_foreground, theme.border, theme.background);
        let log = uniform_list(
            "update-log",
            self.lines.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| this.rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();
        v_flex()
            .id("update-window")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.key(event, cx)))
            .size_full()
            .p(style::GROUP_GAP)
            .gap(style::MARGIN)
            .bg(background)
            .child(
                v_flex()
                    .gap(style::HAIRLINE)
                    .child(
                        div()
                            .text_size(style::TEXT_BODY)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.installed.clone()),
                    )
                    .child(
                        div()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(muted)
                            .child(SharedString::from(copy.source.clone())),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(style::UPDATE_LOG_MIN_HEIGHT)
                    .p(style::ROW_GAP)
                    .rounded(style::RADIUS)
                    .border(style::HAIRLINE)
                    .border_color(border)
                    .font_family(style::MONOSPACE_FONT)
                    .text_size(style::TEXT_CAPTION)
                    .child(log),
            )
            .child(Self::footer(&copy, cx))
    }
}
