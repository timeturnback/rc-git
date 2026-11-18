//! The dialogs a repository window asks with (`IdentitySheet.swift`, `SyncPrompts.swift` and
//! the window controller's challenge and name sheets), each a gpui-component dialog over a
//! small entity that holds its fields and decides nothing: the words, and when its confirm
//! button takes a click, come from kit's [`rc_git_kit::guard_copy`] and
//! [`rc_git_kit::ref_menu`].
//!
//! A sheet answers once. Its confirm and cancel buttons close the dialog; Escape answers as
//! Cancel does. A prompt sheet that is dropped unanswered (its window closed) drops its
//! sender, which the sync model reads as nobody to ask.

pub mod challenge;
pub mod credentials;
pub mod github_sign_in;
pub mod host_key;
pub mod identity;
pub mod ref_name;

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, WeakEntity, Window, div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, WindowExt};

use crate::style;

/// The sheet a window has on screen, for tests to answer.
#[derive(Clone)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "tests answer the sheet on screen through it")
)]
pub enum OpenSheet {
    /// A guard challenge.
    Challenge(WeakEntity<challenge::ChallengeSheet>),
    /// The challenge of pull request `number`'s merge.
    Merge {
        /// The pull request.
        number: u64,
        /// The sheet.
        sheet: WeakEntity<challenge::ChallengeSheet>,
    },
    /// A credentials prompt.
    Credentials(WeakEntity<credentials::CredentialSheet>),
    /// An unknown host key.
    HostKey(WeakEntity<host_key::HostKeySheet>),
    /// A commit identity.
    Identity(WeakEntity<identity::IdentitySheet>),
    /// A new branch or tag name.
    RefName(WeakEntity<ref_name::RefNameSheet>),
}

/// A sheet's answer to a dismissal it did not ask for: Escape, or the window closing it.
pub trait Dismissible: Render {
    /// Answers as the Cancel button does.
    fn dismissed(&mut self, cx: &mut gpui::Context<Self>);

    /// The field that takes the keyboard when the sheet opens.
    fn first_field(&self, cx: &App) -> Option<FocusHandle>;
}

/// Opens `sheet` as a dialog titled `title` in `window`.
pub fn open<T: Dismissible>(
    title: SharedString,
    sheet: &Entity<T>,
    window: &mut Window,
    cx: &mut App,
) {
    let sheet = sheet.clone();
    let first = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false)
            && let Some(field) = sheet.read(cx).first_field(cx)
        {
            window.focus(&field, cx);
        }
        let on_cancel = sheet.clone();
        dialog
            .title(title.clone())
            .w(style::SHEET_FIELD_WIDTH + style::GROUP_GAP * 4.)
            .close_button(false)
            .overlay_closable(false)
            .on_cancel(move |_, _, cx| {
                on_cancel.update(cx, Dismissible::dismissed);
                true
            })
            .child(sheet.clone())
    });
}

/// Closes the sheet on screen.
pub fn close(window: &mut Window, cx: &mut App) {
    window.close_dialog(cx);
}

/// What a sheet button's click runs.
pub type OnClick = Box<dyn Fn(&mut Window, &mut App)>;

/// One button of a sheet's row.
pub struct SheetButton {
    /// The element id.
    pub id: &'static str,
    /// The label.
    pub label: SharedString,
    /// Whether it is the confirm button, drawn as the primary one.
    pub primary: bool,
    /// Whether a destructive confirm reads as one.
    pub danger: bool,
    /// Whether it takes a click.
    pub enabled: bool,
    /// What a click does.
    pub on_click: OnClick,
}

/// A sheet's body: the message, the fields, the problem line, then the buttons on the right.
pub fn body(
    message: Option<String>,
    fields: Vec<AnyElement>,
    problem: Option<&str>,
    buttons: Vec<SheetButton>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut row = div().flex().justify_end().gap(style::ROW_GAP);
    for button in buttons {
        let variant = if button.danger {
            ButtonVariant::Danger
        } else if button.primary {
            ButtonVariant::Primary
        } else {
            ButtonVariant::default()
        };
        let on_click = button.on_click;
        row = row.child(
            Button::new(button.id)
                .label(button.label)
                .with_variant(variant)
                .disabled(!button.enabled)
                .on_click(move |_, window, cx| on_click(window, cx)),
        );
    }
    div()
        .id("sheet")
        .flex()
        .flex_col()
        .gap(style::ROW_GAP)
        .text_size(style::TEXT_BODY)
        .when_some(message, |column, message| {
            column.child(
                div()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(SharedString::from(message)),
            )
        })
        .children(fields)
        .when_some(problem.map(str::to_owned), |column, problem| {
            column.child(
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(theme.red)
                    .child(SharedString::from(problem)),
            )
        })
        .child(row)
}
