//! A small RFC 5545 reader: `VEVENT` with `DTSTART` / `DTEND` / `DURATION` / `SUMMARY` /
//! `EXDATE` and the simple `RRULE`s (`FREQ=DAILY|WEEKLY|MONTHLY|YEARLY` with `INTERVAL`,
//! `COUNT`, `UNTIL` and, for weekly rules, `BYDAY`).
//!
//! Times are kept as *local* wall-clock values: a trailing `Z` is converted to the local
//! zone, a `TZID=` parameter is taken as local (there is no tz database here). No I/O in
//! this module except [`load_dir`]; the panel calls it off the main thread.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use chrono::{Datelike as _, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _, Utc, Weekday};

/// Hard cap on the instances examined per recurring event (a daily event from year 2000
/// is ~10k).
const MAX_INSTANCES: usize = 50_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<NaiveDateTime>,
    /// Weekly rules: the weekdays (empty = the weekday of `DTSTART`).
    pub by_day: Vec<Weekday>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub summary: String,
    pub start: NaiveDateTime,
    /// Exclusive end (all-day events: the day after the last day).
    pub end: NaiveDateTime,
    pub all_day: bool,
    pub rule: Option<Rule>,
    pub exdates: Vec<NaiveDate>,
}

/// One concrete occurrence of an event.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub summary: String,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
}

impl Occurrence {
    /// Whether this occurrence touches `day`.
    pub fn on(&self, day: NaiveDate) -> bool {
        let first = self.start.date();
        // An end exactly at midnight does not touch that day; all-day ends are exclusive too.
        let last = if self.end > self.start {
            (self.end - Duration::nanoseconds(1)).date()
        } else {
            first
        };
        first <= day && day <= last
    }

    /// `14:30–15:30`, `14:30` (no end), `all day`, or the continuation `until 12:00` /
    /// `from 09:00` markers for events spanning several days, as seen on `day`.
    pub fn time_label(&self, day: NaiveDate) -> String {
        if self.all_day {
            return "all day".into();
        }
        let starts_today = self.start.date() == day;
        let ends_today = (self.end - Duration::nanoseconds(1)).date() == day || self.end <= self.start;
        let hm = |t: NaiveDateTime| t.format("%H:%M").to_string();
        match (starts_today, ends_today) {
            (true, true) if self.end > self.start => format!("{}–{}", hm(self.start), hm(self.end)),
            (true, true) => hm(self.start),
            (true, false) => format!("from {}", hm(self.start)),
            (false, true) => format!("until {}", hm(self.end)),
            (false, false) => "all day".into(),
        }
    }
}

// ── parsing ───────────────────────────────────────────────────────────────────

/// Unfold continuation lines (a line starting with space or tab continues the previous one).
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        lines.push(raw.to_string());
    }
    lines
}

/// `NAME;P=V;P=V:value` → (NAME upper-case, params, value).
fn split_line(line: &str) -> Option<(String, Vec<(String, String)>, &str)> {
    // The first ':' outside double quotes ends the name+params part.
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        .collect();
    Some((name, params, value))
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A `DTSTART`-style value: `(local datetime, all_day)`.
fn parse_when(value: &str, params: &[(String, String)]) -> Option<(NaiveDateTime, bool)> {
    let value = value.trim();
    let date_only = params.iter().any(|(k, v)| k == "VALUE" && v.eq_ignore_ascii_case("DATE")) || value.len() == 8;
    if date_only {
        let d = NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d").ok()?;
        return Some((d.and_time(NaiveTime::MIN), true));
    }
    let (body, utc) = match value.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (value, false),
    };
    let naive = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M"))
        .ok()?;
    if utc {
        let local = Utc.from_utc_datetime(&naive).with_timezone(&Local).naive_local();
        Some((local, false))
    } else {
        Some((naive, false))
    }
}

