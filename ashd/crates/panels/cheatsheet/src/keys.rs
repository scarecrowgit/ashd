//! The data side of the cheatsheet: `keybinds.json` → sections of rows, key chips, the
//! instant filter and the column distribution. No GPUI.
//!
//! The file is what `modules/desktop/niri/binds.nix` renders to `/etc/ashd/keybinds.json`:
//! a list of `{keys, niri, title, group, action}`. `keys` is already in display form
//! (`Super+Shift+3`); `Mod` is mapped to `Super` anyway for hand-written files.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One entry of `keybinds.json`.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Bind {
    pub keys: String,
    pub title: String,
    pub group: String,
    /// The niri bind spelling (`Mod+Shift+3`); unused by the view, kept for the filter.
    #[serde(default)]
    pub niri: String,
    #[serde(default)]
    pub action: String,
}

/// Why the file could not be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// No file at the path (the NixOS option `ashd.shell.enable` is off, or a dev machine).
    Missing,
    /// Unreadable or not the expected JSON.
    Invalid(String),
}

/// `$ASHD_KEYBINDS`, else `<etc>/keybinds.json` (`/etc/ashd`, or `$ASHD_ETC_DIR`).
pub fn keybinds_path() -> PathBuf {
    match std::env::var_os("ASHD_KEYBINDS").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => ashd_tokens::state::etc_dir().join("keybinds.json"),
    }
}

pub fn parse(json: &str) -> Result<Vec<Bind>, serde_json::Error> {
    serde_json::from_str(json)
}

/// Read and parse `path`. Blocking: call it from a background task.
pub fn load(path: &Path) -> Result<Vec<Bind>, LoadError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| LoadError::Invalid(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(LoadError::Missing),
        Err(e) => Err(LoadError::Invalid(e.to_string())),
    }
}

// ── key chips ─────────────────────────────────────────────────────────────────

/// The chip text of one key name of a niri bind (`Left` → `←`, `Mod` → `Super`).
pub fn key_label(key: &str) -> String {
    let lower = key.to_ascii_lowercase();
    let fixed = match lower.as_str() {
        "mod" | "super" | "win" | "logo" | "meta" | "mod4" => "Super",
        "ctrl" | "control" => "Ctrl",
        "shift" => "Shift",
        "alt" | "mod1" => "Alt",
        "iso_level3_shift" | "mod5" => "AltGr",
        "left" => "←",
        "right" => "→",
        "up" => "↑",
        "down" => "↓",
        "return" | "enter" => "↵",
        "escape" | "esc" => "Esc",
        "space" => "Space",
        "tab" => "Tab",
        "backspace" => "⌫",
        "delete" => "Del",
        "home" => "Home",
        "end" => "End",
        "page_up" | "pageup" | "prior" => "PgUp",
        "page_down" | "pagedown" | "next" => "PgDn",
        "print" => "Print",
        "slash" => "/",
        "backslash" => "\\",
        "comma" => ",",
        "period" => ".",
        "minus" => "-",
        "equal" => "=",
        "plus" => "+",
        "semicolon" => ";",
        "apostrophe" => "'",
        "grave" => "`",
        "bracketleft" => "[",
        "bracketright" => "]",
        "xf86audioraisevolume" => "Vol +",
        "xf86audiolowervolume" => "Vol −",
        "xf86audiomute" => "Mute",
        "xf86audiomicmute" => "Mic mute",
        "xf86monbrightnessup" => "Bright +",
        "xf86monbrightnessdown" => "Bright −",
        "xf86audiostop" => "Stop",
        "xf86audioplay" => "Play",
        "xf86audionext" => "Next",
        "xf86audioprev" => "Prev",
        "wheelscrollup" => "Wheel ↑",
        "wheelscrolldown" => "Wheel ↓",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_string();
    }
    if let Some(rest) = lower.strip_prefix("xf86") {
        return rest.to_string();
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        // A single letter is drawn as the capital on the keycap.
        (Some(c), None) => c.to_uppercase().collect(),
        _ => key.to_string(),
    }
}

