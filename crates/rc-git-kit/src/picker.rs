//! The keyboard rules every quick-pick panel shares (Go to File, Find in Repository, Open
//! Recent Repository, the ⌘B ref picker): where the up and down arrows move the highlight,
//! and which row stays highlighted when the list changes under it.

/// The row `delta` rows from `current`, clamped to the list; from no highlight, any step lands
/// on the first row. `None` for an empty list.
#[must_use]
pub fn moved(current: Option<usize>, delta: isize, count: usize) -> Option<usize> {
    let last = count.checked_sub(1)?;
    let Some(current) = current else {
        return Some(0);
    };
    Some(current.saturating_add_signed(delta).min(last))
}

/// The next row `step` (1 or -1) away from `current` that `pickable` accepts, stepping over
/// group headers; `None` when there is none that way, so the highlight stays put. From no
/// highlight, a step down starts at the first row and a step up does nothing.
#[must_use]
pub fn next_pickable(
    current: Option<usize>,
    step: isize,
    count: usize,
    pickable: impl Fn(usize) -> bool,
) -> Option<usize> {
    let mut row = match current {
        Some(row) => row.checked_add_signed(step)?,
        None if step > 0 => 0,
        None => return None,
    };
    while row < count {
        if pickable(row) {
            return Some(row);
        }
        row = row.checked_add_signed(step)?;
    }
    None
}

/// The highlight after a result list of `count` rows changed under it: the previous row while
/// it still exists, else row 1 once there is one, else none. Find in Repository's rows are a
/// file then its lines, so row 1 is the first match.
#[must_use]
pub fn kept_highlight(previous: Option<usize>, count: usize) -> Option<usize> {
    match previous {
        Some(row) if row < count => Some(row),
        Some(_) | None if count > 1 => Some(1),
        Some(_) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_clamps_to_the_list() {
        assert_eq!(moved(None, 1, 0), None);
        assert_eq!(moved(Some(1), 1, 0), None);
        assert_eq!(moved(None, 1, 3), Some(0));
        assert_eq!(moved(None, -1, 3), Some(0));
        assert_eq!(moved(Some(0), -1, 3), Some(0));
        assert_eq!(moved(Some(1), 1, 3), Some(2));
        assert_eq!(moved(Some(2), 1, 3), Some(2));
        assert_eq!(
            moved(Some(7), -1, 3),
            Some(2),
            "a stale highlight lands on the last"
        );
    }

    #[test]
    fn next_pickable_steps_over_headers() {
        let pickable = |row: usize| row != 0 && row != 3;
        assert_eq!(next_pickable(None, 1, 5, pickable), Some(1));
        assert_eq!(next_pickable(None, -1, 5, pickable), None);
        assert_eq!(next_pickable(Some(2), 1, 5, pickable), Some(4));
        assert_eq!(next_pickable(Some(4), -1, 5, pickable), Some(2));
        assert_eq!(
            next_pickable(Some(1), -1, 5, pickable),
            None,
            "no row above"
        );
        assert_eq!(next_pickable(Some(4), 1, 5, pickable), None, "no row below");
        assert_eq!(next_pickable(None, 1, 0, pickable), None);
    }

    #[test]
    fn kept_highlight_holds_a_row_that_still_exists() {
        assert_eq!(kept_highlight(Some(3), 5), Some(3));
        assert_eq!(kept_highlight(Some(5), 5), Some(1));
        assert_eq!(kept_highlight(None, 5), Some(1));
        assert_eq!(kept_highlight(None, 1), None);
        assert_eq!(kept_highlight(Some(2), 1), None);
        assert_eq!(kept_highlight(None, 0), None);
    }
}
