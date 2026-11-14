//! Design tokens for ashd. No GPUI: the CLI, the lock screen and the theme renderers use
//! this crate directly; `ashd-ui` turns [`Tokens`] into its GPUI `Theme` global.
//!
//! Source of truth: `ashd/tokens/tokens.json` (embedded at compile time), whose values
//! come from `docs/design/TOKENS.md`. Layout constants live in [`metrics`], motion in
//! [`motion`], theme-file renderers (kitty, starship, nvim, GTK, niri) in [`render`],
//! the persisted current theme + `theme apply` file writer in [`state`].
//!
//! ```
//! use ashd_tokens::{ThemeName, tokens};
//! let ash = tokens(ThemeName::Ash);
//! assert_eq!(ash.colors.accent.hex(), "#E0A458");
//! ```

mod color;
pub mod metrics;
pub mod motion;
mod parse;
pub mod render;
pub mod state;

use std::{fmt, str::FromStr, sync::OnceLock};

pub use color::Rgba;
pub use motion::{Bezier, Spring};
use serde::{Deserialize, Serialize};

/// The embedded `tokens/tokens.json`.
pub const TOKENS_JSON: &str = include_str!("../../../tokens/tokens.json");

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("tokens.json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("tokens.json: missing `{0}`")]
    Missing(String),
    #[error("tokens.json: {0}")]
    Value(String),
}

// ── theme names ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Ash,
    Ink,
    Midnight,
    Moss,
    Dusk,
}

impl ThemeName {
    pub const ALL: [ThemeName; 5] = [Self::Ash, Self::Ink, Self::Midnight, Self::Moss, Self::Dusk];

    /// Lowercase name as used in tokens.json, file names and the CLI (`ash`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Ash => "ash",
            Self::Ink => "ink",
            Self::Midnight => "midnight",
            Self::Moss => "moss",
            Self::Dusk => "dusk",
        }
    }

    /// Display name (`Ash`).
    pub fn title(self) -> &'static str {
        match self {
            Self::Ash => "Ash",
            Self::Ink => "Ink",
            Self::Midnight => "Midnight",
            Self::Moss => "Moss",
            Self::Dusk => "Dusk",
        }
    }

    pub fn next(self) -> Self {
        let ix = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(ix + 1) % Self::ALL.len()]
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(s.trim()))
    }
}

impl fmt::Display for ThemeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ThemeName {
    type Err = TokenError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_label(s).ok_or_else(|| {
            TokenError::Value(format!("unknown theme `{s}` (expected ash|ink|midnight|moss|dusk)"))
        })
    }
}

// ── per-theme tokens ──────────────────────────────────────────────────────────

/// The 17 semantic colour tokens (`--bg` … `--syn-comment`). Same names in every theme.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Colors {
    pub bg: Rgba,
    pub surface: Rgba,
    pub surface_2: Rgba,
    pub line: Rgba,
    pub frame: Rgba,
    pub fg: Rgba,
    pub fg_muted: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub err: Rgba,
    pub info: Rgba,
    pub syn_keyword: Rgba,
    pub syn_string: Rgba,
    pub syn_number: Rgba,
    pub syn_comment: Rgba,
}

impl Colors {
    /// Token names as in tokens.json / CSS (`surface-2`), in declaration order.
    pub const NAMES: [&'static str; 17] = [
        "bg", "surface", "surface-2", "line", "frame", "fg", "fg-muted", "accent", "on-accent", "ok",
        "warn", "err", "info", "syn-keyword", "syn-string", "syn-number", "syn-comment",
    ];

    /// `(css-name, colour)` pairs in [`Self::NAMES`] order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Rgba)> + '_ {
        Self::NAMES.into_iter().zip(self.as_array())
    }

    pub fn as_array(&self) -> [Rgba; 17] {
        [
            self.bg, self.surface, self.surface_2, self.line, self.frame, self.fg, self.fg_muted,
            self.accent, self.on_accent, self.ok, self.warn, self.err, self.info, self.syn_keyword,
            self.syn_string, self.syn_number, self.syn_comment,
        ]
    }

    pub fn get(&self, css_name: &str) -> Option<Rgba> {
        self.iter().find(|(n, _)| *n == css_name).map(|(_, c)| c)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fonts {
    /// UI text (`Inter` in Ash).
    pub sans: String,
    /// Data: time, sizes, paths, hotkeys, counters, labels (`JetBrains Mono` in Ash).
    pub mono: String,
}

/// Corner radii in px: panel (rail, dock, popups) > window > control.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Radii {
    pub panel: f32,
    pub window: f32,
    pub control: f32,
}

