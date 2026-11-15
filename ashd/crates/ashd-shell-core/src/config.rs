//! Optional shell configuration: `/etc/ashd/shell.json` (system, e.g. from the NixOS
//! module), then `$XDG_CONFIG_HOME/ashd/shell.json` (user) overriding it field by field.
//!
//! ```json
//! { "dock": ["kitty", "firefox", "org.gnome.Nautilus"],
//!   "apps": { "terminal": "kitty", "browser": "firefox", "fileManager": "org.gnome.Nautilus" },
//!   "reduceMotion": false }
//! ```
//!
//! `dock` entries are desktop-file ids (`kitty` or `kitty.desktop`). `apps` values are a
//! desktop id or, if no installed app has that id, a command line (`"foot -e btop"`).
//! `reduceMotion: true` is the shell's one motion switch: movement happens at once and only
//! 80 ms fades remain (TOKENS.md §9); `ASHD_REDUCE_MOTION=1` in the environment does the
//! same and wins.
//! Read once at startup and again on SIGHUP; panels read it with
//! [`crate::config`] / [`ShellHandle::config`](crate::ShellHandle::config).

use std::path::{Path, PathBuf};

use gpui::{App, Global};
use serde::Deserialize;

/// Well-known applications (`apps` in shell.json).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppsConfig {
    pub terminal: Option<String>,
    pub browser: Option<String>,
    pub file_manager: Option<String>,
}

/// The merged shell configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellConfig {
    /// Pinned dock apps, desktop ids in order (`.desktop` suffix stripped).
    pub dock: Vec<String>,
    pub apps: AppsConfig,
    /// `reduceMotion` (see the module docs); `None` = not set.
    pub reduce_motion: Option<bool>,
    /// Files that were read, in order (for logs / `ashd status` debugging).
    pub sources: Vec<PathBuf>,
}

impl Global for ShellConfig {}

/// One file as written by the user: every field optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    dock: Option<Vec<String>>,
    apps: Option<AppsConfig>,
    #[serde(rename = "reduceMotion")]
    reduce_motion: Option<bool>,
}

impl ShellConfig {
    /// System then user file. Missing files are fine; malformed ones are logged and skipped.
    pub fn load() -> Self {
        let user = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("ashd/shell.json"));
        let mut paths = vec![PathBuf::from("/etc/ashd/shell.json")];
        paths.extend(user);
        Self::load_from(&paths)
    }

    /// Merge `paths` in order (later files override earlier ones per field).
    pub fn load_from(paths: &[impl AsRef<Path>]) -> Self {
        let mut config = Self::default();
        for path in paths {
            let path = path.as_ref();
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    tracing::warn!("config {}: {e}", path.display());
                    continue;
                }
            };
            match serde_json::from_str::<RawConfig>(&text) {
                Ok(raw) => {
                    config.merge(raw);
                    config.sources.push(path.to_path_buf());
                }
                Err(e) => tracing::warn!("config {}: {e}", path.display()),
            }
        }
        config
    }

    fn merge(&mut self, raw: RawConfig) {
        if let Some(dock) = raw.dock {
            self.dock = dock.into_iter().map(|id| normalize_id(&id)).filter(|id| !id.is_empty()).collect();
        }
        if raw.reduce_motion.is_some() {
            self.reduce_motion = raw.reduce_motion;
        }
        if let Some(apps) = raw.apps {
            self.apps.terminal = apps.terminal.or(self.apps.terminal.take());
            self.apps.browser = apps.browser.or(self.apps.browser.take());
            self.apps.file_manager = apps.file_manager.or(self.apps.file_manager.take());
        }
    }
}

/// Apply `reduceMotion` from the configuration (startup, SIGHUP). The environment variable
/// `ASHD_REDUCE_MOTION` decides when it is set; otherwise the file does (default: off).
pub fn apply_motion(cx: &mut App) {
    if std::env::var_os("ASHD_REDUCE_MOTION").is_some() {
        return;
    }
    let on = config(cx).reduce_motion.unwrap_or(false);
    ashd_ui::anim::set_reduced_motion(on, cx);
}

/// `kitty.desktop` → `kitty`.
pub fn normalize_id(id: &str) -> String {
    id.trim().trim_end_matches(".desktop").to_string()
}

/// The shell configuration (default if the shell has not loaded one, e.g. in tests).
pub fn config(cx: &App) -> &ShellConfig {
    static EMPTY: std::sync::OnceLock<ShellConfig> = std::sync::OnceLock::new();
    cx.try_global::<ShellConfig>().unwrap_or_else(|| EMPTY.get_or_init(ShellConfig::default))
}

/// argv for an `apps` value or a dock id: the installed app's launch argv (terminal apps
/// wrapped in the configured terminal), else the value split as a command line.
pub fn resolve_argv(value: &str, apps: &[ashd_svc_apps::App], terminal: Option<&str>) -> Option<Vec<String>> {
    let id = normalize_id(value);
    if let Some(app) = apps.iter().find(|a| a.id == id) {
        // `Terminal=true` apps run as `<terminal> -e <exec>` (kitty, foot, alacritty,
        // wezterm and ghostty all accept `-e`).
        let term: Option<Vec<String>> = terminal
            .filter(|t| normalize_id(t) != app.id)
            .and_then(|t| resolve_argv(t, apps, None))
            .map(|mut argv| {
                argv.push("-e".into());
                argv
            });
        let argv = app.launch_argv(term.as_deref());
        return (!argv.is_empty()).then_some(argv);
    }
    shlex::split(value).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_overrides_system_per_field() {
        let dir = std::env::temp_dir().join(format!("ashd-shell-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = dir.join("sys.json");
        let user = dir.join("user.json");
        std::fs::write(&sys, r#"{"dock":["kitty.desktop","firefox"],"apps":{"terminal":"kitty","browser":"firefox"}}"#)
            .unwrap();
        std::fs::write(&user, r#"{"apps":{"browser":"zen","fileManager":"yazi"},"reduceMotion":true}"#).unwrap();
        let c = ShellConfig::load_from(&[&sys, &user, &dir.join("missing.json")]);
        assert_eq!(c.dock, vec!["kitty", "firefox"]);
        assert_eq!(c.apps.terminal.as_deref(), Some("kitty"));
        assert_eq!(c.apps.browser.as_deref(), Some("zen"));
        assert_eq!(c.apps.file_manager.as_deref(), Some("yazi"));
        assert_eq!(c.sources.len(), 2);
        assert_eq!(c.reduce_motion, Some(true));

        std::fs::write(&user, "{ not json").unwrap();
        let c = ShellConfig::load_from(&[&sys, &user]);
        assert_eq!(c.apps.browser.as_deref(), Some("firefox"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn argv_falls_back_to_command_line() {
        assert_eq!(resolve_argv("foot -e btop", &[], None), Some(vec!["foot".into(), "-e".into(), "btop".into()]));
        assert_eq!(resolve_argv("", &[], None), None);
    }
}