/// `Super+Shift+3` → `["Super", "Shift", "3"]`.
pub fn chips(keys: &str) -> Vec<String> {
    keys.split('+').map(str::trim).filter(|k| !k.is_empty()).map(key_label).collect()
}

// ── sections ──────────────────────────────────────────────────────────────────

/// What a row does in each workspace mode. The sheet has no mode switcher: one row has to
/// say what the key does in the strip and on the canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modes {
    /// The same in both modes (every plain niri action).
    Both,
    /// Only meaningful on a canvas workspace (`canvas-*` actions): drawn with a `canvas` tag.
    Canvas,
    /// `if-canvas { canvas-…; strip-action; }`: two lines, one per mode.
    Split { canvas: String, strip: String },
}

/// One row of a section: a title and one or more key combinations (`Super+H` and `Super+M`
/// for the same action share a row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The label; for [`Modes::Split`] rows the canvas half of it.
    pub title: String,
    pub combos: Vec<Vec<String>>,
    pub modes: Modes,
    /// Lower-case text the filter matches: the full title, the raw key names and the chip text.
    haystack: String,
}

impl Row {
    fn new(title: String, raw_keys: &[String], combos: Vec<Vec<String>>, action: &str) -> Self {
        let mut haystack = title.to_lowercase();
        for raw in raw_keys {
            haystack.push(' ');
            haystack.push_str(&raw.to_lowercase().replace("mod+", "super+"));
        }
        for chip in combos.iter().flatten() {
            haystack.push(' ');
            haystack.push_str(&chip.to_lowercase());
        }
        let (title, modes) = classify(&title, action);
        Row { title, combos, modes, haystack }
    }

