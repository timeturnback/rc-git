//! The dates History and the blame window show (`HistoryViewController.swift` and
//! `BlamePanelController.swift` used `DateFormatter` with the medium date style), and the
//! `YYYY-MM-DD` day fields of the History date filter (`NSDatePicker` in Swift).
//!
//! This crate takes no calendar or time-zone dependency. Every function that reads a local day
//! takes the zone as an offset in seconds east of UTC, or as `offset_at`, the offset in effect
//! at a given instant; the app passes the system zone (`rc_git_mac::local_utc_offset`), tests
//! a fixed one. Dates are proleptic Gregorian, as `Calendar(identifier: .gregorian)` reads them.

const DAY: i64 = 86_400;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilDate {
    /// The year, astronomical numbering.
    pub year: i64,
    /// The month, 1 to 12.
    pub month: u32,
    /// The day of the month, 1 to 31.
    pub day: u32,
}

/// The date `days` days after 1970-01-01 (Howard Hinnant's `civil_from_days`).
#[must_use]
pub fn civil_from_days(days: i64) -> CivilDate {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    CivilDate {
        year,
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
    }
}

/// Days from 1970-01-01 to `date` (Howard Hinnant's `days_from_civil`).
#[must_use]
pub fn days_from_civil(date: CivilDate) -> i64 {
    let month = i64::from(date.month);
    let year = if month <= 2 { date.year - 1 } else { date.year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(date.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The local calendar day `instant` falls on, as days since the epoch, in a zone `offset`
/// seconds east of UTC.
#[must_use]
pub fn local_day(instant: i64, offset: i32) -> i64 {
    (instant + i64::from(offset)).div_euclid(DAY)
}

/// The medium date: `Nov 11, 2026`.
#[must_use]
pub fn format_date(instant: i64, offset: i32) -> String {
    let date = civil_from_days(local_day(instant, offset));
    let month = MONTHS
        .get(usize::try_from(date.month.saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or("");
    format!("{month} {}, {}", date.day, date.year)
}

/// The medium date and short time: `Nov 11, 2026 at 3:04 PM`.
#[must_use]
pub fn format_date_time(instant: i64, offset: i32) -> String {
    let seconds = (instant + i64::from(offset)).rem_euclid(DAY);
    let hour = seconds / 3_600;
    let minute = seconds % 3_600 / 60;
    let (twelve, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!(
        "{} at {twelve}:{minute:02} {half}",
        format_date(instant, offset)
    )
}

/// A commit list's date and time, 24-hour, in the date filter's day format:
/// `2026-11-11 15:04`.
#[must_use]
pub fn format_list_date_time(instant: i64, offset: i32) -> String {
    let seconds = (instant + i64::from(offset)).rem_euclid(DAY);
    format!(
        "{} {:02}:{:02}",
        format_day(local_day(instant, offset)),
        seconds / 3_600,
        seconds % 3_600 / 60
    )
}

/// [`format_list_date_time`]'s output with every digit replaced by each digit in turn. Every
/// list date has that shape, so the widest of these, as shaped, is the widest a list column
/// has to fit.
#[must_use]
pub fn list_date_time_samples() -> Vec<String> {
    let shape = format_list_date_time(0, 0);
    let mut samples = Vec::with_capacity(10);
    for digit in '0'..='9' {
        let mut sample = String::with_capacity(shape.len());
        for character in shape.chars() {
            sample.push(if character.is_ascii_digit() {
                digit
            } else {
                character
            });
        }
        samples.push(sample);
    }
    samples
}

/// A day as the date filter's field shows it: `2026-11-11`.
#[must_use]
pub fn format_day(day: i64) -> String {
    let date = civil_from_days(day);
    format!("{:04}-{:02}-{:02}", date.year, date.month, date.day)
}

/// The day a date filter field names, as days since the epoch: `YYYY-MM-DD`, surrounding
/// spaces ignored. `None` for anything else, including a date the calendar lacks
/// (`2026-02-30`), so the field can say it is invalid rather than filter on a guess.
#[must_use]
pub fn parse_day(text: &str) -> Option<i64> {
    let mut parts = text.trim().split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let digits = |part: &str, length: usize| {
        (part.len() == length && part.bytes().all(|byte| byte.is_ascii_digit())).then_some(())
    };
    digits(year, 4)?;
    digits(month, 2)?;
    digits(day, 2)?;
    let date = CivilDate {
        year: year.parse().ok()?,
        month: month.parse().ok()?,
        day: day.parse().ok()?,
    };
    if !(1..=12).contains(&date.month) || date.day == 0 {
        return None;
    }
    let days = days_from_civil(date);
    (civil_from_days(days) == date).then_some(days)
}

/// An instant inside local day `day`: its noon, which every zone's offset (at most 14 hours
/// either way) keeps on the same day, so `day_bounds` reads it back as `day`.
#[must_use]
pub fn instant_in_day(day: i64, offset_at: impl Fn(i64) -> i32) -> i64 {
    let noon = day * DAY + DAY / 2;
    noon - i64::from(offset_at(noon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_date_is_the_day_and_a_24_hour_time() {
        let instant = 1_794_409_445;
        assert_eq!(format_list_date_time(instant, 0), "2026-11-11 15:04");
        assert_eq!(
            format_list_date_time(instant, 9 * 3_600),
            "2026-11-12 00:04"
        );
        assert_eq!(
            format_list_date_time(instant, -15 * 3_600),
            "2026-11-11 00:04"
        );
        assert_eq!(format_list_date_time(0, 0), "1970-01-01 00:00");
        assert_eq!(format_list_date_time(-1, 0), "1969-12-31 23:59");
    }

    #[test]
    fn the_list_date_samples_cover_every_digit_in_the_list_shape() {
        let samples = list_date_time_samples();
        assert_eq!(samples.len(), 10);
        assert_eq!(samples[0], "0000-00-00 00:00");
        assert_eq!(samples[9], "9999-99-99 99:99");
        let shape = format_list_date_time(0, 0);
        for sample in &samples {
            assert_eq!(sample.len(), shape.len(), "{sample}");
        }
    }

    #[test]
    fn days_round_trip_through_the_calendar() {
        for days in [-719_468, -1, 0, 1, 59, 60, 11_016, 20_768, 2_932_896] {
            assert_eq!(days_from_civil(civil_from_days(days)), days, "{days}");
        }
        assert_eq!(
            civil_from_days(0),
            CivilDate {
                year: 1970,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            civil_from_days(11_016),
            CivilDate {
                year: 2000,
                month: 2,
                day: 29
            }
        );
    }

    #[test]
    fn the_medium_date_reads_like_the_swift_formatter() {
        // 2026-11-11 15:04:05 UTC.
        let instant = 1_794_409_445;
        assert_eq!(format_date(instant, 0), "Nov 11, 2026");
        assert_eq!(format_date_time(instant, 0), "Nov 11, 2026 at 3:04 PM");
        assert_eq!(
            format_date_time(instant, 9 * 3_600),
            "Nov 12, 2026 at 12:04 AM"
        );
        assert_eq!(
            format_date_time(instant, -15 * 3_600),
            "Nov 11, 2026 at 12:04 AM"
        );
        assert_eq!(
            format_date_time(instant + 21 * 3_600 - 5, 0),
            "Nov 12, 2026 at 12:04 PM"
        );
        assert_eq!(format_date_time(0, 0), "Jan 1, 1970 at 12:00 AM");
        assert_eq!(format_date_time(-1, 0), "Dec 31, 1969 at 11:59 PM");
    }

    #[test]
    fn a_day_field_round_trips_and_refuses_what_the_calendar_lacks() {
        let day = parse_day(" 2026-11-11 ");
        assert_eq!(day, Some(20_768));
        assert_eq!(format_day(20_768), "2026-11-11");
        assert_eq!(parse_day("2024-02-29"), Some(19_782));
        for bad in [
            "",
            "2026-02-30",
            "2025-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-11-00",
            "2026-1-11",
            "26-11-11",
            "2026-11-11-1",
            "2026/11/11",
            "２０２６-11-11",
            "+026-11-11",
        ] {
            assert_eq!(parse_day(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_instant_in_a_day_falls_on_that_day_in_every_zone() {
        for offset in [-14 * 3_600, -5 * 3_600, 0, 5 * 3_600 + 1_800, 14 * 3_600] {
            let instant = instant_in_day(20_768, |_| offset);
            assert_eq!(local_day(instant, offset), 20_768, "offset {offset}");
        }
    }
}