/// `attached`: panels touch the screen edges (Ash). `floating`: panels are inset by
/// `gap` and rounded (Ink, Midnight, Moss, Dusk).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutVariant {
    Attached,
    Floating,
}

/// Rail look: numbers (Ash, Moss), thumbnails (Ink, Dusk), pills (Midnight).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RailVariant {
    Num,
    Thumb,
    Pill,
}

/// Bottom row look: one full-width bar (Ash), split status/dock (Ink, Moss), pills
/// (Midnight, Dusk).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockVariant {
    Bar,
    Split,
    Pill,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScratchpadVariant {
    Stack,
    Tabs,
}

/// Window mode of a workspace: niri's scrolling strip or the infinite canvas.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    #[default]
    Scroll,
    Canvas,
}

/// Per-theme structure (docs/design/TOKENS.md §6 "Theme structure").
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shape {
    pub layout: LayoutVariant,
    /// Gap between panels / windows and inset of floating panels from the screen edge.
    pub gap: f32,
    pub rail: RailVariant,
    pub rail_width: f32,
    pub dock: DockVariant,
    pub dock_height: f32,
    pub scratchpad: ScratchpadVariant,
    pub default_mode: WindowMode,
    /// Label role (mono 10.5) in caps — Ash, Ink, Moss — or lower case as written with
    /// +0.01em tracking — Midnight, Dusk (desk-07/09: no `text-transform`).
    pub label_caps: bool,
}

/// Everything that differs between themes.
#[derive(Clone, PartialEq, Debug)]
pub struct Tokens {
    pub name: ThemeName,
    pub colors: Colors,
    pub fonts: Fonts,
    pub radius: Radii,
    pub shape: Shape,
}

// ── shared tokens ─────────────────────────────────────────────────────────────

/// Type roles (TOKENS.md §3), px. Line heights are in [`metrics::type_scale`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FontSizes {
    pub display: f32,
    pub title: f32,
    pub body: f32,
    pub ui: f32,
    pub label: f32,
    pub code: f32,
    pub numeric: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FontWeights {
    pub regular: u16,
    pub medium: u16,
    pub semibold: u16,
}

/// A CSS box-shadow (`x y blur [spread] color`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
}

