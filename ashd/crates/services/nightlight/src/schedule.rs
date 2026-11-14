//! When is the night light on? Modes, fixed windows, sunset-to-sunrise. Pure: the caller
//! passes the current time in whatever time zone it wants (the service uses `Local`).

use std::{fmt, str::FromStr};

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Timelike as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::solar::{SunDay, sun_day};

/// How the night light is driven.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Never tinted.
    Off,
    /// Tinted until switched off, whatever the time.
    On,
    /// Tinted between two times of day (default 21:00-07:00).
    #[default]
    Fixed,
    /// Tinted from sunset to sunrise at the configured latitude / longitude.
    Sun,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Off, Mode::On, Mode::Fixed, Mode::Sun];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::On => "on",
            Mode::Fixed => "fixed",
            Mode::Sun => "sun",
        }
    }

    /// Follows a clock or the sun (so the tile can say "until 07:00").
    pub fn is_scheduled(self) -> bool {
        matches!(self, Mode::Fixed | Mode::Sun)
    }
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Mode::Off),
            "on" | "manual" => Ok(Mode::On),
            "fixed" | "schedule" | "times" => Ok(Mode::Fixed),
            "sun" | "sunset" | "solar" => Ok(Mode::Sun),
            other => Err(format!("unknown night light mode {other:?} (off, on, fixed, sun)")),
        }
    }
}

/// A wall-clock time without a date, minute resolution: `07:00`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    pub const fn new(hour: u8, minute: u8) -> Self {
        Self { hour, minute }
    }

    pub fn of<Tz: TimeZone>(t: &DateTime<Tz>) -> Self {
        Self { hour: t.hour() as u8, minute: t.minute() as u8 }
    }

    fn naive(self) -> NaiveTime {
        NaiveTime::from_hms_opt(u32::from(self.hour), u32::from(self.minute), 0).unwrap_or(NaiveTime::MIN)
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

impl FromStr for TimeOfDay {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (h, m) = s.trim().split_once(':').ok_or_else(|| format!("{s:?} is not HH:MM"))?;
        let (hour, minute): (u8, u8) = (h.parse().map_err(|_| format!("bad hour in {s:?}"))?, m.parse().map_err(|_| format!("bad minute in {s:?}"))?);
        if hour > 23 || minute > 59 {
            return Err(format!("{s:?} is not a time of day"));
        }
        Ok(Self { hour, minute })
    }
}

impl Serialize for TimeOfDay {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TimeOfDay {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Everything the schedule needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schedule {
    pub mode: Mode,
    /// `Fixed`: the tint starts at `start` ...
    pub start: TimeOfDay,
    /// ... and ends at `end` (the window may wrap midnight).
    pub end: TimeOfDay,
    /// `Sun`: degrees north / east. Without both the mode behaves like `Fixed`.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self { mode: Mode::Fixed, start: TimeOfDay::new(21, 0), end: TimeOfDay::new(7, 0), latitude: None, longitude: None }
    }
}

/// The schedule's answer for one instant.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision<Tz: TimeZone> {
    /// Should the tint be on right now?
    pub active: bool,
    /// The next moment `active` flips because of the schedule (`None` for `Off` / `On`,
    /// polar day / night, and an empty window).
    pub next_change: Option<DateTime<Tz>>,
}

/// A local wall-clock time on `date`; times that do not exist (DST gap) move forward.
fn local_instant<Tz: TimeZone>(tz: &Tz, date: NaiveDate, t: NaiveTime) -> Option<DateTime<Tz>> {
    let naive = date.and_time(t);
    tz.from_local_datetime(&naive)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(naive + Duration::hours(1))).earliest())
}

impl Schedule {
    pub fn decide<Tz: TimeZone>(&self, now: DateTime<Tz>) -> Decision<Tz> {
        match self.mode {
            Mode::Off => Decision { active: false, next_change: None },
            Mode::On => Decision { active: true, next_change: None },
            Mode::Fixed => self.decide_fixed(now),
            Mode::Sun => match (self.latitude, self.longitude) {
                (Some(lat), Some(lon)) => self.decide_sun(now, lat, lon),
                _ => self.decide_fixed(now),
            },
        }
    }

