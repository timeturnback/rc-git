//! The open tabs and the hand-off latch (`AppDelegate.swift`, `saveTabs`, `handOffTabs`,
//! `reclaimTabs`). The tabs are handed to the relaunched build, not just saved: both
//! processes are alive at the launch, so the hand-off flushes the write, and from then on this
//! process records nothing — a write after the hand-off would land on top of whatever the new
//! process has already recorded. A launch that fails lifts the latch and records again.

use rc_git_kit::platform::stores::{SettingsTabStore, SharedSettings};
use rc_git_kit::tabs::{HandOff, OpenTabs};

/// The app's open-tab list and whether it has been handed to a relaunching build.
#[derive(Debug)]
pub struct TabSession {
    tabs: OpenTabs<SettingsTabStore>,
    handed_off: bool,
}

impl TabSession {
    /// The tabs kept in `settings`, not handed off.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self {
            tabs: OpenTabs::new(SettingsTabStore::new(settings)),
            handed_off: false,
        }
    }

    /// The saved tabs.
    #[must_use]
    pub fn tabs(&self) -> &OpenTabs<SettingsTabStore> {
        &self.tabs
    }

    /// Whether the tabs went to a relaunching build, so this process no longer records them.
    #[must_use]
    pub fn is_handed_off(&self) -> bool {
        self.handed_off
    }

    /// Records the tabs open now (see [`OpenTabs::record`]); `dropping` is the repository of
    /// a tab closed on its own with ⌘W. Returns `false`, writing nothing, once the tabs have
    /// been handed off.
    pub fn record(
        &mut self,
        live: &[String],
        selected_index: usize,
        dropping: Option<&str>,
    ) -> bool {
        if self.handed_off {
            return false;
        }
        self.tabs.record(live, selected_index, dropping);
        true
    }

    /// Hands the tabs to the build about to replace this one, flushed so the new process
    /// reads them, and stops recording after that.
    pub fn hand_off(&mut self, live: &[String], selected_index: usize) -> HandOff {
        let handed = self.tabs.hand_off(live, selected_index);
        self.handed_off = true;
        handed
    }

    /// Undoes [`Self::hand_off`] after a relaunch that failed: this process keeps running, so
    /// it records its tabs again, starting with the ones on screen now.
    pub fn reclaim(&mut self, live: &[String], selected_index: usize) {
        self.handed_off = false;
        self.record(live, selected_index, None);
    }
}
