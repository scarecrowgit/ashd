//! Matching niri windows to desktop apps, and a monochrome glyph per app (rail flyout,
//! dock fallback, later alt-tab). Plain functions over the apps service state.

use ashd_svc_apps::App;
use ashd_ui::Icon;

/// The installed app a window with `app_id` belongs to: `StartupWMClass`/id match
/// (case-insensitive), then reverse-DNS suffix (`org.gnome.Nautilus` ↔ `nautilus`), then
/// the exec basename.
pub fn app_for_app_id<'a>(apps: &'a [App], app_id: &str) -> Option<&'a App> {
    let want = app_id.to_ascii_lowercase();
    let exact = |a: &&App| a.window_app_id().eq_ignore_ascii_case(app_id) || a.id.eq_ignore_ascii_case(app_id);
    let suffix = |a: &&App| {
        let id = a.id.to_ascii_lowercase();
        id.rsplit('.').next() == Some(want.as_str()) || want.rsplit('.').next() == Some(id.as_str())
    };
    let exec = |a: &&App| {
        a.exec.first().and_then(|e| e.rsplit('/').next()).is_some_and(|b| b.eq_ignore_ascii_case(app_id))
    };
    apps.iter().find(exact).or_else(|| apps.iter().find(suffix)).or_else(|| apps.iter().find(exec))
}

/// A stroke glyph for an app (by id / categories), [`Icon::Grid`] when nothing fits.
pub fn app_glyph(app: Option<&App>, app_id: &str) -> Icon {
    let id = app.map(|a| a.id.as_str()).unwrap_or(app_id).to_ascii_lowercase();
    let cats: Vec<String> = app.map(|a| a.categories.iter().map(|c| c.to_ascii_lowercase()).collect()).unwrap_or_default();
    let has = |c: &str| cats.iter().any(|x| x == c);
    let any = |keys: &[&str]| keys.iter().any(|k| id.contains(k));
    if has("terminalemulator") || any(&["kitty", "foot", "alacritty", "wezterm", "ghostty", "terminal", "konsole"]) {
        Icon::Term
    } else if has("webbrowser") || any(&["firefox", "zen", "chrom", "brave", "browser", "librewolf"]) {
        Icon::Globe
    } else if has("texteditor") || has("development") || has("ide") || any(&["nvim", "vim", "code", "zed", "emacs", "helix"]) {
        Icon::Code
    } else if has("filemanager") || any(&["nautilus", "thunar", "dolphin", "yazi", "nemo", "files"]) {
        Icon::Folder
    } else if has("instantmessaging") || has("chat") || any(&["telegram", "discord", "matrix", "element", "signal", "slack"]) {
        Icon::Chat
    } else if has("audio") || has("music") || has("player") || any(&["spotify", "music", "mpv", "player"]) {
        Icon::Music
    } else if has("graphics") || has("viewer") || any(&["imv", "image", "gimp", "krita", "loupe"]) {
        Icon::Image
    } else if has("monitor") || any(&["btop", "htop", "monitor"]) {
        Icon::Activity
    } else {
        Icon::Grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, wm: Option<&str>, exec: &str, cats: &[&str]) -> App {
        App {
            id: id.into(),
            name: id.into(),
            exec: vec![exec.into()],
            startup_wm_class: wm.map(Into::into),
            categories: cats.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn matching_and_glyphs() {
        let apps = vec![
            app("org.gnome.Nautilus", None, "nautilus", &["FileManager"]),
            app("firefox", None, "/usr/bin/firefox", &["WebBrowser"]),
            app("kitty", Some("kitty"), "kitty", &["TerminalEmulator"]),
        ];
        assert_eq!(app_for_app_id(&apps, "org.gnome.Nautilus").map(|a| a.id.as_str()), Some("org.gnome.Nautilus"));
        assert_eq!(app_for_app_id(&apps, "nautilus").map(|a| a.id.as_str()), Some("org.gnome.Nautilus"));
        assert_eq!(app_for_app_id(&apps, "Firefox").map(|a| a.id.as_str()), Some("firefox"));
        assert!(app_for_app_id(&apps, "foot").is_none());
        assert_eq!(app_glyph(app_for_app_id(&apps, "kitty"), "kitty"), Icon::Term);
        assert_eq!(app_glyph(None, "foot"), Icon::Term);
        assert_eq!(app_glyph(None, "weird"), Icon::Grid);
    }
}
