//! Report period boundaries.
//!
//! Every period is the half-open interval `[start, end)` of a local wall-clock
//! span, converted to UTC for comparison against stored event timestamps.

use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};

use super::Period;

/// Converts a local date at midnight to UTC.
/// Handles DST ambiguity by picking the earlier time.
pub fn local_midnight_to_utc(local_date: NaiveDate) -> DateTime<Utc> {
    let midnight = local_date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    match Local.from_local_datetime(&midnight) {
        // Single or ambiguous (DST fall-back): use the earlier time
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        LocalResult::None => {
            // DST spring-forward gap at midnight is rare but possible
            // Use 1am local which is guaranteed to exist
            let one_am = local_date.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap());
            Local
                .from_local_datetime(&one_am)
                .unwrap()
                .with_timezone(&Utc)
        }
    }
}

/// Calculates week boundaries (Mon 00:00 to next Mon 00:00 local time) as half-open interval.
fn week_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let days_since_monday = today.weekday().num_days_from_monday();
    let monday = today - chrono::Duration::days(i64::from(days_since_monday));
    let next_monday = monday + chrono::Duration::days(7);

    let start = local_midnight_to_utc(monday);
    let end = local_midnight_to_utc(next_monday);
    (start, end)
}

/// Calculates last week boundaries (previous Mon 00:00 to this Mon 00:00 local time).
fn last_week_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let days_since_monday = today.weekday().num_days_from_monday();
    let this_monday = today - chrono::Duration::days(i64::from(days_since_monday));
    let last_monday = this_monday - chrono::Duration::days(7);

    let start = local_midnight_to_utc(last_monday);
    let end = local_midnight_to_utc(this_monday);
    (start, end)
}

/// Calculates day boundaries (today 00:00 to tomorrow 00:00 local time).
fn day_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let tomorrow = today + chrono::Duration::days(1);

    let start = local_midnight_to_utc(today);
    let end = local_midnight_to_utc(tomorrow);
    (start, end)
}

/// Calculates yesterday boundaries (yesterday 00:00 to today 00:00 local time).
fn last_day_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let yesterday = today - chrono::Duration::days(1);

    let start = local_midnight_to_utc(yesterday);
    let end = local_midnight_to_utc(today);
    (start, end)
}

/// Get boundaries for a given period, using the provided date as reference.
pub fn get_period_boundaries(period: Period, today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    match period {
        Period::Week => week_boundaries(today),
        Period::LastWeek => last_week_boundaries(today),
        Period::Day => day_boundaries(today),
        Period::LastDay => last_day_boundaries(today),
        Period::Custom(start, end) => (start, end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_week_boundaries_for_known_date() {
        // Jan 29, 2025 is a Wednesday
        let wednesday = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = week_boundaries(wednesday);

        // Week should be Jan 27 (Mon) to Feb 3 (Mon) in local time
        // Convert back to local to verify dates
        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 2, 3).unwrap());
    }

    #[test]
    fn test_week_boundaries_on_monday() {
        // Jan 27, 2025 is a Monday
        let monday = NaiveDate::from_ymd_opt(2025, 1, 27).unwrap();
        let (start, end) = week_boundaries(monday);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 2, 3).unwrap());
    }

    #[test]
    fn test_week_boundaries_on_sunday() {
        // Feb 2, 2025 is a Sunday
        let sunday = NaiveDate::from_ymd_opt(2025, 2, 2).unwrap();
        let (start, end) = week_boundaries(sunday);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 2, 3).unwrap());
    }

    #[test]
    fn test_last_week_boundaries_for_known_date() {
        // Jan 29, 2025 is a Wednesday
        let wednesday = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = last_week_boundaries(wednesday);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        // Last week should be Jan 20 (Mon) to Jan 27 (Mon)
        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 20).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
    }

    #[test]
    fn test_day_boundaries_for_known_date() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = day_boundaries(date);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 29).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 1, 30).unwrap());
    }

    #[test]
    fn test_last_day_boundaries_for_known_date() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = last_day_boundaries(date);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 28).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 1, 29).unwrap());
    }
}
