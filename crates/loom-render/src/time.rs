//! Tiny UTC formatter for log filenames.
//!
//! Loom does not depend on `chrono` or `time`. The only formatting need in
//! this module is a sortable timestamp suffix for per-bead JSONL log files
//! (`<bead-id>-<utc>.jsonl`); the algorithm here is the classic Howard
//! Hinnant `civil_from_days` plus a wall-clock seconds split.
//!
//! Output shape: ISO 8601 *basic* format `YYYYMMDDTHHMMSSZ`. No separators,
//! filename-safe on every filesystem, lexicographically sortable.

use std::time::{SystemTime, UNIX_EPOCH};

/// Format a `SystemTime` as a UTC `YYYYMMDDTHHMMSSZ` string.
///
/// Times before the unix epoch are saturated to `19700101T000000Z`. The
/// function never panics — `SystemTime` arithmetic that would underflow falls
/// back to the epoch.
///
/// ```
/// use loom_render::format_utc_timestamp;
/// use std::time::{Duration, UNIX_EPOCH};
///
/// // 2026-05-03 12:30:45 UTC = 1777811445 seconds since epoch.
/// let t = UNIX_EPOCH + Duration::from_secs(1777811445);
/// assert_eq!(format_utc_timestamp(t), "20260503T123045Z");
/// ```
pub fn format_utc_timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |duration| {
        i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
    });
    let (year, month, day, hour, minute, second) = utc_parts(secs);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Decompose seconds-since-epoch into `(year, month, day, hour, minute,
/// second)` UTC components.
const fn utc_parts(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute, second)
}

/// Convert days since 1970-01-01 to a proleptic Gregorian (year, month, day).
///
/// Algorithm by Howard Hinnant
/// (<https://howardhinnant.github.io/date_algorithms.html#civil_from_days>),
/// shifted from a 0000-03-01 era origin to 1970-01-01.
const fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_is_19700101() {
        assert_eq!(format_utc_timestamp(UNIX_EPOCH), "19700101T000000Z");
    }

    #[test]
    fn known_timestamp_2026_05_03() {
        // 2026-05-03 12:30:45 UTC.
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_777_811_445);
        assert_eq!(format_utc_timestamp(t), "20260503T123045Z");
    }

    #[test]
    fn leap_day_2024_02_29() {
        // 2024-02-29 00:00:00 UTC = 1709164800.
        let t = UNIX_EPOCH + std::time::Duration::from_hours(474_768);
        assert_eq!(format_utc_timestamp(t), "20240229T000000Z");
    }

    #[test]
    fn end_of_year_rollover() {
        // 2025-12-31 23:59:59 UTC = 1767225599.
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_599);
        assert_eq!(format_utc_timestamp(t), "20251231T235959Z");
    }

    #[test]
    fn pre_epoch_saturates_to_epoch() {
        let t = UNIX_EPOCH - std::time::Duration::from_mins(1);
        assert_eq!(format_utc_timestamp(t), "19700101T000000Z");
    }

    #[test]
    fn timestamps_lex_sort_in_chronological_order() {
        let earlier =
            format_utc_timestamp(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000));
        let later = format_utc_timestamp(UNIX_EPOCH + std::time::Duration::from_hours(500_000));
        assert!(earlier < later);
    }
}
