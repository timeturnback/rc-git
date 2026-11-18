//! One window's pull requests: the link of its repository, the list, and the open pull
//! request tabs. Every read runs off the UI thread through [`PullRequestService`], one at a
//! time per list and per tab with at most one queued behind it, and its answer is taken only
//! under the generation it was asked for ([`super::state`]). Polling follows
//! [`super::polling`]: a timer is armed only for what is on screen in the active window, and
//! re-armed after each read.
//!
//! A tab's changes ([`super::changes`]) are read once the user shows them, always for the
//! stamp of the tab's current read: a read that lands with another head or base starts them
//! over. The head is fetched only from [`PullRequestsModel::fetch_changes`], the user's click.

use std::sync::Arc;

use gpui::{Context, Task};

use super::changes::{ChangesLoad, ChangesState, FileLoad};
use super::link::{Linked, RepositoryLink};
use super::polling::{
    DETAIL_INTERVAL, LIST_INTERVAL, PollInputs, PollPlan, detail_poll, is_due, list_poll,
};
use super::state::{DetailLoad, DetailState, ListLoad, ListState};
use super::{Cancel, PullRequestService};
use crate::blocking::blocking;
use crate::platform::api_tokens::AccountKey;

/// Now, in seconds since the Unix epoch; a test supplies its own.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The fetch tab `number`'s changes asked for, under their generation and stamp.
struct FetchAsked {
    number: u64,
    generation: u64,
    stamp: super::PullRequestStamp,
}

/// One tab's changes and the cancel flag of its running fetch.
struct TabChanges {
    number: u64,
    state: ChangesState,
    fetch_cancel: Option<Cancel>,
}

impl TabChanges {
    fn stop_fetch(&mut self) {
        if let Some(cancel) = self.fetch_cancel.take() {
            cancel.signal();
        }
    }
}

/// One window's pull requests.
pub struct PullRequestsModel {
    service: Arc<dyn PullRequestService>,
    clock: Clock,
    repo_path: Option<String>,
    link: Option<RepositoryLink>,
    link_generation: u64,
    list: ListState,
    tabs: Vec<DetailState>,
    selected: Option<u64>,
    section_visible: bool,
    page_visible: bool,
    window_active: bool,
    list_cancel: Option<Cancel>,
    detail_cancels: Vec<(u64, Cancel)>,
    list_timer: Option<Task<()>>,
    detail_timer: Option<Task<()>>,
    choice_problem: Option<String>,
    changes: Vec<TabChanges>,
}

impl std::fmt::Debug for PullRequestsModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PullRequestsModel")
            .field("repo_path", &self.repo_path)
            .field("link", &self.link)
            .field("tabs", &self.tabs.len())
            .finish_non_exhaustive()
    }
}

impl PullRequestsModel {
    /// A model with no repository, reading through `service`, the section shown and the
    /// window active until told otherwise.
    #[must_use]
    pub fn new(service: Arc<dyn PullRequestService>, clock: Clock) -> Self {
        Self {
            service,
            clock,
            repo_path: None,
            link: None,
            link_generation: 0,
            list: ListState::new(),
            tabs: Vec::new(),
            selected: None,
            section_visible: true,
            page_visible: false,
            window_active: true,
            list_cancel: None,
            detail_cancels: Vec::new(),
            list_timer: None,
            detail_timer: None,
            choice_problem: None,
            changes: Vec::new(),
        }
    }

    /// What the repository's remote links to; `None` while it is being read.
    #[must_use]
    pub fn link(&self) -> Option<&RepositoryLink> {
        self.link.as_ref()
    }

    /// The list.
    #[must_use]
    pub fn list(&self) -> &ListState {
        &self.list
    }

    /// The open tabs, in the order they opened.
    #[must_use]
    pub fn tabs(&self) -> &[DetailState] {
        &self.tabs
    }

    /// The tab on show when the page shows.
    #[must_use]
    pub fn selected_tab(&self) -> Option<&DetailState> {
        let number = self.selected?;
        self.tabs.iter().find(|tab| tab.number() == number)
    }

    /// Why the last account choice could not be saved.
    #[must_use]
    pub fn choice_problem(&self) -> Option<&str> {
        self.choice_problem.as_deref()
    }

