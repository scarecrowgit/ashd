//! Result rows and the small pure helpers around them (text, sizes, ages, command lines,
//! web search URLs). No GPUI beyond the icon enum.

use std::{ops::Range, path::PathBuf};

use ashd_ipc::LauncherMode;
use ashd_svc_apps::App;
use ashd_ui::Icon;

/// Rows visible at once; the list scrolls past that (selection keeps itself in view).
pub const MAX_VISIBLE: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum RowIcon {
    /// The app's icon file, generic app glyph when `None`.
    App(Option<PathBuf>),
    Glyph(Icon),
    /// A glyph drawn as text (emoji).
    Text(String),
}

/// What Enter does. Executed by the panel (spawns go through niri).
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    LaunchApp(Box<App>),
    FocusWindow(u64),
    /// Spawn this argv through niri.
    Spawn(Vec<String>),
    /// A typed command line: shell-less argv when possible, the user's shell otherwise.
    Run(String),
    /// Like [`Act::Run`], inside the configured terminal.
    RunInTerminal(String),
    /// Copy to the clipboard (`wl-copy`).
    Copy(String),
    /// Switch to a prefix mode with this query.
    Mode(LauncherMode, String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub icon: RowIcon,
    pub title: String,
    /// Matched char indices in `title` (drawn in accent, weight 600).
    pub matched: Vec<u32>,
    pub subtitle: String,
    /// Right-hand hint of unselected rows (the selected row shows `↵`).
    pub hint: Option<String>,
    /// Section label drawn above this row.
    pub section: Option<&'static str>,
    pub primary: Act,
    /// Shift+Enter.
    pub alt: Option<Act>,
}

impl Row {
    pub fn new(icon: RowIcon, title: impl Into<String>, subtitle: impl Into<String>, primary: Act) -> Self {
        Self {
            icon,
            title: title.into(),
            matched: Vec::new(),
            subtitle: subtitle.into(),
            hint: None,
            section: None,
            primary,
            alt: None,
        }
    }
    pub fn matched(mut self, m: Vec<u32>) -> Self {
        self.matched = m;
        self
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
    pub fn alt(mut self, a: Act) -> Self {
        self.alt = Some(a);
        self
    }
    pub fn section(mut self, s: &'static str) -> Self {
        self.section = Some(s);
        self
    }
}

pub fn mode_for_prefix(c: char) -> Option<LauncherMode> {
    [LauncherMode::Calc, LauncherMode::Files, LauncherMode::Run, LauncherMode::Emoji, LauncherMode::Web]
        .into_iter()
        .find(|m| m.prefix() == c)
}

pub fn mode_label(m: LauncherMode) -> &'static str {
    match m {
        LauncherMode::Calc => "calc",
        LauncherMode::Files => "files",
        LauncherMode::Run => "run",
        LauncherMode::Emoji => "emoji",
        LauncherMode::Web => "web",
    }
}

pub fn placeholder(m: Option<LauncherMode>) -> &'static str {
    match m {
        None => "Search apps and windows",
        Some(LauncherMode::Calc) => "2 * (3 + 4), 0xFF, 15% of 80",
        Some(LauncherMode::Files) => "Search files",
        Some(LauncherMode::Run) => "Run a command",
        Some(LauncherMode::Emoji) => "Search emoji",
        Some(LauncherMode::Web) => "Search the web",
    }
}

/// Merge matched char indices into byte ranges of `text`.
pub fn highlight_ranges(text: &str, indices: &[u32]) -> Vec<Range<usize>> {
    if indices.is_empty() {
        return Vec::new();
    }
    let mut bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    bounds.push(text.len());
    let mut out: Vec<Range<usize>> = Vec::new();
    for &ix in indices {
        let ix = ix as usize;
        if ix + 1 >= bounds.len() {
            continue;
        }
        let (s, e) = (bounds[ix], bounds[ix + 1]);
        match out.last_mut() {
            Some(last) if last.end == s => last.end = e,
            _ => out.push(s..e),
        }
    }
    out
}