/// `P1D`, `PT1H30M`, `P1W`, `-PT15M` → a duration.
fn parse_duration(value: &str) -> Option<Duration> {
    let v = value.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut in_time = false;
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (unit, in_time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

fn parse_weekday(code: &str) -> Option<Weekday> {
    // BYDAY may carry an ordinal prefix (`1MO`, `-1FR`): only the plain day is supported.
    let code = code.trim_start_matches(['+', '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9']);
    Some(match code.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_rule(value: &str) -> Option<Rule> {
    let mut freq = None;
    let mut interval = 1;
    let mut count = None;
    let mut until = None;
    let mut by_day = Vec::new();
    for part in value.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = match v.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => Some(Freq::Daily),
                    "WEEKLY" => Some(Freq::Weekly),
                    "MONTHLY" => Some(Freq::Monthly),
                    "YEARLY" => Some(Freq::Yearly),
                    _ => None,
                }
            }
            "INTERVAL" => interval = v.trim().parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(1),
            "COUNT" => count = v.trim().parse().ok(),
            "UNTIL" => until = parse_when(v, &[]).map(|(t, all_day)| if all_day { t + Duration::days(1) - Duration::nanoseconds(1) } else { t }),
            "BYDAY" => by_day = v.split(',').filter_map(parse_weekday).collect(),
            _ => {}
        }
    }
    Some(Rule { freq: freq?, interval, count, until, by_day })
}

/// Every `VEVENT` of an `.ics` text. Malformed events are skipped.
pub fn parse(text: &str) -> Vec<Event> {
    struct Draft {
        summary: String,
        start: Option<(NaiveDateTime, bool)>,
        end: Option<(NaiveDateTime, bool)>,
        duration: Option<Duration>,
        rule: Option<Rule>,
        exdates: Vec<NaiveDate>,
        cancelled: bool,
    }
    let mut events = Vec::new();
    let mut draft: Option<Draft> = None;
    let mut depth = 0usize; // nested components (VALARM) inside a VEVENT
    for line in unfold(text) {
        let Some((name, params, value)) = split_line(&line) else { continue };
        match (name.as_str(), draft.as_mut()) {
            ("BEGIN", None) if value.eq_ignore_ascii_case("VEVENT") => {
                draft = Some(Draft {
                    summary: String::new(),
                    start: None,
                    end: None,
                    duration: None,
                    rule: None,
                    exdates: Vec::new(),
                    cancelled: false,
                });
                depth = 0;
            }
            ("BEGIN", Some(_)) => depth += 1,
            ("END", Some(_)) if depth > 0 => depth -= 1,
            ("END", Some(_)) if value.eq_ignore_ascii_case("VEVENT") => {
                let d = draft.take().expect("checked Some");
                let Some((start, all_day)) = d.start else { continue };
                if d.cancelled {
                    continue;
                }
                let end = match (d.end, d.duration) {
                    (Some((end, _)), _) if end >= start => end,
                    (_, Some(len)) => start + len,
                    _ if all_day => start + Duration::days(1),
                    _ => start,
                };
                events.push(Event {
                    summary: if d.summary.trim().is_empty() { "(no title)".into() } else { d.summary },
                    start,
                    end,
                    all_day,
                    rule: d.rule,
                    exdates: d.exdates,
                });
            }
            (_, Some(d)) if depth == 0 => match name.as_str() {
                "SUMMARY" => d.summary = unescape(value),
                "DTSTART" => d.start = parse_when(value, &params),
                "DTEND" => d.end = parse_when(value, &params),
                "DURATION" => d.duration = parse_duration(value),
                "RRULE" => d.rule = parse_rule(value),
                "EXDATE" => d.exdates.extend(value.split(',').filter_map(|v| parse_when(v, &params)).map(|(t, _)| t.date())),
                "STATUS" => d.cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
                _ => {}
            },
            _ => {}
        }
    }
    events
}

// ── recurrence ────────────────────────────────────────────────────────────────

fn add_months(date: NaiveDate, months: i32) -> Option<NaiveDate> {
    let total = date.year() * 12 + date.month0() as i32 + months;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
}