    /// Display height: split rows carry two lines.
    pub fn height(&self) -> f32 {
        match self.modes {
            Modes::Split { .. } => SPLIT_ROW_H,
            _ => ROW_H,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub rows: Vec<Row>,
}

/// `Zoom in (strip: wider column)` → (`Zoom in`, `Wider column`).
fn split_title(title: &str) -> Option<(String, String)> {
    let inner = title.trim_end().strip_suffix(')')?;
    let at = inner.rfind("(strip:")?;
    let canvas = inner[..at].trim();
    let strip = inner[at + "(strip:".len()..].trim();
    (!canvas.is_empty() && !strip.is_empty()).then(|| (canvas.to_string(), upper_first(strip)))
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// A niri action as a sentence: `focus-column-left` → `Focus column left`,
/// `set-column-width "+10%"` → `Set column width +10%`.
fn humanize(statement: &str) -> String {
    upper_first(&statement.replace('-', " ").replace('"', ""))
}

/// The statements of an action: the body of `if-canvas { a; b; }` (or the action itself),
/// and whether it was guarded.
fn statements(action: &str) -> (bool, Vec<&str>) {
    let a = action.trim();
    let (guarded, body) = match a.strip_prefix("if-canvas") {
        Some(rest) => (true, rest.trim().trim_start_matches('{').trim_end_matches('}')),
        None => (false, a),
    };
    (guarded, body.split(';').map(str::trim).filter(|s| !s.is_empty()).collect())
}

fn is_canvas_statement(s: &str) -> bool {
    s.starts_with("canvas-") || s == "toggle-window-pinned"
}

/// The label and mode meaning of a row from its title and niri action.
fn classify(title: &str, action: &str) -> (String, Modes) {
    let (guarded, stmts) = statements(action);
    let strip_side: Vec<&&str> = stmts.iter().filter(|s| !is_canvas_statement(s)).collect();
    if guarded && !strip_side.is_empty() {
        return match split_title(title) {
            Some((canvas, strip)) => (canvas.clone(), Modes::Split { canvas, strip }),
            None => (title.to_string(), Modes::Split { canvas: title.to_string(), strip: humanize(strip_side[0]) }),
        };
    }
    if !stmts.is_empty() && strip_side.is_empty() {
        return (title.to_string(), Modes::Canvas);
    }
    // A title that spells out both modes without a guarded action (hand-written files).
    match split_title(title) {
        Some((canvas, strip)) => (canvas.clone(), Modes::Split { canvas, strip }),
        None => (title.to_string(), Modes::Both),
    }
}

/// A numbered series: `Mod+1`..`Mod+9` titled `Workspace 1`..`Workspace 9`.
struct Series {
    title_template: String,
    key_prefix: String,
    digits: Vec<u32>,
    raw: Vec<String>,
    /// Position of the first member in the group (the series row takes its place).
    first: usize,
}

/// Split `Mod+Ctrl+5` into (`Mod+Ctrl+`, 5) when the last key is a single digit.
fn digit_key(keys: &str) -> Option<(String, u32)> {
    let (prefix, last) = keys.rsplit_once('+').map_or(("", keys), |(p, l)| (p, l));
    let mut chars = last.chars();
    let d = chars.next()?.to_digit(10)?;
    chars.next().is_none().then(|| (if prefix.is_empty() { String::new() } else { format!("{prefix}+") }, d))
}

fn title_template(title: &str, digit: u32) -> Option<String> {
    let d = char::from_digit(digit, 10)?;
    (title.matches(d).count() == 1).then(|| title.replace(d, "N"))
}

fn group_rows(binds: &[&Bind]) -> Vec<Row> {
    // Pass 1: numbered series (three or more members with one digit title/keys template).
    let mut series: Vec<Series> = Vec::new();
    let mut slot: Vec<Option<usize>> = vec![None; binds.len()];
    for (ix, b) in binds.iter().enumerate() {
        let Some((prefix, d)) = digit_key(&b.keys) else { continue };
        let Some(template) = title_template(&b.title, d) else { continue };
        let at = match series.iter().position(|s| s.title_template == template && s.key_prefix == prefix) {
            Some(at) => at,
            None => {
                series.push(Series {
                    title_template: template,
                    key_prefix: prefix,
                    digits: Vec::new(),
                    raw: Vec::new(),
                    first: ix,
                });
                series.len() - 1
            }
        };
        series[at].digits.push(d);
        series[at].raw.push(b.keys.clone());
        slot[ix] = Some(at);
    }
    let is_series = |at: usize| series[at].digits.len() >= 3;

    // Pass 2: rows in file order; same-title binds share a row.
    let mut rows: Vec<(String, Vec<String>)> = Vec::new(); // (title, raw keys)
    let mut series_rows: Vec<(usize, usize)> = Vec::new(); // (row index, series)
    for (ix, b) in binds.iter().enumerate() {
        match slot[ix] {
            Some(at) if is_series(at) => {
                if series[at].first == ix {
                    series_rows.push((rows.len(), at));
                    rows.push((series[at].title_template.clone(), Vec::new()));
                }
            }
            _ => match rows.iter_mut().find(|(t, _)| t.eq_ignore_ascii_case(&b.title)) {
                Some((_, keys)) => keys.push(b.keys.clone()),
                None => rows.push((b.title.clone(), vec![b.keys.clone()])),
            },
        }
    }
    // The niri action of a row: that of its first bind (same-title binds share it).
    let action_of = |raw: &[String]| {
        raw.first().and_then(|k| binds.iter().find(|b| &b.keys == k)).map_or("", |b| b.action.as_str()).to_string()
    };
    rows.into_iter()
        .enumerate()
        .map(|(ix, (title, raw))| match series_rows.iter().find(|(r, _)| *r == ix) {
            Some(&(_, at)) => {
                let s = &series[at];
                let (lo, hi) = (s.digits.iter().min().copied().unwrap_or(1), s.digits.iter().max().copied().unwrap_or(9));
                let mut combo = chips(s.key_prefix.trim_end_matches('+'));
                combo.push(format!("{lo}…{hi}"));
                Row::new(title, &s.raw, vec![combo], &action_of(&s.raw))
            }
            None => {
                let combos = raw.iter().map(|k| chips(k)).collect();
                Row::new(title, &raw, combos, &action_of(&raw))
            }
        })
        .collect()
}

/// Group `binds` by `group` (first-appearance order), collapse digit series and same-title
/// binds.
pub fn sections(binds: &[Bind]) -> Vec<Section> {
    let mut names: Vec<&str> = Vec::new();
    for b in binds {
        if !names.contains(&b.group.as_str()) {
            names.push(&b.group);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let members: Vec<&Bind> = binds.iter().filter(|b| b.group == name).collect();
            Section { name: name.to_string(), rows: group_rows(&members) }
        })
        .collect()
}

// ── filter ────────────────────────────────────────────────────────────────────

/// Query tokens: lower-case words; `mod` is spelled `super` in the haystack.
fn tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|w| {
            let w = w.to_lowercase();
            if w == "mod" { "super".to_string() } else { w.replace("mod+", "super+") }
        })
        .collect()
}

/// The sections to draw: rows narrowed by `query` (every word must occur in the title or
/// the keys), empty sections dropped.
pub fn visible<'a>(sections: &'a [Section], query: &str) -> Vec<(&'a Section, Vec<&'a Row>)> {
    let words = tokens(query);
    sections
        .iter()
        .filter_map(|s| {
            let rows: Vec<&Row> = s.rows.iter().filter(|r| words.iter().all(|w| r.haystack.contains(w.as_str()))).collect();
            (!rows.is_empty()).then_some((s, rows))
        })
        .collect()
}

