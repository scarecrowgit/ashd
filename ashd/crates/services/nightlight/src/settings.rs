//! Night light settings: defaults, the `"nightlight"` section of `shell.json`, and the
//! per-user state file the tile writes (`<state dir>/nightlight.json`).
//!
//! ```json
//! { "nightlight": { "mode": "fixed", "temperature": 3400,
//!                   "start": "21:00", "end": "07:00",
//!                   "latitude": 52.5, "longitude": 13.4, "transitionSecs": 45 } }
//! ```
//!
//! Precedence: built-in defaults < `/etc/ashd/shell.json` < `$XDG_CONFIG_HOME/ashd/shell.json`
//! < the state file, which only holds what the UI changes (`mode`, `temperature`,
//! `lastMode`). Delete the state file to go back to `shell.json`.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    ramp::{MIN_K, NEUTRAL_K},
    schedule::{Mode, Schedule, TimeOfDay},
};

/// Warm white of a typical "night" setting.
pub const DEFAULT_TEMPERATURE: u32 = 3400;
/// Length of the ramp at a schedule edge.
pub const DEFAULT_SCHEDULE_TRANSITION: Duration = Duration::from_secs(45);
/// Length of the ramp when toggled by hand.
pub const DEFAULT_MANUAL_TRANSITION: Duration = Duration::from_secs(1);
/// Ramp when the temperature is adjusted while tinted (slider drags).
pub const ADJUST_TRANSITION: Duration = Duration::from_millis(250);
/// Schedule ramps are kept inside the 30-60 s the design asks for.
const MIN_SCHEDULE_TRANSITION: Duration = Duration::from_secs(30);
const MAX_SCHEDULE_TRANSITION: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub schedule: Schedule,
    /// The tinted temperature in Kelvin.
    pub temperature: u32,
    /// What "switch on" returns to (never `Off`).
    pub last_mode: Mode,
    pub schedule_transition: Duration,
    pub manual_transition: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schedule: Schedule::default(),
            temperature: DEFAULT_TEMPERATURE,
            last_mode: Mode::Fixed,
            schedule_transition: DEFAULT_SCHEDULE_TRANSITION,
            manual_transition: DEFAULT_MANUAL_TRANSITION,
        }
    }
}

pub fn clamp_temperature(k: u32) -> u32 {
    k.clamp(MIN_K, NEUTRAL_K)
}

impl Settings {
    pub fn mode(&self) -> Mode {
        self.schedule.mode
    }

    /// Switch to `mode`, remembering the last non-off one.
    pub fn set_mode(&mut self, mode: Mode) {
        self.schedule.mode = mode;
        if mode != Mode::Off {
            self.last_mode = mode;
        }
    }

    pub fn enabled(&self) -> bool {
        self.schedule.mode != Mode::Off
    }

    /// `shell.json` files in order, then the state file.
    pub fn load(state_file: &Path) -> Self {
        let etc = ashd_tokens::state::etc_dir();
        let mut paths = vec![etc.join("shell.json"), etc.join("power.json")];
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        {
            paths.push(dir.join("ashd/shell.json"));
        }
        Self::load_from(&paths, state_file)
    }

    pub fn load_from(config_files: &[impl AsRef<Path>], state_file: &Path) -> Self {
        let mut s = Self::default();
        for path in config_files {
            let path = path.as_ref();
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    tracing::warn!("nightlight config {}: {e}", path.display());
                    continue;
                }
            };
            match serde_json::from_str::<ConfigFile>(&text) {
                Ok(f) => {
                    if let Some(raw) = f.nightlight {
                        s.apply_config(raw);
                    }
                }
                Err(e) => tracing::warn!("nightlight config {}: {e}", path.display()),
            }
        }
        match std::fs::read_to_string(state_file) {
            Ok(text) => match serde_json::from_str::<Persisted>(&text) {
                Ok(p) => s.apply_persisted(p),
                Err(e) => tracing::warn!("nightlight state {}: {e}", state_file.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("nightlight state {}: {e}", state_file.display()),
        }
        s
    }

    fn apply_config(&mut self, raw: RawConfig) {
        if let Some(m) = raw.mode {
            self.set_mode(m);
        }
        if let Some(k) = raw.temperature {
            self.temperature = clamp_temperature(k);
        }
        if let Some(t) = raw.start {
            self.schedule.start = t;
        }
        if let Some(t) = raw.end {
            self.schedule.end = t;
        }
        if raw.latitude.is_some() {
            self.schedule.latitude = raw.latitude.filter(|v| (-90.0..=90.0).contains(v));
        }
        if raw.longitude.is_some() {
            self.schedule.longitude = raw.longitude.filter(|v| (-180.0..=180.0).contains(v));
        }
        if let Some(s) = raw.transition_secs {
            self.schedule_transition = Duration::from_secs_f64(s.max(0.0)).clamp(MIN_SCHEDULE_TRANSITION, MAX_SCHEDULE_TRANSITION);
        }
    }

    fn apply_persisted(&mut self, p: Persisted) {
        if let Some(m) = p.last_mode.filter(|m| *m != Mode::Off) {
            self.last_mode = m;
        }
        if let Some(m) = p.mode {
            let last = self.last_mode;
            self.set_mode(m);
            if m == Mode::Off {
                self.last_mode = last;
            }
        }
        if let Some(k) = p.temperature {
            self.temperature = clamp_temperature(k);
        }
    }

    pub fn persisted(&self) -> Persisted {
        Persisted { mode: Some(self.mode()), temperature: Some(self.temperature), last_mode: Some(self.last_mode) }
    }
}

