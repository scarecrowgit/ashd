//! Idle configuration: the `"idle"` section of `shell.json`.
//!
//! ```json
//! { "idle": { "enabled": true, "lockAfter": "10m", "screenOffAfter": "15m" } }
//! ```
//!
//! (`<etc>/power.json`, written by the NixOS module from `ashd.power.settings`, is read right
//! after `shell.json` and has the same sections.) Durations are a number of seconds (`600`), a humantime string (`"10m"`, `"90s"`) or
//! "never" (`0`, `false`, `"never"`, `"off"`, `""`). The system file
//! (`/etc/ashd/shell.json`, `$ASHD_ETC_DIR`) is read first, then
//! `$XDG_CONFIG_HOME/ashd/shell.json` overrides it field by field. `"enabled": false`
//! turns both stages off.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

/// Default time without input before the session is locked.
pub const DEFAULT_LOCK_AFTER: Duration = Duration::from_secs(10 * 60);
/// Default time without input before the monitors are powered off.
pub const DEFAULT_SCREEN_OFF_AFTER: Duration = Duration::from_secs(15 * 60);
/// Shortest accepted stage (a typo like `"5"` milliseconds must not lock instantly).
pub const MIN_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleConfig {
    /// `None`: never lock because of idleness.
    pub lock_after: Option<Duration>,
    /// `None`: never power the monitors off.
    pub screen_off_after: Option<Duration>,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self { lock_after: Some(DEFAULT_LOCK_AFTER), screen_off_after: Some(DEFAULT_SCREEN_OFF_AFTER) }
    }
}

impl IdleConfig {
    /// Both stages off.
    pub const DISABLED: Self = Self { lock_after: None, screen_off_after: None };

    /// Short stages for tests and the nested session.
    pub fn new(lock_after: Option<Duration>, screen_off_after: Option<Duration>) -> Self {
        let clamp = |d: Option<Duration>| d.map(|d| d.max(MIN_AFTER));
        Self { lock_after: clamp(lock_after), screen_off_after: clamp(screen_off_after) }
    }

    /// System file, then user file.
    pub fn load() -> Self {
        let etc = ashd_tokens::state::etc_dir();
        let mut paths = vec![etc.join("shell.json"), etc.join("power.json")];
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        {
            paths.push(dir.join("ashd/shell.json"));
        }
        Self::load_from(&paths)
    }

    /// Merge `paths` in order; missing files are fine, malformed ones are logged and skipped.
    pub fn load_from(paths: &[impl AsRef<Path>]) -> Self {
        let mut cfg = Self::default();
        let mut enabled = true;
        for path in paths {
            let path = path.as_ref();
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    tracing::warn!("idle config {}: {e}", path.display());
                    continue;
                }
            };
            match serde_json::from_str::<File>(&text) {
                Ok(file) => {
                    let Some(raw) = file.idle else { continue };
                    if let Some(e) = raw.enabled {
                        enabled = e;
                    }
                    if let Some(v) = raw.lock_after.filter(|v| !matches!(v, Spec::Flag(true))) {
                        cfg.lock_after = v.resolve();
                    }
                    if let Some(v) = raw.screen_off_after.filter(|v| !matches!(v, Spec::Flag(true))) {
                        cfg.screen_off_after = v.resolve();
                    }
                }
                Err(e) => tracing::warn!("idle config {}: {e}", path.display()),
            }
        }
        if !enabled {
            return Self::DISABLED;
        }
        Self::new(cfg.lock_after, cfg.screen_off_after)
    }
}

#[derive(Deserialize)]
struct File {
    idle: Option<Raw>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Raw {
    enabled: Option<bool>,
    lock_after: Option<Spec>,
    screen_off_after: Option<Spec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Spec {
    Secs(u64),
    Flag(bool),
    Text(String),
}

impl Spec {
    fn resolve(&self) -> Option<Duration> {
        match self {
            Spec::Secs(0) | Spec::Flag(false) => None,
            Spec::Secs(s) => Some(Duration::from_secs(*s)),
            Spec::Flag(true) => None, // filtered out by the loader ("true" = keep the default)
            Spec::Text(t) => {
                let t = t.trim();
                if t.is_empty() || matches!(t.to_ascii_lowercase().as_str(), "never" | "off" | "none" | "false") {
                    return None;
                }
                match humantime::parse_duration(t) {
                    Ok(d) if d.is_zero() => None,
                    Ok(d) => Some(d),
                    Err(e) => {
                        tracing::warn!("idle config: bad duration {t:?}: {e}");
                        None
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(texts: &[&str]) -> IdleConfig {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let p = dir.path().join(format!("{i}.json"));
                std::fs::write(&p, t).unwrap();
                p
            })
            .collect();
        IdleConfig::load_from(&paths)
    }

    #[test]
    fn defaults_without_a_section() {
        assert_eq!(load(&[]), IdleConfig::default());
        assert_eq!(load(&[r#"{"dock":[]}"#]), IdleConfig::default());
        let d = IdleConfig::default();
        assert_eq!(d.lock_after, Some(Duration::from_secs(600)));
        assert_eq!(d.screen_off_after, Some(Duration::from_secs(900)));
    }

    #[test]
    fn numbers_and_humantime_strings() {
        let c = load(&[r#"{"idle":{"lockAfter":5,"screenOffAfter":"8s"}}"#]);
        assert_eq!(c, IdleConfig::new(Some(Duration::from_secs(5)), Some(Duration::from_secs(8))));
        let c = load(&[r#"{"idle":{"lockAfter":"2m"}}"#]);
        assert_eq!(c.lock_after, Some(Duration::from_secs(120)));
        assert_eq!(c.screen_off_after, Some(DEFAULT_SCREEN_OFF_AFTER));
    }

    #[test]
    fn never_disables_a_stage_and_enabled_false_both() {
        let c = load(&[r#"{"idle":{"lockAfter":0,"screenOffAfter":"never"}}"#]);
        assert_eq!(c, IdleConfig::DISABLED);
        let c = load(&[r#"{"idle":{"screenOffAfter":false}}"#]);
        assert_eq!(c.screen_off_after, None);
        assert!(c.lock_after.is_some());
        assert_eq!(load(&[r#"{"idle":{"enabled":false,"lockAfter":5}}"#]), IdleConfig::DISABLED);
    }

    #[test]
    fn user_file_overrides_system_per_field_and_bad_files_are_skipped() {
        let c = load(&[r#"{"idle":{"lockAfter":"1m","screenOffAfter":"2m"}}"#, r#"{"idle":{"lockAfter":"30s"}}"#, "{ nope"]);
        assert_eq!(c.lock_after, Some(Duration::from_secs(30)));
        assert_eq!(c.screen_off_after, Some(Duration::from_secs(120)));
    }

    #[test]
    fn sub_second_stages_are_clamped() {
        let c = IdleConfig::new(Some(Duration::from_millis(5)), None);
        assert_eq!(c.lock_after, Some(MIN_AFTER));
    }
}
