//! The project's real key binds for the two hints in the panel (never the mockup's): the
//! keys that toggle the scratchpad and the ones that open the clipboard manager, read from
//! `/etc/ashd/keybinds.json` (override: `$ASHD_KEYBINDS`). No file, no hint.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct Bind {
    keys: String,
    #[serde(default)]
    action: String,
}

/// The hint labels, lower-case as the file shows them (`super+s`); the label style
/// upper-cases them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// `ashd scratch toggle`.
    pub panel: Option<String>,
    /// `ashd clipboard`.
    pub clipboard: Option<String>,
    /// `ashd scratch hide` (named in the empty Windows section).
    pub hide: Option<String>,
}

pub fn keybinds_path() -> PathBuf {
    match std::env::var_os("ASHD_KEYBINDS").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => ashd_tokens::state::etc_dir().join("keybinds.json"),
    }
}

/// Hints from the JSON text of `keybinds.json`.
pub fn parse(json: &str) -> Hints {
    let Ok(binds) = serde_json::from_str::<Vec<Bind>>(json) else { return Hints::default() };
    let find = |want: &[&str]| {
        binds.iter().find_map(|b| {
            let words = shlex::split(&b.action)?;
            let ashd = words.iter().position(|w| w == "ashd" || w.ends_with("/ashd"))?;
            (words[ashd + 1..].iter().map(String::as_str).take(want.len()).eq(want.iter().copied()))
                .then(|| b.keys.to_lowercase())
        })
    };
    Hints {
        panel: find(&["scratch", "toggle"]).or_else(|| find(&["scratch", "open"])),
        clipboard: find(&["clipboard"]),
        hide: find(&["scratch", "hide"]),
    }
}

/// Read and parse `path` (blocking).
pub fn load(path: &Path) -> Hints {
    std::fs::read_to_string(path).map(|t| parse(&t)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_real_binds() {
        let json = r#"[
          {"keys":"Super+H","title":"Hide","group":"W","action":"spawn \"ashd\" \"scratch\" \"hide\""},
          {"keys":"Super+S","title":"Scratchpad","group":"W","action":"spawn \"ashd\" \"scratch\" \"toggle\""},
          {"keys":"Super+V","title":"Clipboard","group":"W","action":"spawn \"/run/current-system/sw/bin/ashd\" \"clipboard\""}
        ]"#;
        assert_eq!(
            parse(json),
            Hints { panel: Some("super+s".into()), clipboard: Some("super+v".into()), hide: Some("super+h".into()) }
        );
    }

    #[test]
    fn no_binds_no_hints() {
        assert_eq!(parse("[]"), Hints::default());
        assert_eq!(parse("not json"), Hints::default());
        let json = r#"[{"keys":"Super+H","title":"x","group":"W","action":"spawn \"ashd\" \"scratch\" \"hide\""}]"#;
        assert_eq!(parse(json), Hints { hide: Some("super+h".into()), ..Hints::default() }, "hide is not the panel toggle");
        assert_eq!(load(Path::new("/nonexistent/keybinds.json")), Hints::default());
    }
}