/// Start dates of the instances of `event`, in order, ending at `limit` (inclusive).
fn instance_dates(event: &Event, limit: NaiveDate) -> Vec<NaiveDate> {
    let first = event.start.date();
    let Some(rule) = &event.rule else { return vec![first] };
    let mut out = Vec::new();
    let max_count = rule.count.map(|c| c as usize).unwrap_or(usize::MAX);
    let until = rule.until;
    let time = event.start.time();
    let emit = |d: NaiveDate, out: &mut Vec<NaiveDate>| -> bool {
        if d < first {
            return true;
        }
        if d > limit || until.is_some_and(|u| d.and_time(time) > u) || out.len() >= max_count || out.len() >= MAX_INSTANCES {
            return false;
        }
        out.push(d);
        true
    };
    let step = rule.interval as i64;
    match rule.freq {
        Freq::Daily => {
            let mut d = first;
            while emit(d, &mut out) {
                d += Duration::days(step);
            }
        }
        Freq::Weekly => {
            let days: Vec<Weekday> = if rule.by_day.is_empty() { vec![first.weekday()] } else { rule.by_day.clone() };
            let monday = first - Duration::days(first.weekday().num_days_from_monday() as i64);
            let mut week = 0i64;
            'weeks: loop {
                let base = monday + Duration::weeks(week * step);
                let mut in_week: Vec<NaiveDate> =
                    days.iter().map(|w| base + Duration::days(w.num_days_from_monday() as i64)).collect();
                in_week.sort();
                in_week.dedup();
                for d in in_week {
                    if !emit(d, &mut out) {
                        // `emit` rejects early dates with `true`, so `false` always ends the walk.
                        break 'weeks;
                    }
                }
                week += 1;
                if base > limit {
                    break;
                }
            }
        }
        Freq::Monthly => {
            let mut n = 0i32;
            while let Some(month_start) = add_months(first.with_day(1).unwrap_or(first), n * step as i32) {
                if month_start > limit {
                    break;
                }
                // Months without that day (the 31st) are skipped, as RFC 5545 says.
                if let Some(d) = month_start.with_day(first.day())
                    && !emit(d, &mut out)
                {
                    break;
                }
                n += 1;
                if n as usize > MAX_INSTANCES {
                    break;
                }
            }
        }
        Freq::Yearly => {
            let mut n = 0i32;
            loop {
                let year = first.year() + n * step as i32;
                if year > limit.year() {
                    break;
                }
                if let Some(d) = NaiveDate::from_ymd_opt(year, first.month(), first.day())
                    && !emit(d, &mut out)
                {
                    break;
                }
                n += 1;
                if n as usize > MAX_INSTANCES {
                    break;
                }
            }
        }
    }
    out
}

/// All occurrences that touch `[from, to]` (inclusive days), sorted by start.
pub fn expand(events: &[Event], from: NaiveDate, to: NaiveDate) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for ev in events {
        let len = ev.end - ev.start;
        let exdates: HashSet<NaiveDate> = ev.exdates.iter().copied().collect();
        for date in instance_dates(ev, to) {
            if exdates.contains(&date) {
                continue;
            }
            let start = date.and_time(ev.start.time());
            let occ = Occurrence { summary: ev.summary.clone(), start, end: start + len, all_day: ev.all_day };
            let last = if occ.end > occ.start { (occ.end - Duration::nanoseconds(1)).date() } else { date };
            if last >= from && date <= to {
                out.push(occ);
            }
        }
    }
    out.sort_by(|a, b| (!a.all_day, a.start, &a.summary).cmp(&(!b.all_day, b.start, &b.summary)));
    out
}

// ── files ─────────────────────────────────────────────────────────────────────

/// The directories searched for calendars: `$XDG_DATA_HOME/ashd/calendars` (default
/// `~/.local/share/ashd/calendars`) and `~/.calendar`.
pub fn calendar_dirs() -> Vec<PathBuf> {
    let mut dirs_ = Vec::new();
    if let Some(data) = dirs::data_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/share"))) {
        dirs_.push(data.join("ashd/calendars"));
    }
    if let Some(home) = dirs::home_dir() {
        dirs_.push(home.join(".calendar"));
    }
    dirs_
}

/// What was found on disk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calendars {
    pub events: Vec<Event>,
    /// Number of `.ics` files read.
    pub files: usize,
}

