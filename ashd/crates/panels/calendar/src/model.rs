//! Calendar arithmetic with no GPUI: the 6×7 Monday-first grid, ISO week numbers, keyboard
//! movement, English names (locale-independent, as in the mockup).

use chrono::{Datelike as _, Duration, NaiveDate};

pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];

/// Header cells of the grid, Monday first.
pub const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

const DAYS_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// `November 2026`.
pub fn month_title(date: NaiveDate) -> String {
    format!("{} {}", MONTHS[date.month0() as usize], date.year())
}

/// `Fri 13 Nov`.
pub fn day_label(date: NaiveDate) -> String {
    format!(
        "{} {} {}",
        DAYS_SHORT[date.weekday().num_days_from_monday() as usize],
        date.day(),
        &MONTHS[date.month0() as usize][..3]
    )
}

pub fn first_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("day 1 exists")
}

/// The Monday on or before the first of `date`'s month: the top-left cell of the grid.
pub fn grid_start(date: NaiveDate) -> NaiveDate {
    let first = first_of_month(date);
    first - Duration::days(first.weekday().num_days_from_monday() as i64)
}

/// 6 weeks × 7 days.
pub const GRID_WEEKS: usize = 6;

pub fn grid_end(date: NaiveDate) -> NaiveDate {
    grid_start(date) + Duration::days((GRID_WEEKS * 7 - 1) as i64)
}

/// ISO week number of a grid row (the week of its Monday).
pub fn iso_week(row_start: NaiveDate) -> u32 {
    row_start.iso_week().week()
}

/// `date` shifted by whole months, the day clamped to the target month's length.
pub fn add_months(date: NaiveDate, months: i32) -> NaiveDate {
    let total = date.year() * 12 + date.month0() as i32 + months;
    let (year, month) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let mut day = date.day();
    loop {
        if let Some(d) = NaiveDate::from_ymd_opt(year, month, day) {
            return d;
        }
        day -= 1;
    }
}

/// What a navigation key does to the selected day (`None`: not a navigation key).
pub fn navigate(selected: NaiveDate, today: NaiveDate, key: &str, shift: bool) -> Option<NaiveDate> {
    let step = |days: i64| selected.checked_add_signed(Duration::days(days));
    match key {
        "left" => step(-1),
        "right" => step(1),
        "up" => step(-7),
        "down" => step(7),
        "pageup" => Some(add_months(selected, if shift { -12 } else { -1 })),
        "pagedown" => Some(add_months(selected, if shift { 12 } else { 1 })),
        "home" => Some(today),
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn titles_and_labels_are_english() {
        assert_eq!(month_title(d(2026, 11, 13)), "November 2026");
        assert_eq!(day_label(d(2026, 11, 13)), "Fri 13 Nov");
        assert_eq!(day_label(d(2026, 3, 2)), "Mon 2 Mar");
    }

    #[test]
    fn grid_starts_on_monday() {
        // 1 Nov 2026 is a Sunday: the grid starts on Mon 26 Oct (the mockup's week 44).
        assert_eq!(grid_start(d(2026, 11, 13)), d(2026, 10, 26));
        assert_eq!(iso_week(d(2026, 10, 26)), 44);
        assert_eq!(iso_week(d(2026, 11, 2)), 45);
        // A month starting on Monday starts the grid on the 1st.
        assert_eq!(grid_start(d(2026, 6, 20)), d(2026, 6, 1));
        assert_eq!(grid_end(d(2026, 11, 13)), d(2026, 12, 6));
    }

    #[test]
    fn grid_always_covers_the_month() {
        for month in 1..=12 {
            let any = d(2026, month, 15);
            let last = add_months(first_of_month(any), 1) - Duration::days(1);
            assert!(grid_start(any) <= first_of_month(any));
            assert!(grid_end(any) >= last, "month {month}");
        }
    }

    #[test]
    fn iso_week_at_year_edges() {
        assert_eq!(iso_week(d(2024, 12, 30)), 1);
        assert_eq!(iso_week(d(2021, 1, 4)), 1);
        assert_eq!(iso_week(d(2020, 12, 28)), 53);
    }

    #[test]
    fn months_clamp_the_day() {
        assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(add_months(d(2024, 1, 31), 1), d(2024, 2, 29));
        assert_eq!(add_months(d(2026, 3, 31), -1), d(2026, 2, 28));
        assert_eq!(add_months(d(2026, 12, 15), 1), d(2027, 1, 15));
        assert_eq!(add_months(d(2026, 1, 15), -1), d(2025, 12, 15));
        assert_eq!(add_months(d(2026, 5, 15), 12), d(2027, 5, 15));
    }

    #[test]
    fn keys_move_the_selection() {
        let today = d(2026, 11, 13);
        let sel = d(2026, 11, 30);
        assert_eq!(navigate(sel, today, "right", false), Some(d(2026, 12, 1)));
        assert_eq!(navigate(sel, today, "left", false), Some(d(2026, 11, 29)));
        assert_eq!(navigate(sel, today, "down", false), Some(d(2026, 12, 7)));
        assert_eq!(navigate(sel, today, "up", false), Some(d(2026, 11, 23)));
        assert_eq!(navigate(sel, today, "pagedown", false), Some(d(2026, 12, 30)));
        assert_eq!(navigate(sel, today, "pageup", false), Some(d(2026, 10, 30)));
        assert_eq!(navigate(sel, today, "pagedown", true), Some(d(2027, 11, 30)));
        assert_eq!(navigate(sel, today, "home", false), Some(today));
        assert_eq!(navigate(sel, today, "x", false), None);
    }
}