/// Char indices in `text` covered by the first occurrence of every (lower-case) token.
pub fn substring_indices(text: &str, tokens: &[String]) -> Vec<u32> {
    let hay: Vec<char> = text.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
    let mut out = Vec::new();
    for t in tokens {
        let needle: Vec<char> = t.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
        if needle.is_empty() || needle.len() > hay.len() {
            continue;
        }
        if let Some(start) = hay.windows(needle.len()).position(|w| w == needle.as_slice()) {
            out.extend((start..start + needle.len()).map(|i| i as u32));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `6.8K`, `512B`, `1.2M`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{:.0}{}", v, UNITS[u])
    }
}

/// `now`, `15 min`, `1 h`, `4 d`, `3 mo`, `2 y`. `tight` drops the space (`2d`).
pub fn human_age(secs: u64, tight: bool) -> String {
    let (n, unit) = match secs {
        0..=59 => return "now".to_string(),
        60..=3599 => (secs / 60, "min"),
        3600..=86_399 => (secs / 3600, "h"),
        86_400..=2_591_999 => (secs / 86_400, "d"),
        2_592_000..=31_535_999 => (secs / 2_592_000, "mo"),
        _ => (secs / 31_536_000, "y"),
    };
    if tight && unit != "min" { format!("{n}{unit}") } else { format!("{n} {unit}") }
}

/// Does this command line need a shell (pipes, globs, expansion, `VAR=x cmd`, …)?
pub fn needs_shell(line: &str) -> bool {
    if line.chars().any(|c| "|&;<>()$`*?[]{}~#\n".contains(c)) {
        return true;
    }
    match shlex::split(line) {
        None => true,
        Some(argv) => argv.first().is_some_and(|a| {
            a.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        }),
    }
}

/// argv to spawn for a typed command line: split without a shell when possible,
/// `<shell> -c <line>` when the line needs one.
pub fn plan_run(line: &str, shell: &str) -> Option<Vec<String>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if needs_shell(line) {
        return Some(vec![shell.to_string(), "-c".into(), line.to_string()]);
    }
    shlex::split(line).filter(|a| !a.is_empty())
}

/// Percent-encode a query for a URL component.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub struct Engine {
    pub name: &'static str,
    /// `{}` is replaced with the encoded query.
    pub url: &'static str,
    /// `!g query` style key.
    pub key: &'static str,
}

pub const ENGINES: [Engine; 7] = [
    Engine { name: "DuckDuckGo", url: "https://duckduckgo.com/?q={}", key: "ddg" },
    Engine { name: "Google", url: "https://www.google.com/search?q={}", key: "g" },
    Engine { name: "Wikipedia", url: "https://en.wikipedia.org/w/index.php?search={}", key: "w" },
    Engine { name: "GitHub", url: "https://github.com/search?q={}", key: "gh" },
    Engine { name: "YouTube", url: "https://www.youtube.com/results?search_query={}", key: "yt" },
    Engine { name: "crates.io", url: "https://crates.io/search?q={}", key: "cr" },
    Engine { name: "NixOS packages", url: "https://search.nixos.org/packages?query={}", key: "nix" },
];