    /// Whether the section shows.
    #[must_use]
    pub fn section_visible(&self) -> bool {
        self.section_visible
    }

    /// Whether the page of the selected tab shows.
    #[must_use]
    pub fn page_visible(&self) -> bool {
        self.page_visible && self.selected.is_some()
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn linked(&self) -> Option<Linked> {
        match &self.link {
            Some(RepositoryLink::Linked(linked)) => Some(linked.clone()),
            Some(_) | None => None,
        }
    }

    /// Shows the repository at `repo_path` (its tab path): reads its link, then its list.
    pub fn attach(&mut self, repo_path: String, cx: &mut Context<Self>) -> Task<()> {
        self.repo_path = Some(repo_path);
        self.link = None;
        self.forget(cx);
        self.relink(cx)
    }

    /// Drops the list and every tab: a running read's answer will be dropped.
    fn forget(&mut self, cx: &mut Context<Self>) {
        self.list.reset();
        if let Some(cancel) = self.list_cancel.take() {
            cancel.signal();
        }
        for (_, cancel) in self.detail_cancels.drain(..) {
            cancel.signal();
        }
        self.tabs.clear();
        for changes in &mut self.changes {
            changes.stop_fetch();
        }
        self.changes.clear();
        self.selected = None;
        self.list_timer = None;
        self.detail_timer = None;
        cx.notify();
    }

    /// Reads the link again: an account signed in or out, a choice made, a remote changed.
    /// An unchanged link keeps the list and the tabs; a changed one starts over.
    pub fn relink(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(path) = self.repo_path.clone() else {
            return Task::ready(());
        };
        self.link_generation += 1;
        let generation = self.link_generation;
        let service = Arc::clone(&self.service);
        let read = blocking(cx.background_executor(), move || service.link(&path));
        cx.spawn(async move |this, cx| {
            let link = read.await;
            this.update(cx, |this, cx| this.linked_as(generation, link, cx))
                .ok();
        })
    }

    fn linked_as(&mut self, generation: u64, link: RepositoryLink, cx: &mut Context<Self>) {
        if generation != self.link_generation {
            return;
        }
        if self.link.as_ref() == Some(&link) {
            return;
        }
        self.forget(cx);
        self.link = Some(link);
        if self.section_visible {
            self.refresh_list(cx);
        }
        cx.notify();
    }

    /// Reads the first page again now.
    pub fn refresh_list(&mut self, cx: &mut Context<Self>) {
        let Some(linked) = self.linked() else {
            return;
        };
        if let Some(load) = self.list.refresh() {
            self.run_list(load, linked, cx);
        }
        cx.notify();
    }

    /// Reads the next page.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(linked) = self.linked() else {
            return;
        };
        if let Some(load) = self.list.load_more() {
            self.run_list(load, linked, cx);
        }
        cx.notify();
    }

    fn run_list(&mut self, load: ListLoad, linked: Linked, cx: &mut Context<Self>) {
        self.list_timer = None;
        let cancel = Cancel::new();
        self.list_cancel = Some(cancel.clone());
        let service = Arc::clone(&self.service);
        let ListLoad { generation, after } = load;
        let read = blocking(cx.background_executor(), move || {
            service.list(&linked, after.as_ref(), &cancel)
        });
        cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |this, cx| {
                let now = this.now();
                if let Some(next) = this.list.finish(generation, answer, now)
                    && let Some(linked) = this.linked()
                {
                    this.run_list(next, linked, cx);
                }
                this.schedule_list_poll(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn inputs(&self, visible: bool) -> PollInputs {
        PollInputs {
            window_active: self.window_active,
            visible,
            now: self.now(),
        }
    }

    fn schedule_list_poll(&mut self, cx: &mut Context<Self>) {
        self.list_timer = None;
        if self.list.is_loading() || self.linked().is_none() {
            return;
        }
        let plan = list_poll(self.inputs(self.section_visible), self.list.freshness());
        self.list_timer = Self::timer(plan, cx, Self::refresh_list);
    }

    fn timer(
        plan: PollPlan,
        cx: &mut Context<Self>,
        then: fn(&mut Self, &mut Context<Self>),
    ) -> Option<Task<()>> {
        let PollPlan::After(delay) = plan else {
            return None;
        };
        Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, then).ok();
        }))
    }

    /// Opens pull request `number`'s tab, or brings it forward, and reads it.
    pub fn open(&mut self, number: u64, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|tab| tab.number() == number) {
            self.tabs.push(DetailState::new(number));
        }
        self.selected = Some(number);
        self.page_visible = true;
        self.refresh_detail(cx);
    }

    /// Closes pull request `number`'s tab and stops its read.
    pub fn close(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.number() == number) else {
            return;
        };
        self.tabs.remove(index);
        self.changes.retain_mut(|changes| {
            if changes.number == number {
                changes.stop_fetch();
            }
            changes.number != number
        });
        self.detail_cancels.retain(|(tab, cancel)| {
            if *tab == number {
                cancel.signal();
            }
            *tab != number
        });
        if self.selected == Some(number) {
            let neighbour = self.tabs.get(index).or_else(|| self.tabs.last());
            self.selected = neighbour.map(DetailState::number);
            self.detail_timer = None;
            self.schedule_detail_poll(cx);
        }
        cx.notify();
    }

    /// Shows tab `number`'s page.
    pub fn select(&mut self, number: u64, cx: &mut Context<Self>) {
        if self.tabs.iter().any(|tab| tab.number() == number) {
            self.selected = Some(number);
            self.page_visible = true;
            self.came_into_view(cx);
            cx.notify();
        }
    }

    /// Reads the selected tab again now.
    pub fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(number) = self.selected {
            self.refresh_detail_of(number, cx);
        }
    }

    /// Reads tab `number` again now.
    fn refresh_detail_of(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(linked) = self.linked() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.number() == number) else {
            return;
        };
        if let Some(load) = tab.refresh() {
            self.run_detail(load, linked, cx);
        }
        cx.notify();
    }

    fn run_detail(&mut self, load: DetailLoad, linked: Linked, cx: &mut Context<Self>) {
        if self.selected == Some(load.number) {
            self.detail_timer = None;
        }
        let cancel = Cancel::new();
        self.detail_cancels
            .retain(|(number, _)| *number != load.number);
        self.detail_cancels.push((load.number, cancel.clone()));
        let service = Arc::clone(&self.service);
        let read = blocking(cx.background_executor(), move || {
            service.detail(&linked, load.number, &cancel)
        });
        cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |this, cx| this.detail_read(load, answer, cx))
                .ok();
        })
        .detach();
    }

    fn detail_read(
        &mut self,
        load: DetailLoad,
        answer: Result<super::PullRequestDetail, super::ReadError>,
        cx: &mut Context<Self>,
    ) {
        let now = self.now();
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.number() == load.number) else {
            return;
        };
        let next = tab.finish(load.generation, answer, now);
        let stamp = tab.stamp();
        if let (Some(next), Some(linked)) = (next, self.linked()) {
            self.run_detail(next, linked, cx);
        }
        if let Some(stamp) = stamp {
            self.follow_stamp(load.number, stamp, cx);
        }
        if self.selected == Some(load.number) {
            self.schedule_detail_poll(cx);
        }
        cx.notify();
    }

    fn schedule_detail_poll(&mut self, cx: &mut Context<Self>) {
        self.detail_timer = None;
        let visible = self.page_visible();
        let Some(tab) = self.selected_tab() else {
            return;
        };
        if tab.is_loading() {
            return;
        }
        let plan = detail_poll(
            self.inputs(visible),
            tab.freshness(),
            tab.has_pending_checks(),
        );
        self.detail_timer = Self::timer(plan, cx, Self::refresh_detail);
    }

    /// Whether the section shows: a hidden section neither polls nor reads.
    pub fn set_section_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.section_visible == visible {
            return;
        }
        self.section_visible = visible;
        self.came_into_view(cx);
        cx.notify();
    }

    /// Whether the selected tab's page shows.
    pub fn set_page_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.page_visible == visible {
            return;
        }
        self.page_visible = visible;
        self.came_into_view(cx);
        cx.notify();
    }

    /// Whether the window is the active one: an inactive window polls nothing.
    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.window_active == active {
            return;
        }
        self.window_active = active;
        self.came_into_view(cx);
    }

    /// Re-plans both polls after a change of what shows: reads what is due now, arms the
    /// timers for the rest, and disarms what no longer shows.
    fn came_into_view(&mut self, cx: &mut Context<Self>) {
        let now = self.now();
        let list_due = self.window_active
            && self.section_visible
            && is_due(self.list.freshness(), LIST_INTERVAL, now);
        if list_due && !self.list.is_loading() {
            self.refresh_list(cx);
        } else {
            self.schedule_list_poll(cx);
        }
        let detail_due = self.window_active
            && self.page_visible()
            && self.selected_tab().is_some_and(|tab| {
                !tab.is_loading() && is_due(tab.freshness(), DETAIL_INTERVAL, now)
            });
        if detail_due {
            self.refresh_detail(cx);
        } else {
            self.schedule_detail_poll(cx);
        }
    }

    /// Uses account `key` for this repository from now on, then reads the link again.
    pub fn choose_account(&mut self, key: AccountKey, cx: &mut Context<Self>) -> Task<()> {
        let Some(path) = self.repo_path.clone() else {
            return Task::ready(());
        };
        let service = Arc::clone(&self.service);
        let save = blocking(cx.background_executor(), move || {
            service.choose_account(&path, &key)
        });
        cx.spawn(async move |this, cx| {
            let saved = save.await;
            let relink = this.update(cx, |this, cx| {
                this.choice_problem = saved.err();
                cx.notify();
                this.relink(cx)
            });
            if let Ok(relink) = relink {
                relink.await;
            }
        })
    }

    /// The changes of tab `number`, once shown.
    #[must_use]
    pub fn changes(&self, number: u64) -> Option<&ChangesState> {
        self.changes
            .iter()
            .find(|changes| changes.number == number)
            .map(|changes| &changes.state)
    }

    /// The selected tab's changes, once shown.
    #[must_use]
    pub fn selected_changes(&self) -> Option<&ChangesState> {
        self.changes(self.selected?)
    }

    /// Shows the selected tab's changes: reads what the repository holds for its current read,
    /// unless they are already read for it. Nothing until the tab's read is current. Never
    /// fetches.
    pub fn show_changes(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        let stamp = self.selected_tab().and_then(DetailState::stamp);
        let Some(stamp) = stamp else {
            return;
        };
        if self
            .changes(number)
            .is_some_and(|shown| *shown.stamp() == stamp)
        {
            return;
        }
        self.start_changes(number, stamp, cx);
    }

    /// Starts tab `number`'s changes over for `stamp`, stopping a fetch for the old one.
    fn start_changes(
        &mut self,
        number: u64,
        stamp: super::PullRequestStamp,
        cx: &mut Context<Self>,
    ) {
        let (state, load) = ChangesState::new(stamp);
        self.changes.retain_mut(|changes| {
            if changes.number == number {
                changes.stop_fetch();
            }
            changes.number != number
        });
        self.changes.push(TabChanges {
            number,
            state,
            fetch_cancel: None,
        });
        self.run_changes(number, load, cx);
        cx.notify();
    }

    /// After tab `number` read `stamp`: shown changes of another head or base start over.
    fn follow_stamp(
        &mut self,
        number: u64,
        stamp: super::PullRequestStamp,
        cx: &mut Context<Self>,
    ) {
        let stale = self
            .changes(number)
            .is_some_and(|shown| *shown.stamp() != stamp);
        if stale {
            self.start_changes(number, stamp, cx);
        }
    }

    fn changes_mut(&mut self, number: u64) -> Option<&mut TabChanges> {
        self.changes
            .iter_mut()
            .find(|changes| changes.number == number)
    }

    /// Fetches the selected tab's head: the user's click, and the only way a fetch starts.
    pub fn fetch_changes(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        let load = self
            .changes_mut(number)
            .and_then(|changes| changes.state.fetch());
        if let Some(load) = load {
            self.run_changes(number, load, cx);
        }
        cx.notify();
    }

    /// Stops the selected tab's running fetch; Fetch Changes shows again at once.
    pub fn cancel_changes_fetch(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        if let Some(changes) = self.changes_mut(number)
            && changes.state.cancel_fetch()
        {
            changes.stop_fetch();
        }
        cx.notify();
    }

    /// Reads the selected tab's changes again (after fetching the base, say).
    pub fn reread_changes(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        let load = self
            .changes_mut(number)
            .and_then(|changes| changes.state.reread());
        if let Some(load) = load {
            self.run_changes(number, load, cx);
        }
        cx.notify();
    }

    fn run_changes(&mut self, number: u64, load: ChangesLoad, cx: &mut Context<Self>) {
        let (Some(path), Some(linked)) = (self.repo_path.clone(), self.linked()) else {
            return;
        };
        let service = Arc::clone(&self.service);
        match load {
            ChangesLoad::Read { generation, stamp } => {
                let read = blocking(cx.background_executor(), {
                    let stamp = stamp.clone();
                    move || service.changes(&path, &linked, &stamp)
                });
                cx.spawn(async move |this, cx| {
                    let answer = read.await;
                    this.update(cx, |this, cx| {
                        if let Some(changes) = this.changes_mut(number) {
                            changes.state.finish_read(generation, &stamp, answer);
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            ChangesLoad::Fetch { generation, stamp } => {
                let asked = FetchAsked {
                    number,
                    generation,
                    stamp,
                };
                self.run_fetch(asked, cx);
            }
        }
    }

    fn run_fetch(&mut self, asked: FetchAsked, cx: &mut Context<Self>) {
        let number = asked.number;
        let (Some(path), Some(linked)) = (self.repo_path.clone(), self.linked()) else {
            return;
        };
        let cancel = Cancel::new();
        if let Some(changes) = self.changes_mut(number) {
            changes.stop_fetch();
            changes.fetch_cancel = Some(cancel.clone());
        }
        let service = Arc::clone(&self.service);
        let fetch = blocking(cx.background_executor(), {
            let stamp = asked.stamp.clone();
            move || service.fetch_head(&path, &linked, &stamp, &cancel)
        });
        cx.spawn(async move |this, cx| {
            let answer = fetch.await;
            this.update(cx, |this, cx| this.fetched(&asked, answer, cx))
                .ok();
        })
        .detach();
    }

    fn fetched(
        &mut self,
        asked: &FetchAsked,
        answer: Result<super::changes::HeadFetched, super::changes::FetchProblem>,
        cx: &mut Context<Self>,
    ) {
        let number = asked.number;
        let Some(changes) = self.changes_mut(number) else {
            return;
        };
        let next = changes
            .state
            .finish_fetch(asked.generation, &asked.stamp, answer);
        if !changes.state.is_fetching() {
            changes.fetch_cancel = None;
        }
        let moved = matches!(
            changes.state.phase(),
            super::changes::ChangesPhase::Moved { .. }
        );
        if let Some(next) = next {
            self.run_changes(number, next, cx);
        }
        if moved {
            self.refresh_detail_of(number, cx);
        }
        cx.notify();
    }

    /// Selects file `index` of the selected tab's changes and reads its diff.
    pub fn select_changed_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        let load = self
            .changes_mut(number)
            .and_then(|changes| changes.state.select(index));
        cx.notify();
        let (Some(load), Some(path)) = (load, self.repo_path.clone()) else {
            return;
        };
        self.run_file(number, load, path, cx);
    }

    fn run_file(&mut self, number: u64, load: FileLoad, path: String, cx: &mut Context<Self>) {
        let service = Arc::clone(&self.service);
        let read = blocking(cx.background_executor(), {
            let change = load.change.clone();
            move || service.file_diff(&path, &change)
        });
        cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |this, cx| {
                if let Some(changes) = this.changes_mut(number) {
                    changes.state.finish_file(&load, answer);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether the list's poll timer is armed.
    #[must_use]
    pub fn list_poll_armed(&self) -> bool {
        self.list_timer.is_some()
    }

    /// Whether the selected tab's poll timer is armed.
    #[must_use]
    pub fn detail_poll_armed(&self) -> bool {
        self.detail_timer.is_some()
    }
}