    fn decide_fixed<Tz: TimeZone>(&self, now: DateTime<Tz>) -> Decision<Tz> {
        if self.start == self.end {
            return Decision { active: false, next_change: None };
        }
        let tz = now.timezone();
        let today = now.date_naive();
        let mut transitions: Vec<(DateTime<Tz>, bool)> = Vec::new();
        for offset in -1..=1 {
            let date = today + Duration::days(offset);
            transitions.extend(local_instant(&tz, date, self.start.naive()).map(|t| (t, true)));
            transitions.extend(local_instant(&tz, date, self.end.naive()).map(|t| (t, false)));
        }
        resolve(transitions, &now, None)
    }

    fn decide_sun<Tz: TimeZone>(&self, now: DateTime<Tz>, lat: f64, lon: f64) -> Decision<Tz> {
        let tz = now.timezone();
        let today = now.date_naive();
        let mut transitions: Vec<(DateTime<Tz>, bool)> = Vec::new();
        let mut today_day = None;
        for offset in -1..=2 {
            let date = today + Duration::days(offset);
            let day = sun_day(date, lat, lon);
            if offset == 0 {
                today_day = Some(day);
            }
            if let SunDay::Normal { rise, set } = day {
                transitions.push((rise.with_timezone(&tz), false));
                transitions.push((set.with_timezone(&tz), true));
            }
        }
        let polar = match today_day {
            Some(SunDay::PolarNight) => Some(true),
            Some(SunDay::PolarDay) => Some(false),
            _ => None,
        };
        resolve(transitions, &now, polar)
    }
}

