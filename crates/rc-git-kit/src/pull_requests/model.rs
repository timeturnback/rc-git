//! One window's pull requests: the link of its repository, the list, and the open pull
//! request tabs. Every read runs off the UI thread through [`PullRequestService`], one at a
//! time per list and per tab with at most one queued behind it, and its answer is taken only
//! under the generation it was asked for ([`super::state`]). Polling follows
//! [`super::polling`]: a timer is armed only for what is on screen in the active window, and
//! re-armed after each read.

use std::sync::Arc;

use gpui::{Context, Task};

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
        let (Some(number), Some(linked)) = (self.selected, self.linked()) else {
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
        if let (Some(next), Some(linked)) = (next, self.linked()) {
            self.run_detail(next, linked, cx);
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