/// Only overlays cast shadows ("lines, not shadows").
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shadows {
    /// Menus, launcher, popups: `0 24px 60px rgba(0,0,0,.45)`.
    pub overlay: Shadow,
    /// One large modal: `0 30px 80px rgba(0,0,0,.55)`.
    pub quicklook: Shadow,
    /// Flyouts, toasts, submenus: `0 18px 40px rgba(0,0,0,.35)`.
    pub flyout: Shadow,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Scrims {
    pub modal: Rgba,
    /// Power / session dialogs: 60 % dim.
    pub power: Rgba,
}

/// Durations in milliseconds (TOKENS.md §9). See also [`motion`] for the const
/// versions and the per-animation timings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Durations {
    pub instant: u64,
    pub fast: u64,
    pub flyout: u64,
    pub base: u64,
    pub slow: u64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Easings {
    pub standard: Bezier,
    pub enter: Bezier,
    pub exit: Bezier,
    pub toast: Bezier,
    pub out_cubic: Bezier,
    pub linear: Bezier,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Springs {
    pub view: Spring,
    pub pane_resize: Spring,
    pub workspace_switch: Spring,
    pub canvas_zoom: Spring,
    pub canvas_pan: Spring,
    pub scratchpad: Spring,
    pub shake: Spring,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Shared {
    /// Spacing scale: 2 4 6 8 10 12 16 24 32.
    pub space: Vec<f32>,
    pub font_size: FontSizes,
    pub font_weight: FontWeights,
    pub border_width: f32,
    pub shadow: Shadows,
    pub scrim: Scrims,
    pub duration: Durations,
    pub easing: Easings,
    pub spring: Springs,
}

/// The whole tokens file.
#[derive(Clone, PartialEq, Debug)]
pub struct TokenSet {
    pub default: ThemeName,
    pub shared: Shared,
    /// Indexed by `ThemeName as usize` (same order as [`ThemeName::ALL`]).
    pub themes: [Tokens; 5],
}

impl TokenSet {
    /// Parse a tokens.json document.
    pub fn from_json(json: &str) -> Result<Self, TokenError> {
        parse::parse(json)
    }

    /// The embedded tokens.json, parsed once. Panics only if the embedded file is broken,
    /// which `cargo test -p ashd-tokens` rules out.
    pub fn builtin() -> &'static TokenSet {
        static SET: OnceLock<TokenSet> = OnceLock::new();
        SET.get_or_init(|| Self::from_json(TOKENS_JSON).expect("embedded tokens/tokens.json is valid"))
    }

    pub fn get(&self, name: ThemeName) -> &Tokens {
        &self.themes[name as usize]
    }
}

/// Built-in tokens of one theme.
pub fn tokens(name: ThemeName) -> &'static Tokens {
    TokenSet::builtin().get(name)
}

/// Built-in shared tokens (type scale, durations, shadows …).
pub fn shared() -> &'static Shared {
    &TokenSet::builtin().shared
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses() {
        let set = TokenSet::from_json(TOKENS_JSON).unwrap();
        assert_eq!(set.default, ThemeName::Ash);
        for (ix, name) in ThemeName::ALL.into_iter().enumerate() {
            assert_eq!(set.themes[ix].name, name);
        }
    }

    #[test]
    fn every_theme_has_17_colours() {
        let raw: serde_json::Value = serde_json::from_str(TOKENS_JSON).unwrap();
        for name in ThemeName::ALL {
            let colors = raw["themes"][name.label()]["color"].as_object().unwrap();
            assert_eq!(colors.len(), 17, "{name}");
            for css in Colors::NAMES {
                assert!(colors.contains_key(css), "{name} lacks {css}");
            }
            let t = tokens(name);
            assert_eq!(t.colors.iter().count(), 17);
            for (css, c) in t.colors.iter() {
                assert_eq!(c.hex(), colors[css].as_str().unwrap().to_uppercase(), "{name} {css}");
            }
        }
    }

    #[test]
    fn ash_reference_values() {
        let ash = tokens(ThemeName::Ash);
        assert_eq!(ash.colors.accent, Rgba::rgb(0xE0A458));
        assert_eq!(ash.colors.accent.hex(), "#E0A458");
        assert_eq!(ash.colors.surface.hex(), "#161615");
        assert_eq!(ash.fonts.sans, "Inter");
        assert_eq!(ash.fonts.mono, "JetBrains Mono");
        assert_eq!(ash.radius, Radii { panel: 0.0, window: 0.0, control: 0.0 });
        assert_eq!(ash.shape.layout, LayoutVariant::Attached);
        assert_eq!(ash.shape.rail_width, metrics::RAIL_W);
        assert_eq!(ash.shape.dock_height, metrics::BAR_H);
        assert_eq!(tokens(ThemeName::Dusk).radius.panel, 12.0);
    }

    #[test]
    fn contrast_matches_design() {
        // TOKENS.md: Ash fg on surface 12.6:1, fg-muted 5.9:1.
        let c = &tokens(ThemeName::Ash).colors;
        assert!((c.fg.contrast(c.surface) - 12.6).abs() < 0.2);
        assert!((c.fg_muted.contrast(c.surface) - 5.9).abs() < 0.2);
    }

    #[test]
    fn motion_consts_match_json() {
        let s = shared();
        assert_eq!(s.duration.instant, motion::DUR_INSTANT);
        assert_eq!(s.duration.fast, motion::DUR_FAST);
        assert_eq!(s.duration.flyout, motion::DUR_FLYOUT);
        assert_eq!(s.duration.base, motion::DUR_BASE);
        assert_eq!(s.duration.slow, motion::DUR_SLOW);
        assert_eq!(s.easing.standard, motion::EASE_STANDARD);
        assert_eq!(s.easing.enter, motion::EASE_OUT_EXPO);
        assert_eq!(s.easing.exit, motion::EASE_OUT_QUAD);
        assert_eq!(s.easing.out_cubic, motion::EASE_OUT_CUBIC);
        assert_eq!(s.spring.view, motion::SPRING_VIEW);
        assert_eq!(s.spring.workspace_switch, motion::SPRING_WORKSPACE);
        assert_eq!(s.spring.shake, motion::SPRING_SHAKE);
        assert_eq!(s.shadow.overlay.blur, 60.0);
        assert_eq!(s.scrim.power.alpha(), 0.6);
    }

    /// Cross-check every value against docs/design/TOKENS.md (skipped when the docs are not
    /// next to the crate, e.g. in a crane source-filtered build).
    #[test]
    fn matches_tokens_md() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/design/TOKENS.md");
        let Ok(md) = std::fs::read_to_string(&path) else {
            eprintln!("TOKENS.md not found, skipping");
            return;
        };
        // table rows of one `## N.` section as trimmed cells
        let rows = |section: &str| -> Vec<Vec<String>> {
            let body = md.split(&format!("\n## {section}")).nth(1).unwrap_or_else(|| panic!("section {section}"));
            let body = body.split("\n## ").next().unwrap();
            body.lines()
                .filter(|l| l.starts_with('|') && !l.starts_with("|---"))
                .skip(1)
                .map(|l| l.trim_matches('|').split('|').map(|c| c.trim().trim_matches('`').to_string()).collect())
                .collect()
        };
        let themes = ThemeName::ALL;
        // 1. colours: `--bg` | role | 5 values (each maybe followed by "(12.6:1)")
        let mut n = 0;
        for row in rows("1.") {
            let css = row[0].trim_start_matches("--");
            for (i, name) in themes.into_iter().enumerate() {
                let hex = row[2 + i].split_whitespace().next().unwrap();
                assert_eq!(tokens(name).colors.get(css).unwrap().hex(), hex.to_uppercase(), "{name} {css}");
            }
            n += 1;
        }
        assert_eq!(n, 17);
        // 2. fonts
        for row in rows("2.") {
            let name = ThemeName::from_label(&row[0]).unwrap();
            assert_eq!((tokens(name).fonts.sans.as_str(), tokens(name).fonts.mono.as_str()), (row[1].as_str(), row[2].as_str()));
        }
        // 4. spacing
        let space: Vec<f32> = rows("4.").iter().map(|r| r[1].trim_end_matches("px").parse().unwrap()).collect();
        assert_eq!(shared().space, space);
        // 5. radii
        for row in rows("5.") {
            let name = ThemeName::from_label(&row[0]).unwrap();
            let v: Vec<f32> = row[1..4].iter().map(|c| c.parse().unwrap()).collect();
            let r = tokens(name).radius;
            assert_eq!(vec![r.panel, r.window, r.control], v, "{name} radii");
        }
        // 6. structure: layout | rail (variant . width) | dock (variant . height) | scratchpad | radius | gap | default mode
        for row in rows("6.").into_iter().filter(|r| ThemeName::from_label(&r[0]).is_some()) {
            let name = ThemeName::from_label(&row[0]).unwrap();
            let sh = tokens(name).shape;
            let layout = if sh.layout == LayoutVariant::Attached { "attached" } else { "floating" };
            assert_eq!(row[1], layout, "{name}");
            let rail = format!("{} . {}px", serde_json::to_string(&sh.rail).unwrap().trim_matches('"'), sh.rail_width);
            assert_eq!(row[2].replace('\u{b7}', "."), rail, "{name} rail");
            let dock = format!("{} . {}px", serde_json::to_string(&sh.dock).unwrap().trim_matches('"'), sh.dock_height);
            assert_eq!(row[3].replace('\u{b7}', "."), dock, "{name} dock");
            assert_eq!(row[4], serde_json::to_string(&sh.scratchpad).unwrap().trim_matches('"'), "{name}");
            assert_eq!(row[5], format!("{}px", tokens(name).radius.panel), "{name} panel radius");
            assert_eq!(row[6], format!("{}px", sh.gap), "{name} gap");
            let mode = if sh.default_mode == WindowMode::Canvas { "canvas" } else { "niri" };
            assert_eq!(row[7], mode, "{name} mode");
        }
        // 9. springs
        let sp = shared().spring;
        for (name, spring) in [
            ("view", sp.view), ("workspace", sp.workspace_switch), ("canvas zoom", sp.canvas_zoom),
            ("canvas pan", sp.canvas_pan), ("scratchpad", sp.scratchpad), ("wrong password", sp.shake),
        ] {
            let row = md.lines().find(|l| l.to_lowercase().starts_with(&format!("| {name}"))).unwrap_or_else(|| panic!("{name}"));
            let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
            assert_eq!(cells[1].parse::<f32>().unwrap(), spring.damping, "{name}");
            assert_eq!(cells[2].parse::<f32>().unwrap(), spring.stiffness, "{name}");
        }
    }

    #[test]
    fn theme_name_roundtrip() {
        for n in ThemeName::ALL {
            assert_eq!(n.label().parse::<ThemeName>().unwrap(), n);
            assert_eq!(serde_json::to_string(&n).unwrap(), format!("\"{}\"", n.label()));
        }
        assert_eq!(ThemeName::Dusk.next(), ThemeName::Ash);
        assert!("nope".parse::<ThemeName>().is_err());
    }
}
