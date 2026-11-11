//! History's search bar (`HistoryViewController.buildSearchArea`): the query field, the
//! Message/Author scope, the Dates toggle and, under it, the Committed since/until days. The
//! controls describe a [`CommitFilter`]; the pane hands it to the model, which searches.
//!
//! The day fields read `YYYY-MM-DD` (kit `dates::parse_day`); a field that does not parse adds
//! no bound, and is drawn as invalid. The bounds are local days through kit
//! `ref_filter::day_bounds`, in the user's zone.

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;
use rc_git_core::CommitFilter;
use rc_git_kit::dates::{format_day, instant_in_day, local_day, parse_day};
use rc_git_kit::ref_filter::{CommitSearchScope, day_bounds};

/// The zone the date filter and the dates on screen are read in: seconds east of UTC at an
/// instant.
pub type OffsetAt = fn(i64) -> i32;

/// The zone dates are read in, and the instant the date filter's fields start from.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    /// The zone.
    pub offset_at: OffsetAt,
    /// Now, seconds since the Unix epoch.
    pub now: i64,
}

/// How many days before today the Since field starts at, as the Swift picker did.
pub const SINCE_DAYS_BACK: i64 = 30;

/// The search controls of one History pane.
pub struct SearchControls {
    /// The query field.
    pub query: Entity<InputState>,
    /// The Committed since field.
    pub since: Entity<InputState>,
    /// The Committed until field.
    pub until: Entity<InputState>,
    /// Which commit field the query matches.
    pub scope: CommitSearchScope,
    /// Whether the dates row shows; its bounds count only while it does.
    pub dates_open: bool,
    /// Whether the since bound is on.
    pub since_on: bool,
    /// Whether the until bound is on.
    pub until_on: bool,
    offset_at: OffsetAt,
}

impl SearchControls {
    /// Empty controls; the day fields start a month ago and today.
    pub fn new<T: 'static>(
        offset_at: OffsetAt,
        now: i64,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let today = local_day(now, offset_at(now));
        let since_text = format_day(today - SINCE_DAYS_BACK);
        let until_text = format_day(today);
        Self {
            query: cx.new(|cx| InputState::new(window, cx).placeholder("Search commits")),
            since: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("YYYY-MM-DD")
                    .default_value(since_text)
            }),
            until: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("YYYY-MM-DD")
                    .default_value(until_text)
            }),
            scope: CommitSearchScope::Message,
            dates_open: false,
            since_on: false,
            until_on: false,
            offset_at,
        }
    }

    /// The filter the controls describe now.
    #[must_use]
    pub fn filter(&self, cx: &gpui::App) -> CommitFilter {
        let query = self.query.read(cx).value().trim().to_owned();
        let mut filter = CommitFilter::default();
        if !query.is_empty() {
            match self.scope {
                CommitSearchScope::Author => filter.author = Some(query),
                CommitSearchScope::Message => filter.message = Some(query),
            }
        }
        if self.dates_open {
            let offset_at = self.offset_at;
            let since = self.day(&self.since, self.since_on, cx);
            let until = self.day(&self.until, self.until_on, cx);
            let bounds = day_bounds(since, until, offset_at);
            filter.after = bounds.after;
            filter.before = bounds.before;
        }
        filter
    }

    /// Whether the field holds a day the calendar has; an empty field counts as valid.
    #[must_use]
    pub fn is_valid(field: &Entity<InputState>, cx: &gpui::App) -> bool {
        let text = field.read(cx).value();
        text.trim().is_empty() || parse_day(&text).is_some()
    }

    /// An instant inside the day `field` names, when `on` and the field parses.
    fn day(&self, field: &Entity<InputState>, on: bool, cx: &gpui::App) -> Option<i64> {
        if !on {
            return None;
        }
        let day = parse_day(&field.read(cx).value())?;
        Some(instant_in_day(day, self.offset_at))
    }

    /// Opens or closes the dates row; opening sets the until field to today, as Swift did.
    pub fn toggle_dates(&mut self, now: i64, window: &mut Window, cx: &mut gpui::App) {
        self.dates_open = !self.dates_open;
        if self.dates_open {
            let today = format_day(local_day(now, (self.offset_at)(now)));
            self.until
                .update(cx, |field, cx| field.set_value(today, window, cx));
        }
    }

    /// Clears the query and closes the dates row: the model dropped the search, so the
    /// controls must not show one over the unfiltered list.
    pub fn clear(&mut self, window: &mut Window, cx: &mut gpui::App) {
        self.query
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.dates_open = false;
    }
}