/// Engines for a web query, with a leading engine key (`gh rust async`) moving that engine
/// to the front and stripped from the query. `custom` is an extra URL template (`$ASHD_SEARCH_URL`).
pub fn web_targets(query: &str, custom: Option<&str>) -> Vec<(String, String, String)> {
    let query = query.trim();
    let (key, rest) = match query.split_once(char::is_whitespace) {
        Some((k, r)) => (Some(k), r.trim()),
        None => (None, query),
    };
    let keyed = key.and_then(|k| ENGINES.iter().position(|e| e.key == k)).filter(|_| !rest.is_empty());
    let (q, first) = match keyed {
        Some(ix) => (rest, Some(ix)),
        None => (query, None),
    };
    let mut out: Vec<(String, String, String)> = Vec::new();
    if let Some(tpl) = custom.filter(|t| t.contains("{}")) {
        out.push(("Default search".to_string(), tpl.replace("{}", &url_encode(q)), q.to_string()));
    }
    let order = first.into_iter().chain((0..ENGINES.len()).filter(|i| Some(*i) != first));
    for ix in order {
        let e = &ENGINES[ix];
        out.push((e.name.to_string(), e.url.replace("{}", &url_encode(q)), q.to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_map_to_modes() {
        assert_eq!(mode_for_prefix('='), Some(LauncherMode::Calc));
        assert_eq!(mode_for_prefix('/'), Some(LauncherMode::Files));
        assert_eq!(mode_for_prefix('>'), Some(LauncherMode::Run));
        assert_eq!(mode_for_prefix(':'), Some(LauncherMode::Emoji));
        assert_eq!(mode_for_prefix('!'), Some(LauncherMode::Web));
        assert_eq!(mode_for_prefix('f'), None);
    }

    #[test]
    fn highlight_ranges_merge_and_respect_utf8() {
        assert_eq!(highlight_ranges("Firefox", &[0, 1, 2]), vec![0..3]);
        assert_eq!(highlight_ranges("Firefox", &[0, 2, 3]), vec![0..1, 2..4]);
        assert_eq!(highlight_ranges("é-fox", &[0, 1]), vec![0..3]);
        assert!(highlight_ranges("abc", &[7]).is_empty());
    }

    #[test]
    fn substring_indices_are_case_insensitive() {
        let t = |s: &str| s.to_string();
        assert_eq!(substring_indices("Config.KDL", &[t("conf"), t("kdl")]), vec![0, 1, 2, 3, 7, 8, 9]);
        assert!(substring_indices("a", &[t("zz")]).is_empty());
    }

    #[test]
    fn sizes_and_ages() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(6_963), "6.8K");
        assert_eq!(human_size(1_468_006), "1.4M");
        assert_eq!(human_size(52_428_800), "50M");
        assert_eq!(human_age(5, false), "now");
        assert_eq!(human_age(900, false), "15 min");
        assert_eq!(human_age(3_700, false), "1 h");
        assert_eq!(human_age(4 * 86_400, false), "4 d");
        assert_eq!(human_age(2 * 86_400, true), "2d");
        assert_eq!(human_age(40 * 86_400, false), "1 mo");
    }

    #[test]
    fn run_lines_split_or_use_the_shell() {
        assert_eq!(plan_run("firejail --list", "zsh"), Some(vec!["firejail".into(), "--list".into()]));
        assert_eq!(plan_run("echo 'a b' c", "zsh"), Some(vec!["echo".into(), "a b".into(), "c".into()]));
        assert_eq!(plan_run("ls | wc -l", "zsh"), Some(vec!["zsh".into(), "-c".into(), "ls | wc -l".into()]));
        assert_eq!(plan_run("FOO=1 env", "zsh"), Some(vec!["zsh".into(), "-c".into(), "FOO=1 env".into()]));
        assert_eq!(plan_run("echo \"open", "sh"), Some(vec!["sh".into(), "-c".into(), "echo \"open".into()]));
        assert_eq!(plan_run("  ", "sh"), None);
        assert!(!needs_shell("kitty --title=a=b"));
    }

    #[test]
    fn web_targets_encode_and_honour_engine_keys() {
        let t = web_targets("rust async & await", None);
        assert_eq!(t[0].0, "DuckDuckGo");
        assert_eq!(t[0].1, "https://duckduckgo.com/?q=rust+async+%26+await");
        let t = web_targets("gh niri ipc", None);
        assert_eq!(t[0].0, "GitHub");
        assert_eq!(t[0].2, "niri ipc");
        assert_eq!(t.len(), ENGINES.len());
        // a lone word that happens to be a key is a query
        assert_eq!(web_targets("g", None)[0].0, "DuckDuckGo");
        let t = web_targets("x", Some("https://s.example/?q={}"));
        assert_eq!(t[0].1, "https://s.example/?q=x");
    }
}