/// `<state dir>/nightlight.json`.
pub fn state_file() -> PathBuf {
    ashd_tokens::state::state_dir().join("nightlight.json")
}

/// What the UI changes and the service remembers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Persisted {
    pub mode: Option<Mode>,
    pub temperature: Option<u32>,
    pub last_mode: Option<Mode>,
}

pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let mut text = serde_json::to_string_pretty(&settings.persisted()).map_err(std::io::Error::other)?;
    text.push('\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ashd_tokens::state::write_atomic(path, text.as_bytes())
}

#[derive(Deserialize)]
struct ConfigFile {
    nightlight: Option<RawConfig>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawConfig {
    mode: Option<Mode>,
    temperature: Option<u32>,
    start: Option<TimeOfDay>,
    end: Option<TimeOfDay>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    transition_secs: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::load_from(&[] as &[PathBuf], &dir.path().join("missing"));
        assert_eq!(s, Settings::default());
        assert_eq!(s.mode(), Mode::Fixed);
        assert_eq!((s.schedule.start, s.schedule.end), (TimeOfDay::new(21, 0), TimeOfDay::new(7, 0)));
        assert_eq!(s.temperature, 3400);
    }

    #[test]
    fn config_files_merge_field_by_field_and_the_state_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        let sys = write(dir.path(), "sys.json", r#"{"nightlight":{"mode":"sun","temperature":3000,"latitude":52.5,"longitude":13.4,"start":"22:00"}}"#);
        let user = write(dir.path(), "user.json", r#"{"nightlight":{"temperature":4000,"end":"06:30","transitionSecs":600}}"#);
        let bad = write(dir.path(), "bad.json", "{ nope");
        let state = write(dir.path(), "state.json", r#"{"mode":"off","temperature":2800,"lastMode":"on"}"#);
        let s = Settings::load_from(&[&sys, &user, &bad], &state);
        assert_eq!(s.mode(), Mode::Off);
        assert_eq!(s.last_mode, Mode::On);
        assert_eq!(s.temperature, 2800);
        assert_eq!((s.schedule.start, s.schedule.end), (TimeOfDay::new(22, 0), TimeOfDay::new(6, 30)));
        assert_eq!((s.schedule.latitude, s.schedule.longitude), (Some(52.5), Some(13.4)));
        assert_eq!(s.schedule_transition, Duration::from_secs(60), "clamped to the 30-60 s the design asks for");
    }

    #[test]
    fn without_a_state_file_the_config_mode_applies() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write(dir.path(), "c.json", r#"{"nightlight":{"mode":"off"}}"#);
        let s = Settings::load_from(&[&cfg], &dir.path().join("none.json"));
        assert_eq!(s.mode(), Mode::Off);
        assert_eq!(s.last_mode, Mode::Fixed, "switching on goes back to the schedule");
    }

    #[test]
    fn out_of_range_values_are_ignored_or_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write(dir.path(), "c.json", r#"{"nightlight":{"temperature":99999,"latitude":123,"longitude":-200}}"#);
        let s = Settings::load_from(&[&cfg], &dir.path().join("none.json"));
        assert_eq!(s.temperature, NEUTRAL_K);
        assert_eq!((s.schedule.latitude, s.schedule.longitude), (None, None));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/dir/nightlight.json");
        let mut s = Settings::default();
        s.set_mode(Mode::On);
        s.temperature = 2600;
        s.set_mode(Mode::Off);
        save(&path, &s).unwrap();
        let back = Settings::load_from(&[] as &[PathBuf], &path);
        assert_eq!((back.mode(), back.last_mode, back.temperature), (Mode::Off, Mode::On, 2600));
    }
}