/// From a set of (instant, becomes-active) transitions: the state at `now` is that of the
/// latest transition not after it; the next change is the first one after it. `forced`
/// overrides the state (polar day / night) but not the next change.
fn resolve<Tz: TimeZone>(mut transitions: Vec<(DateTime<Tz>, bool)>, now: &DateTime<Tz>, forced: Option<bool>) -> Decision<Tz> {
    transitions.sort_by_key(|(t, _)| t.clone());
    let current = transitions.iter().rev().find(|(t, _)| t <= now).map(|(_, a)| *a);
    let next_change = transitions.iter().find(|(t, _)| t > now).map(|(t, _)| t.clone());
    let active = forced.or(current).unwrap_or(false);
    // A "change" to the state we are already in (consecutive transitions of one kind
    // around a polar day) is not a change.
    let next_change = next_change.filter(|t| transitions.iter().find(|(x, _)| x == t).is_some_and(|(_, a)| *a != active));
    Decision { active, next_change }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    fn at(h: u32, m: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(3 * 3600).unwrap().with_ymd_and_hms(2024, 11, 14, h, m, 0).unwrap()
    }

    fn fixed(start: (u8, u8), end: (u8, u8)) -> Schedule {
        Schedule { mode: Mode::Fixed, start: TimeOfDay::new(start.0, start.1), end: TimeOfDay::new(end.0, end.1), ..Schedule::default() }
    }

    #[test]
    fn off_and_on_ignore_the_clock() {
        let off = Schedule { mode: Mode::Off, ..Schedule::default() };
        let on = Schedule { mode: Mode::On, ..Schedule::default() };
        assert_eq!(off.decide(at(23, 0)), Decision { active: false, next_change: None });
        assert_eq!(on.decide(at(12, 0)), Decision { active: true, next_change: None });
    }

    #[test]
    fn default_window_wraps_midnight() {
        let s = Schedule::default();
        for (h, m, active) in [(0, 0, true), (6, 59, true), (7, 0, false), (12, 0, false), (20, 59, false), (21, 0, true), (23, 59, true)] {
            assert_eq!(s.decide(at(h, m)).active, active, "{h}:{m:02}");
        }
    }

    #[test]
    fn next_change_is_the_end_while_active_and_the_start_while_inactive() {
        let s = Schedule::default();
        let d = s.decide(at(23, 30));
        assert_eq!(TimeOfDay::of(&d.next_change.unwrap()), TimeOfDay::new(7, 0));
        assert_eq!(d.next_change.unwrap().date_naive(), at(0, 0).date_naive() + Duration::days(1));
        let d = s.decide(at(2, 0));
        assert_eq!(TimeOfDay::of(&d.next_change.unwrap()), TimeOfDay::new(7, 0));
        assert_eq!(d.next_change.unwrap().date_naive(), at(0, 0).date_naive());
        let d = s.decide(at(12, 0));
        assert_eq!(TimeOfDay::of(&d.next_change.unwrap()), TimeOfDay::new(21, 0));
    }

    #[test]
    fn daytime_window_does_not_wrap() {
        let s = fixed((9, 0), (17, 0));
        assert!(!s.decide(at(8, 59)).active);
        assert!(s.decide(at(9, 0)).active);
        assert!(s.decide(at(16, 59)).active);
        assert!(!s.decide(at(17, 0)).active);
        assert!(!s.decide(at(23, 0)).active);
    }

    #[test]
    fn an_empty_window_never_tints() {
        let s = fixed((7, 0), (7, 0));
        assert_eq!(s.decide(at(7, 0)), Decision { active: false, next_change: None });
    }

    #[test]
    fn sun_mode_without_coordinates_falls_back_to_the_fixed_window() {
        let s = Schedule { mode: Mode::Sun, ..Schedule::default() };
        assert!(s.decide(at(23, 0)).active);
        assert!(!s.decide(at(12, 0)).active);
    }

    #[test]
    fn sun_mode_follows_sunset_and_sunrise() {
        // Moscow, 2024-12-21 (MSK = UTC+3): sunrise ~08:57, sunset ~15:57.
        let tz = FixedOffset::east_opt(3 * 3600).unwrap();
        let at = |h, m| tz.with_ymd_and_hms(2024, 12, 21, h, m, 0).unwrap();
        let s = Schedule { mode: Mode::Sun, latitude: Some(55.75), longitude: Some(37.62), ..Schedule::default() };
        assert!(s.decide(at(3, 0)).active);
        assert!(!s.decide(at(12, 0)).active);
        assert!(s.decide(at(18, 0)).active);
        let d = s.decide(at(12, 0));
        let t = TimeOfDay::of(&d.next_change.unwrap());
        assert!(t >= TimeOfDay::new(15, 52) && t <= TimeOfDay::new(16, 2), "sunset at {t}");
        let d = s.decide(at(3, 0));
        let t = TimeOfDay::of(&d.next_change.unwrap());
        assert!(t >= TimeOfDay::new(8, 52) && t <= TimeOfDay::new(9, 2), "sunrise at {t}");
        // After sunset the next change is tomorrow's sunrise.
        let d = s.decide(at(20, 0));
        assert_eq!(d.next_change.unwrap().date_naive(), at(0, 0).date_naive() + Duration::days(1));
    }

    #[test]
    fn polar_night_is_always_on_and_polar_day_always_off() {
        let tz = FixedOffset::east_opt(3600).unwrap();
        let s = Schedule { mode: Mode::Sun, latitude: Some(78.2), longitude: Some(15.6), ..Schedule::default() };
        let winter = tz.with_ymd_and_hms(2024, 12, 21, 12, 0, 0).unwrap();
        let summer = tz.with_ymd_and_hms(2024, 6, 21, 12, 0, 0).unwrap();
        let d = s.decide(winter);
        assert!(d.active && d.next_change.is_none());
        let d = s.decide(summer);
        assert!(!d.active && d.next_change.is_none());
    }

    #[test]
    fn time_of_day_parses_prints_and_round_trips_through_json() {
        assert_eq!("7:05".parse::<TimeOfDay>().unwrap(), TimeOfDay::new(7, 5));
        assert_eq!(TimeOfDay::new(7, 5).to_string(), "07:05");
        assert!("24:00".parse::<TimeOfDay>().is_err());
        assert!("07".parse::<TimeOfDay>().is_err());
        assert!("7:60".parse::<TimeOfDay>().is_err());
        let json = serde_json::to_string(&TimeOfDay::new(21, 0)).unwrap();
        assert_eq!(json, "\"21:00\"");
        assert_eq!(serde_json::from_str::<TimeOfDay>(&json).unwrap(), TimeOfDay::new(21, 0));
    }

    #[test]
    fn mode_parses_aliases() {
        assert_eq!("manual".parse::<Mode>().unwrap(), Mode::On);
        assert_eq!("Sunset".parse::<Mode>().unwrap(), Mode::Sun);
        assert!("nope".parse::<Mode>().is_err());
        assert_eq!(serde_json::to_string(&Mode::Fixed).unwrap(), "\"fixed\"");
    }
}