/// Read every `*.ics` directly inside `dirs` (missing directories are fine).
pub fn load_dirs(dirs: &[PathBuf]) -> Calendars {
    let mut out = Calendars::default();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ics")))
            .collect();
        paths.sort();
        for path in paths {
            match read_file(&path) {
                Some(events) => {
                    out.files += 1;
                    out.events.extend(events);
                }
                None => tracing::warn!("calendar: cannot read {}", path.display()),
            }
        }
    }
    out
}

fn read_file(path: &Path) -> Option<Vec<Event>> {
    let bytes = std::fs::read(path).ok()?;
    Some(parse(&String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn wrap(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")
    }

    #[test]
    fn plain_event_with_times() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261113T143000\r\nDTEND:20261113T153000\r\nSUMMARY:Standup\\, daily\r\nEND:VEVENT\r\n",
        ));
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].summary, "Standup, daily");
        assert!(!evs[0].all_day);
        let occ = expand(&evs, d(2026, 11, 1), d(2026, 11, 30));
        assert_eq!(occ.len(), 1);
        assert!(occ[0].on(d(2026, 11, 13)));
        assert!(!occ[0].on(d(2026, 11, 14)));
        assert_eq!(occ[0].time_label(d(2026, 11, 13)), "14:30–15:30");
    }

    #[test]
    fn all_day_and_multi_day() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261110\nDTEND;VALUE=DATE:20261113\nSUMMARY:Trip\nEND:VEVENT\n\
             BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261120\nSUMMARY:Birthday\nEND:VEVENT\n",
        ));
        let occ = expand(&evs, d(2026, 11, 1), d(2026, 11, 30));
        let trip = occ.iter().find(|o| o.summary == "Trip").unwrap();
        assert!(trip.all_day);
        assert!(trip.on(d(2026, 11, 10)) && trip.on(d(2026, 11, 12)));
        assert!(!trip.on(d(2026, 11, 13)), "DTEND of a date is exclusive");
        let bday = occ.iter().find(|o| o.summary == "Birthday").unwrap();
        assert!(bday.on(d(2026, 11, 20)) && !bday.on(d(2026, 11, 21)));
        assert_eq!(bday.time_label(d(2026, 11, 20)), "all day");
    }

    #[test]
    fn folded_lines_and_alarms() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\r\nDTSTART:20261113T090000\r\nSUMMARY:A very long\r\n  title that wraps\r\nBEGIN:VALARM\r\nSUMMARY:alarm\r\nEND:VALARM\r\nEND:VEVENT\r\n",
        ));
        assert_eq!(evs[0].summary, "A very long title that wraps");
    }

    #[test]
    fn utc_times_become_local() {
        let evs = parse(&wrap("BEGIN:VEVENT\nDTSTART:20261113T120000Z\nSUMMARY:x\nEND:VEVENT\n"));
        let expect = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap().with_timezone(&Local).naive_local();
        assert_eq!(evs[0].start, expect);
    }

    #[test]
    fn duration_instead_of_dtend() {
        let evs = parse(&wrap("BEGIN:VEVENT\nDTSTART:20261113T090000\nDURATION:PT1H30M\nSUMMARY:x\nEND:VEVENT\n"));
        assert_eq!(evs[0].end - evs[0].start, Duration::minutes(90));
        assert_eq!(parse_duration("P1W"), Some(Duration::weeks(1)));
        assert_eq!(parse_duration("-PT15M"), Some(-Duration::minutes(15)));
        assert_eq!(parse_duration("garbage"), None);
    }

    #[test]
    fn cancelled_and_titleless() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\nDTSTART:20261113T090000\nSTATUS:CANCELLED\nSUMMARY:gone\nEND:VEVENT\n\
             BEGIN:VEVENT\nDTSTART:20261114T090000\nEND:VEVENT\n",
        ));
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].summary, "(no title)");
    }

    fn recurring(rule: &str, start: &str) -> Vec<Event> {
        parse(&wrap(&format!("BEGIN:VEVENT\nDTSTART:{start}\nDTEND:{start}\nSUMMARY:r\nRRULE:{rule}\nEND:VEVENT\n")))
    }

    fn days(evs: &[Event], from: NaiveDate, to: NaiveDate) -> Vec<u32> {
        expand(evs, from, to).iter().map(|o| o.start.date().day()).collect()
    }

    #[test]
    fn daily_with_interval_and_count() {
        let evs = recurring("FREQ=DAILY;INTERVAL=2;COUNT=4", "20261101T080000");
        assert_eq!(days(&evs, d(2026, 11, 1), d(2026, 11, 30)), [1, 3, 5, 7]);
    }

    #[test]
    fn weekly_default_and_byday() {
        let evs = recurring("FREQ=WEEKLY", "20261102T080000"); // a Monday
        assert_eq!(days(&evs, d(2026, 11, 1), d(2026, 11, 30)), [2, 9, 16, 23, 30]);
        let evs = recurring("FREQ=WEEKLY;BYDAY=MO,WE;INTERVAL=2", "20261102T080000");
        assert_eq!(days(&evs, d(2026, 11, 1), d(2026, 11, 30)), [2, 4, 16, 18, 30]);
    }

    #[test]
    fn monthly_skips_missing_days_and_yearly() {
        let evs = recurring("FREQ=MONTHLY", "20261031T080000");
        let got: Vec<_> = expand(&evs, d(2026, 10, 1), d(2027, 1, 31)).iter().map(|o| o.start.date()).collect();
        assert_eq!(got, [d(2026, 10, 31), d(2026, 12, 31), d(2027, 1, 31)]);
        let evs = recurring("FREQ=YEARLY", "20240229T080000");
        let got: Vec<_> = expand(&evs, d(2024, 1, 1), d(2029, 1, 1)).iter().map(|o| o.start.date()).collect();
        assert_eq!(got, [d(2024, 2, 29), d(2028, 2, 29)]);
    }

    #[test]
    fn until_and_exdate() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\nDTSTART:20261101T080000\nSUMMARY:r\nRRULE:FREQ=DAILY;UNTIL=20261105T080000\nEXDATE:20261103T080000\nEND:VEVENT\n",
        ));
        assert_eq!(days(&evs, d(2026, 11, 1), d(2026, 11, 30)), [1, 2, 4, 5]);
    }

    #[test]
    fn recurring_before_window_is_not_listed_after_until() {
        let evs = recurring("FREQ=DAILY;COUNT=3", "20260101T080000");
        assert!(expand(&evs, d(2026, 11, 1), d(2026, 11, 30)).is_empty());
    }

    #[test]
    fn multi_day_instance_spills_into_window() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\nDTSTART:20261031T220000\nDTEND:20261101T060000\nSUMMARY:night\nEND:VEVENT\n",
        ));
        let occ = expand(&evs, d(2026, 11, 1), d(2026, 11, 30));
        assert_eq!(occ.len(), 1);
        assert!(occ[0].on(d(2026, 11, 1)));
        assert_eq!(occ[0].time_label(d(2026, 11, 1)), "until 06:00");
        assert_eq!(occ[0].time_label(d(2026, 10, 31)), "from 22:00");
    }

    #[test]
    fn all_day_events_sort_first() {
        let evs = parse(&wrap(
            "BEGIN:VEVENT\nDTSTART:20261113T080000\nSUMMARY:b\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART;VALUE=DATE:20261113\nSUMMARY:a\nEND:VEVENT\n",
        ));
        let occ = expand(&evs, d(2026, 11, 13), d(2026, 11, 13));
        assert_eq!(occ.iter().map(|o| o.summary.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn loads_every_ics_in_the_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ics"), wrap("BEGIN:VEVENT\nDTSTART:20261113T080000\nSUMMARY:a\nEND:VEVENT\n")).unwrap();
        std::fs::write(dir.path().join("b.ICS"), wrap("BEGIN:VEVENT\nDTSTART:20261114T080000\nSUMMARY:b\nEND:VEVENT\n")).unwrap();
        std::fs::write(dir.path().join("note.txt"), "nope").unwrap();
        let cals = load_dirs(&[dir.path().to_path_buf(), dir.path().join("missing")]);
        assert_eq!(cals.files, 2);
        assert_eq!(cals.events.len(), 2);
    }
}
