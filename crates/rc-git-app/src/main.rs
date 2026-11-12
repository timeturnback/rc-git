//! rc-git on GPUI: the app shell, one window per repository grouped as native tabs, Files and
//! History. Changes and the sync dialogs land in later phases.

mod app;
mod blame;
mod changes;
mod diff;
mod files;
mod history;
mod icons;
mod inbox;
mod log;
mod menus;
mod palette;
mod sheets;
mod sidebar;
mod status;
mod style;
mod window;

use std::sync::Arc;

use gpui::{App, AppContext, Entity};
use rc_git_kit::back_forward::Direction;
use rc_git_kit::file_url;
use rc_git_kit::platform::credentials::CachingCredentialStore;
use rc_git_kit::platform::host_keys::SettingsHostKeyStore;
use rc_git_kit::platform::stores::SharedSettings;
use rc_git_mac::{FsEventsWatchers, KeychainCredentialStore, Preferences};

use crate::app::{Services, Shell, ShellGlobal, live_opener};
use crate::inbox::Inbox;
use crate::menus::{
    BringAllToFront, ClearRecents, CloseWindow, GoBack, GoForward, HideApp, HideOthers, NewTab,
    OpenRecent, OpenRepository, Quit, ReopenClosedTab, ShowAll, ToggleHiddenFiles,
};

/// The live services: the app's own preferences domain, the core, `FSEvents`, the keychain.
fn live_services() -> Services {
    let settings: SharedSettings = Arc::new(Preferences::current_application());
    Services {
        settings: Arc::clone(&settings),
        opener: live_opener(),
        watchers: Arc::new(FsEventsWatchers),
        credentials: Arc::new(CachingCredentialStore::new(KeychainCredentialStore::new())),
        host_keys: Arc::new(SettingsHostKeyStore::new(settings)),
        home: std::env::var("HOME").unwrap_or_default(),
        app_name: "rc-git".to_owned(),
    }
}

/// Registers the commands the shell answers, app-wide.
fn register_actions(shell: &Entity<Shell>, cx: &mut App) {
    let on = shell.clone();
    cx.on_action(move |_: &NewTab, cx| {
        on.update(cx, |shell, cx| {
            shell.new_window(cx);
        });
    });
    let on = shell.clone();
    cx.on_action(move |_: &ReopenClosedTab, cx| {
        on.update(cx, Shell::reopen_closed_tab);
    });
    let on = shell.clone();
    cx.on_action(move |_: &OpenRepository, cx| {
        on.update(cx, |_, cx| Shell::choose_repository(cx));
    });
    let on = shell.clone();
    cx.on_action(move |action: &OpenRecent, cx| {
        on.update(cx, |shell, cx| {
            shell.open(&action.path, cx);
        });
    });
    let on = shell.clone();
    cx.on_action(move |_: &ClearRecents, cx| on.update(cx, Shell::clear_recents));
    let on = shell.clone();
    cx.on_action(move |_: &GoBack, cx| on.update(cx, |shell, cx| shell.go(Direction::Back, cx)));
    let on = shell.clone();
    cx.on_action(move |_: &GoForward, cx| {
        on.update(cx, |shell, cx| shell.go(Direction::Forward, cx));
    });
    let on = shell.clone();
    cx.on_action(move |_: &ToggleHiddenFiles, cx| on.update(cx, Shell::toggle_hidden_files));
    let on = shell.clone();
    cx.on_action(move |_: &CloseWindow, cx| on.update(cx, Shell::close_window_group));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
}

/// Builds the shell, opens what launched the app, restores the saved tabs, and keeps opening
/// whatever the system hands over afterwards.
fn start(services: Services, launch: &[String], urls: Inbox<String>, cx: &mut App) {
    gpui_component::init(cx);
    gpui_component::Theme::sync_system_appearance(None, cx);
    match menus::key_bindings(cx) {
        Ok(bindings) => cx.bind_keys(bindings),
        Err(error) => log::error(&format!("{error:#}")),
    }
    let shell = cx.new(|cx| Shell::new(services, cx));
    cx.set_global(ShellGlobal(shell.clone()));
    register_actions(&shell, cx);
    shell.update(cx, |shell, cx| {
        shell.rebuild_menus(cx);
        for path in launch.iter().chain(&urls.take()) {
            if let Some(path) = file_url::path(path) {
                shell.open(&path, cx);
            }
        }
        shell.restore_tabs(cx);
    });
    cx.activate(true);
    let weak = shell.downgrade();
    cx.spawn(async move |cx| {
        loop {
            let arrived = urls.next().await;
            let opened = weak.update(cx, |shell, cx| {
                for url in &arrived {
                    if let Some(path) = file_url::path(url) {
                        shell.open(&path, cx);
                    }
                }
            });
            if opened.is_err() {
                return;
            }
        }
    })
    .detach();
}

fn main() {
    let launch: Vec<String> = std::env::args()
        .skip(1)
        .filter(|argument| !argument.starts_with('-'))
        .collect();
    let urls: Inbox<String> = Inbox::default();
    let sender = urls.clone();
    let application = gpui_platform::application().with_assets(icons::Assets);
    application.on_open_urls(move |list| {
        for url in list {
            sender.push(url);
        }
    });
    application.on_reopen(|cx| {
        let Some(shell) = cx
            .try_global::<ShellGlobal>()
            .map(|global| global.0.clone())
        else {
            return;
        };
        shell.update(cx, |shell, cx| {
            if shell.windows().is_empty() {
                shell.new_window(cx);
            }
        });
    });
    application.run(move |cx: &mut App| start(live_services(), &launch, urls, cx));
}

#[cfg(test)]
mod tests;