// ── layout ────────────────────────────────────────────────────────────────────

pub const ROW_H: f32 = 26.0;
/// A row with a canvas line and a strip line.
pub const SPLIT_ROW_H: f32 = 40.0;
/// Section title (mono 10 caps, line 14) + the 6 px gap below it.
pub const SECTION_HEAD_H: f32 = 20.0;

/// Approximate height of a section with these rows.
pub fn section_height(rows: &[&Row]) -> f32 {
    SECTION_HEAD_H + rows.iter().map(|r| r.height()).sum::<f32>()
}

/// Masonry: each section, in order, goes to the currently shortest column (ties: the
/// leftmost). Returns the section indices of every column.
pub fn masonry(heights: &[f32], columns: usize) -> Vec<Vec<usize>> {
    let columns = columns.max(1);
    let mut cols: Vec<Vec<usize>> = vec![Vec::new(); columns];
    let mut used = vec![0.0f32; columns];
    for (ix, h) in heights.iter().enumerate() {
        let target = (0..columns).fold(0, |best, c| if used[c] < used[best] { c } else { best });
        cols[target].push(ix);
        used[target] += h;
    }
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(group: &str, keys: &str, title: &str) -> Bind {
        Bind { keys: keys.into(), title: title.into(), group: group.into(), niri: String::new(), action: String::new() }
    }

    #[test]
    fn chips_split_and_use_glyphs() {
        assert_eq!(chips("Super+Shift+3"), ["Super", "Shift", "3"]);
        assert_eq!(chips("Mod+Ctrl+Up"), ["Super", "Ctrl", "↑"]);
        assert_eq!(chips("Super+Return"), ["Super", "↵"]);
        assert_eq!(chips("Super+Slash"), ["Super", "/"]);
        assert_eq!(chips("Super+BracketLeft"), ["Super", "["]);
        assert_eq!(chips("Super+Alt+Escape"), ["Super", "Alt", "Esc"]);
        assert_eq!(chips("Super+q"), ["Super", "Q"]);
        assert_eq!(chips("XF86AudioRaiseVolume"), ["Vol +"]);
        assert_eq!(chips("XF86AudioStop"), ["Stop"]);
        assert_eq!(chips("XF86Sleep"), ["sleep"]);
        assert_eq!(chips("Super+F11"), ["Super", "F11"]);
        assert!(chips("").is_empty());
    }

    #[test]
    fn groups_keep_file_order_and_series_collapse() {
        let mut binds = vec![bind("Apps", "Super+Space", "Launcher"), bind("Workspaces", "Super+Ctrl+Up", "Workspace up")];
        for n in 1..=9 {
            binds.push(bind("Workspaces", &format!("Super+{n}"), &format!("Workspace {n}")));
        }
        for n in 1..=9 {
            binds.push(bind("Workspaces", &format!("Super+Ctrl+{n}"), &format!("Move column to workspace {n}")));
        }
        binds.push(bind("Apps", "Super+E", "File manager"));
        let s = sections(&binds);
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Apps", "Workspaces"]);
        assert_eq!(s[0].rows.len(), 2);
        let ws = &s[1].rows;
        assert_eq!(ws.len(), 3, "up + two series");
        assert_eq!(ws[1].title, "Workspace N");
        assert_eq!(ws[1].combos, vec![vec!["Super".to_string(), "1…9".to_string()]]);
        assert_eq!(ws[2].title, "Move column to workspace N");
        assert_eq!(ws[2].combos[0], ["Super", "Ctrl", "1…9"]);
    }

    #[test]
    fn two_numbered_binds_are_not_a_series() {
        let binds = vec![bind("W", "Super+1", "Desk 1"), bind("W", "Super+2", "Desk 2")];
        assert_eq!(sections(&binds)[0].rows.len(), 2);
    }

    #[test]
    fn same_title_shares_a_row() {
        let binds = vec![
            bind("Windows", "Super+H", "Hide to scratchpad"),
            bind("Windows", "Super+Escape", "Overview"),
            bind("Windows", "Super+M", "Hide to scratchpad"),
        ];
        let rows = &sections(&binds)[0].rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].combos, vec![vec!["Super".to_string(), "H".to_string()], vec!["Super".to_string(), "M".to_string()]]);
    }

    #[test]
    fn filter_matches_title_and_keys_and_drops_empty_sections() {
        let binds = vec![
            bind("Apps", "Super+Space", "Launcher"),
            bind("Layout", "Super+Minus", "Narrower column"),
            bind("Screenshots", "Super+Shift+3", "Screenshot screen"),
        ];
        let s = sections(&binds);
        let names = |q: &str| visible(&s, q).iter().map(|(s, r)| (s.name.clone(), r.len())).collect::<Vec<_>>();
        assert_eq!(names("").len(), 3);
        assert_eq!(names("launch"), [("Apps".to_string(), 1)]);
        assert_eq!(names("NARROW"), [("Layout".to_string(), 1)]);
        // by keys: raw spelling, chip glyph, `mod` as an alias of Super
        assert_eq!(names("shift 3"), [("Screenshots".to_string(), 1)]);
        assert_eq!(names("super+shift+3"), [("Screenshots".to_string(), 1)]);
        assert_eq!(names("mod+space"), [("Apps".to_string(), 1)]);
        assert_eq!(names("minus"), [("Layout".to_string(), 1)]);
        assert_eq!(names("nonsense"), []);
        // every word has to match
        assert_eq!(names("launcher screen"), []);
    }

    fn with_action(group: &str, keys: &str, title: &str, action: &str) -> Bind {
        Bind { action: action.into(), ..bind(group, keys, title) }
    }

    #[test]
    fn every_group_shows_without_a_mode() {
        let binds = vec![bind("Layout", "Super+C", "Center column"), bind("Canvas", "Super+0", "Fit all windows")];
        let s = sections(&binds);
        assert_eq!(visible(&s, "").len(), 2);
        assert_eq!(visible(&s, "fit").len(), 1);
    }

    #[test]
    fn modes_are_spelled_out_per_row() {
        let rows = |binds: Vec<Bind>| sections(&binds).remove(0).rows;
        // title carries both meanings: split into a canvas line and a strip line
        let r = &rows(vec![with_action(
            "Canvas",
            "Super+Equal",
            "Zoom in (strip: wider column)",
            r#"if-canvas { canvas-zoom-in; set-column-width "+10%"; }"#,
        )])[0];
        assert_eq!(r.title, "Zoom in");
        assert_eq!(r.modes, Modes::Split { canvas: "Zoom in".into(), strip: "Wider column".into() });
        assert_eq!(r.height(), SPLIT_ROW_H);
        // the strip side of a title without the "(strip: …)" spelling comes from the action
        let r = &rows(vec![with_action(
            "Columns",
            "Super+Shift+WheelScrollUp",
            "Scroll left (wheel)",
            "if-canvas { canvas-pan-left; focus-column-left; }",
        )])[0];
        assert_eq!(r.modes, Modes::Split { canvas: "Scroll left (wheel)".into(), strip: "Focus column left".into() });
        // canvas-only actions, guarded or not
        for action in ["if-canvas { canvas-pan-left; }", "canvas-home", "toggle-window-pinned", "canvas-set-zoom 1.0"] {
            assert_eq!(rows(vec![with_action("C", "Super+A", "Do it", action)])[0].modes, Modes::Canvas, "{action}");
        }
        // everything else (including the mode toggle itself) is the same in both
        for action in ["toggle-workspace-mode", "focus-column-left", "spawn \"ashd\" \"launcher\"", ""] {
            let r = &rows(vec![with_action("C", "Super+A", "Do it", action)])[0];
            assert_eq!((r.modes.clone(), r.height()), (Modes::Both, ROW_H), "{action}");
        }
        // the filter still sees the whole title (both halves)
        let s = sections(&[with_action("Canvas", "Super+Equal", "Zoom in (strip: wider column)", "if-canvas { canvas-zoom-in; set-column-width \"+10%\"; }")]);
        assert_eq!(visible(&s, "wider").len(), 1);
        assert_eq!(visible(&s, "zoom").len(), 1);
    }

    #[test]
    fn section_height_counts_split_rows_double() {
        let binds = vec![
            bind("A", "Super+A", "One"),
            with_action("A", "Super+B", "Two (strip: three)", "if-canvas { canvas-fit; center-column; }"),
        ];
        let s = sections(&binds);
        let rows: Vec<&Row> = s[0].rows.iter().collect();
        assert_eq!(section_height(&rows), SECTION_HEAD_H + ROW_H + SPLIT_ROW_H);
    }

    #[test]
    fn masonry_balances_columns() {
        let h = [5.0, 1.0, 1.0, 1.0, 4.0, 1.0];
        let cols = masonry(&h, 3);
        assert_eq!(cols, vec![vec![0], vec![1, 3, 5], vec![2, 4]]);
        assert_eq!(masonry(&[], 3), vec![Vec::<usize>::new(); 3]);
        assert_eq!(masonry(&[1.0, 1.0], 0).len(), 1, "at least one column");
    }

    #[test]
    fn load_reports_missing_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("none.json")), Err(LoadError::Missing));
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{\"nope\": 1}").unwrap();
        assert!(matches!(load(&bad), Err(LoadError::Invalid(_))));
        let ok = dir.path().join("ok.json");
        std::fs::write(&ok, r#"[{"keys":"Super+Space","niri":"Mod+Space","title":"Launcher","group":"Apps","action":"spawn"}]"#)
            .unwrap();
        assert_eq!(load(&ok).unwrap()[0].title, "Launcher");
        // niri / action are optional
        assert!(parse(r#"[{"keys":"a","title":"t","group":"g"}]"#).is_ok());
    }

    /// The fixture (a copy of the generated `/etc/ashd/keybinds.json`) parses and covers the
    /// project's groups; every mode-dependent bind says what it does in each mode.
    #[test]
    fn fixture_parses() {
        let binds = parse(include_str!("../fixtures/keybinds.json")).unwrap();
        let s = sections(&binds);
        let names: Vec<_> = s.iter().map(|s| s.name.as_str()).collect();
        for want in ["Apps", "Windows", "Columns", "Workspaces", "Canvas", "Scratchpad", "Shell", "Media", "Screenshots", "Session"] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
        // the numbered workspaces collapse
        let ws = s.iter().find(|s| s.name == "Workspaces").unwrap();
        assert!(ws.rows.len() <= 8, "{} workspace rows", ws.rows.len());
        // no row of an `if-canvas` bind is drawn as "the same in both modes"
        for b in binds.iter().filter(|b| b.action.contains("if-canvas")) {
            let row = s
                .iter()
                .flat_map(|s| &s.rows)
                .find(|r| r.combos.iter().any(|c| *c == chips(&b.keys)))
                .unwrap_or_else(|| panic!("{} has no row", b.keys));
            assert_ne!(row.modes, Modes::Both, "{}: {}", b.keys, b.title);
            if let Modes::Split { canvas, strip } = &row.modes {
                assert!(!canvas.is_empty() && !strip.is_empty(), "{}", b.title);
            }
        }
    }
}
