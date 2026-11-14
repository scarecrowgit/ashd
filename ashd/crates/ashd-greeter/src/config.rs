//! `/etc/ashd/greeter.json`, the state file with the last successful user, the theme.

use std::{
    fs,
    path::{Path, PathBuf},
};

use ashd_tokens::ThemeName;
use serde::{Deserialize, Serialize};

/// One session the greeter can start: the name on the session line and the argv handed to
/// greetd's `start_session`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SessionDef {
    pub name: String,
    pub cmd: Vec<String>,
    /// `KEY=VALUE` pairs added to the session environment.
    #[serde(default)]
    pub env: Vec<String>,
}

impl SessionDef {
    pub fn from_cmd(cmd: Vec<String>) -> Self {
        Self { name: name_of(&cmd), cmd, env: Vec::new() }
    }
}

/// `niri-session` → "niri", `/usr/bin/sway` → "sway".
fn name_of(cmd: &[String]) -> String {
    let base = cmd.first().map(|c| c.rsplit('/').next().unwrap_or(c)).unwrap_or("session");
    base.strip_suffix("-session").filter(|b| !b.is_empty()).unwrap_or(base).to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub sessions: Vec<SessionDef>,
    pub default_user: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { sessions: vec![SessionDef { name: "niri".into(), cmd: vec!["niri-session".into()], env: Vec::new() }], default_user: None }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileConfig {
    sessions: Vec<SessionDef>,
    default_user: Option<String>,
}

impl Config {
    /// `/etc/ashd/greeter.json` (or `$ASHD_ETC_DIR/greeter.json`).
    pub fn default_path() -> PathBuf {
        ashd_tokens::state::etc_dir().join("greeter.json")
    }

    /// Parse the JSON text. Sessions without a name or a command are dropped; no sessions
    /// left means the built-in `niri-session`.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let f: FileConfig = serde_json::from_str(text)?;
        let mut cfg = Self::default();
        let sessions: Vec<SessionDef> = f.sessions.into_iter().filter(|s| !s.name.trim().is_empty() && !s.cmd.is_empty()).collect();
        if !sessions.is_empty() {
            cfg.sessions = sessions;
        }
        cfg.default_user = f.default_user.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
        Ok(cfg)
    }

    /// A missing file is the default config; an unreadable or invalid one is logged and
    /// the default is used too (a broken config must not lock everybody out).
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                tracing::error!("{}: {e}; using the defaults", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::error!("{}: {e}; using the defaults", path.display());
                Self::default()
            }
        }
    }
}

/// What the greeter remembers between logins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct State {
    #[serde(default)]
    pub user: Option<String>,
    /// Name of the last session.
    #[serde(default)]
    pub session: Option<String>,
}

/// Where [`State`] lives: `$ASHD_GREETER_STATE` (a file), else
/// `/var/cache/ashd-greeter/state.json`. `None` for a demo run without the variable
/// (a demo never touches the real cache).
pub fn state_path(demo: bool) -> Option<PathBuf> {
    match std::env::var_os("ASHD_GREETER_STATE") {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ if demo => None,
        _ => Some(PathBuf::from("/var/cache/ashd-greeter/state.json")),
    }
}

impl State {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Atomic write (temp file + rename); the directory must exist (tmpfiles creates it).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        fs::rename(&tmp, path)
    }
}

/// The theme from `/etc/ashd/theme` (`$ASHD_ETC_DIR`), Ash when missing or invalid. The
/// greeter user has no state dir of its own, so the per-user theme file is not consulted.
pub fn system_theme() -> ThemeName {
    ashd_tokens::state::resolve_theme(Path::new("/nonexistent/ashd-theme"), &ashd_tokens::state::system_theme_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_file() {
        let cfg = Config::parse(
            r#"{ "sessions": [ { "name": "niri", "cmd": ["niri-session"] },
                               { "name": "sway", "cmd": ["sway"], "env": ["A=b"] } ],
                 "default_user": " alice " }"#,
        )
        .unwrap();
        assert_eq!(cfg.sessions.len(), 2);
        assert_eq!(cfg.sessions[1], SessionDef { name: "sway".into(), cmd: vec!["sway".into()], env: vec!["A=b".into()] });
        assert_eq!(cfg.default_user.as_deref(), Some("alice"));
    }

    #[test]
    fn empty_or_broken_sessions_fall_back_to_niri() {
        for text in ["{}", r#"{"sessions": []}"#, r#"{"sessions":[{"name":"","cmd":["x"]},{"name":"y","cmd":[]}]}"#] {
            assert_eq!(Config::parse(text).unwrap(), Config::default(), "{text}");
        }
        assert!(Config::parse("not json").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(&dir.path().join("missing.json")), Config::default());
        let bad = dir.path().join("bad.json");
        fs::write(&bad, "{").unwrap();
        assert_eq!(Config::load(&bad), Config::default());
    }

    #[test]
    fn session_names_come_from_the_command() {
        assert_eq!(SessionDef::from_cmd(vec!["niri-session".into()]).name, "niri");
        assert_eq!(SessionDef::from_cmd(vec!["/run/current-system/sw/bin/sway".into(), "--unsupported-gpu".into()]).name, "sway");
        assert_eq!(SessionDef::from_cmd(vec!["-session".into()]).name, "-session");
    }

    #[test]
    fn state_round_trips_and_survives_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert_eq!(State::load(&path), State::default());
        let s = State { user: Some("alice".into()), session: Some("niri".into()) };
        s.save(&path).unwrap();
        assert_eq!(State::load(&path), s);
        fs::write(&path, "garbage").unwrap();
        assert_eq!(State::load(&path), State::default());
        assert!(s.save(&dir.path().join("no/such/dir/state.json")).is_err());
    }
}
