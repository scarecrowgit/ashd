//! The do-not-disturb schedule: a daily window (`start` – `end`, local time, may cross
//! midnight) during which DND is switched on automatically. Persisted as
//! `<state dir>/dnd-schedule.json`:
//!
//! ```json
//! { "enabled": true, "start": "23:00", "end": "08:00" }
//! ```
//!
//! No GPUI here; the panel reads / writes the file on a background thread and runs the
//! once-a-minute timer.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{Local, Timelike as _};
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "dnd-schedule.json";
/// Time-of-day editing granularity.
pub const STEP_MINUTES: u16 = 30;
const DAY: u16 = 24 * 60;

/// A time of day in minutes since midnight (0..1440).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tod(pub u16);

impl Tod {
    pub fn new(hour: u16, minute: u16) -> Self {
        Self((hour * 60 + minute) % DAY)
    }

    pub fn now() -> Self {
        let t = Local::now();
        Self::new(t.hour() as u16, t.minute() as u16)
    }

    /// `23:00`.
    pub fn label(self) -> String {
        format!("{:02}:{:02}", self.0 / 60, self.0 % 60)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (h, m) = text.trim().split_once(':')?;
        let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
        (h < 24 && m < 60).then(|| Self::new(h, m))
    }

    /// `self` moved by `delta` minutes, wrapping around midnight.
    pub fn shifted(self, delta: i32) -> Self {
        Self((self.0 as i32 + delta).rem_euclid(DAY as i32) as u16)
    }
}

impl Serialize for Tod {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.label())
    }
}

impl<'de> Deserialize<'de> for Tod {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Tod::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("not a HH:MM time: {text:?}")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    pub start: Tod,
    pub end: Tod,
}

impl Default for Schedule {
    fn default() -> Self {
        Self { enabled: false, start: Tod::new(23, 0), end: Tod::new(8, 0) }
    }
}

impl Schedule {
    /// Whether `now` is inside the window. Independent of `enabled`; `start == end` is an
    /// empty window.
    pub fn contains(&self, now: Tod) -> bool {
        let (s, e, n) = (self.start.0, self.end.0, now.0);
        match s.cmp(&e) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => s <= n && n < e,
            std::cmp::Ordering::Greater => n >= s || n < e,
        }
    }

    /// The schedule asks for DND right now.
    pub fn wants_dnd(&self, now: Tod) -> bool {
        self.enabled && self.contains(now)
    }

    /// `23:00 – 08:00`.
    pub fn window_label(&self) -> String {
        format!("{} – {}", self.start.label(), self.end.label())
    }
}

pub fn path() -> PathBuf {
    ashd_tokens::state::state_dir().join(FILE_NAME)
}

/// Read the file; a missing or malformed one gives the default (disabled) schedule.
pub fn load(path: &Path) -> Schedule {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{}: {e}; using the default schedule", path.display());
            Schedule::default()
        }),
        Err(_) => Schedule::default(),
    }
}

pub fn save(path: &Path, schedule: &Schedule) -> io::Result<()> {
    let mut text = serde_json::to_string_pretty(schedule).map_err(io::Error::other)?;
    text.push('\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ashd_tokens::state::write_atomic(path, text.as_bytes())
}

/// Time until the next minute boundary (+20 ms): window edges are whole minutes, so the
/// timer wakes exactly there.
pub fn until_next_minute() -> Duration {
    let now = Local::now();
    let into_minute = now.timestamp_subsec_millis() as u64 + u64::from(now.second()) * 1000;
    Duration::from_millis(60_000 - into_minute.min(59_999) + 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(s: (u16, u16), e: (u16, u16)) -> Schedule {
        Schedule { enabled: true, start: Tod::new(s.0, s.1), end: Tod::new(e.0, e.1) }
    }

    #[test]
    fn daytime_window() {
        let s = sched((9, 0), (17, 30));
        assert!(!s.contains(Tod::new(8, 59)));
        assert!(s.contains(Tod::new(9, 0)));
        assert!(s.contains(Tod::new(17, 29)));
        assert!(!s.contains(Tod::new(17, 30)));
    }

    #[test]
    fn overnight_window_crosses_midnight() {
        let s = sched((23, 0), (8, 0));
        assert!(s.contains(Tod::new(23, 0)));
        assert!(s.contains(Tod::new(0, 0)));
        assert!(s.contains(Tod::new(7, 59)));
        assert!(!s.contains(Tod::new(8, 0)));
        assert!(!s.contains(Tod::new(12, 0)));
        assert!(!s.contains(Tod::new(22, 59)));
    }

    #[test]
    fn equal_edges_are_an_empty_window_and_disabled_never_wants() {
        let s = sched((8, 0), (8, 0));
        assert!(!s.contains(Tod::new(8, 0)));
        let mut s = sched((23, 0), (8, 0));
        assert!(s.wants_dnd(Tod::new(1, 0)));
        s.enabled = false;
        assert!(!s.wants_dnd(Tod::new(1, 0)));
    }

    #[test]
    fn times_parse_print_and_shift() {
        assert_eq!(Tod::parse("23:05"), Some(Tod::new(23, 5)));
        assert_eq!(Tod::parse(" 7:30 "), Some(Tod::new(7, 30)));
        assert_eq!(Tod::parse("24:00"), None);
        assert_eq!(Tod::parse("12:60"), None);
        assert_eq!(Tod::parse("noon"), None);
        assert_eq!(Tod::new(7, 5).label(), "07:05");
        assert_eq!(Tod::new(23, 45).shifted(30), Tod::new(0, 15));
        assert_eq!(Tod::new(0, 10).shifted(-30), Tod::new(23, 40));
        assert_eq!(sched((23, 0), (8, 0)).window_label(), "23:00 – 08:00");
    }

    #[test]
    fn persists_round_trip_and_tolerates_junk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/dnd-schedule.json");
        assert_eq!(load(&file), Schedule::default());
        let s = sched((22, 30), (6, 45));
        save(&file, &s).unwrap();
        assert_eq!(load(&file), s);
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"22:30\""));
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load(&file), Schedule::default());
        std::fs::write(&file, r#"{"enabled": true}"#).unwrap();
        assert_eq!(load(&file), Schedule { enabled: true, ..Schedule::default() });
    }
}
