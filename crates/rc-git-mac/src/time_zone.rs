//! The user's time zone, for the dates History and the blame window show and the day bounds of
//! the History date filter (`Calendar.current` and `DateFormatter` in the Swift app).

use crate::cf::{CFTypeRef, Owned};

/// Seconds from the Unix epoch to `CoreFoundation`'s reference date, 2001-01-01 UTC.
const REFERENCE_DATE: i64 = 978_307_200;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFTimeZoneCopyDefault() -> CFTypeRef;
    fn CFTimeZoneGetSecondsFromGMT(zone: CFTypeRef, at: f64) -> f64;
}

/// The offset from UTC, in seconds east, that the user's time zone has at `instant` (seconds
/// since the Unix epoch), daylight saving included. `0` when the zone cannot be read.
#[must_use]
pub fn local_utc_offset(instant: i64) -> i32 {
    // SAFETY: `CFTimeZoneCopyDefault` takes no arguments and returns an owned, immutable time
    // zone (or null), which `from_create` releases once.
    let Some(zone) = (unsafe { Owned::from_create(CFTimeZoneCopyDefault()) }) else {
        return 0;
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a commit time in seconds is far inside f64's exact integer range"
    )]
    let at = (instant - REFERENCE_DATE) as f64;
    // SAFETY: `zone` is a live time zone for the call; the function only reads it.
    let seconds = unsafe { CFTimeZoneGetSecondsFromGMT(zone.as_ptr(), at) };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a zone offset is whole seconds within a day, far inside i32"
    )]
    let offset = seconds.round() as i32;
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_is_a_real_zone_offset() {
        let offset = local_utc_offset(1_794_409_445);
        assert!(offset.abs() <= 14 * 3_600, "{offset}");
        assert_eq!(offset % 60, 0, "zones offset by whole minutes: {offset}");
    }
}
