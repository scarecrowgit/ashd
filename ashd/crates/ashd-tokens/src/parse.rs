//! tokens.json → typed [`TokenSet`]. The JSON uses CSS-ish strings (`"12px"`,
//! `"cubic-bezier(.2,.8,.2,1)"`, `"rgba(0,0,0,.45)"`); they are converted here once.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{
    Bezier, Colors, Durations, Easings, FontSizes, FontWeights, Fonts, Radii, Rgba, Scrims, Shadow,
    Shadows, Shape, Shared, Spring, Springs, ThemeName, TokenError, TokenSet, Tokens,
};

#[derive(Deserialize)]
struct RawFile {
    #[serde(rename = "$meta")]
    meta: RawMeta,
    shared: RawShared,
    themes: BTreeMap<String, RawTheme>,
}

#[derive(Deserialize)]
struct RawMeta {
    default: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawShared {
    space: BTreeMap<String, String>,
    font_size: BTreeMap<String, String>,
    font_weight: BTreeMap<String, u16>,
    border: BTreeMap<String, String>,
    shadow: BTreeMap<String, String>,
    scrim: BTreeMap<String, String>,
    duration: BTreeMap<String, String>,
    easing: BTreeMap<String, String>,
    spring: BTreeMap<String, RawSpring>,
}

#[derive(Deserialize)]
struct RawSpring {
    damping: f32,
    stiffness: f32,
}

#[derive(Deserialize)]
struct RawTheme {
    color: BTreeMap<String, String>,
    font: RawFont,
    radius: BTreeMap<String, String>,
    shape: RawShape,
}

#[derive(Deserialize)]
struct RawFont {
    sans: String,
    mono: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawShape {
    layout: crate::LayoutVariant,
    gap: String,
    rail: RawRail,
    dock: RawDock,
    scratchpad: crate::ScratchpadVariant,
    default_mode: crate::WindowMode,
    /// Label role in caps (Ash, Ink, Moss) or as written (Midnight, Dusk); default caps.
    #[serde(default = "label_caps_default")]
    label_caps: bool,
}

fn label_caps_default() -> bool {
    true
}

#[derive(Deserialize)]
struct RawRail {
    variant: crate::RailVariant,
    width: String,
}

#[derive(Deserialize)]
struct RawDock {
    variant: crate::DockVariant,
    height: String,
}

fn get<'a, V>(map: &'a BTreeMap<String, V>, path: &str, key: &str) -> Result<&'a V, TokenError> {
    map.get(key).ok_or_else(|| TokenError::Missing(format!("{path}.{key}")))
}

/// `"12px"` / `"12"` → 12.0
pub(crate) fn px(s: &str) -> Result<f32, TokenError> {
    let t = s.trim();
    t.strip_suffix("px").unwrap_or(t).trim().parse().map_err(|_| TokenError::Value(format!("bad length `{s}`")))
}

/// `"120ms"` → 120
fn ms(s: &str) -> Result<u64, TokenError> {
    let t = s.trim();
    t.strip_suffix("ms")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| TokenError::Value(format!("bad duration `{s}`")))
}

/// `"cubic-bezier(.2,.8,.2,1)"` / `"linear"` → Bezier
fn bezier(s: &str) -> Result<Bezier, TokenError> {
    let t = s.trim();
    if t == "linear" {
        return Ok(Bezier::LINEAR);
    }
    let inner = t
        .strip_prefix("cubic-bezier(")
        .and_then(|r| r.strip_suffix(')'))
        .ok_or_else(|| TokenError::Value(format!("bad easing `{s}`")))?;
    let v: Vec<f32> = inner
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| TokenError::Value(format!("bad easing `{s}`")))?;
    match v.as_slice() {
        [a, b, c, d] => Ok(Bezier(*a, *b, *c, *d)),
        _ => Err(TokenError::Value(format!("bad easing `{s}`"))),
    }
}

/// `"0 24px 60px rgba(0,0,0,.45)"` → Shadow
fn shadow(s: &str) -> Result<Shadow, TokenError> {
    let bad = || TokenError::Value(format!("bad shadow `{s}`"));
    let color_at = s.find("rgba(").or_else(|| s.find('#')).ok_or_else(bad)?;
    let color = Rgba::parse(&s[color_at..])?;
    let lens: Vec<f32> = s[..color_at].split_whitespace().map(px).collect::<Result<_, _>>()?;
    let at = |i: usize| lens.get(i).copied().unwrap_or(0.0);
    if lens.len() < 2 {
        return Err(bad());
    }
    Ok(Shadow { x: at(0), y: at(1), blur: at(2), spread: at(3), color })
}

pub(crate) fn parse(json: &str) -> Result<TokenSet, TokenError> {
    let raw: RawFile = serde_json::from_str(json)?;
    let default: ThemeName = raw.meta.default.parse()?;
    let s = &raw.shared;

    let mut space: Vec<f32> = s.space.values().map(|v| px(v)).collect::<Result<_, _>>()?;
    space.sort_by(f32::total_cmp);

    let fs = |k: &str| get(&s.font_size, "shared.font-size", k).and_then(|v| px(v));
    let fw = |k: &str| get(&s.font_weight, "shared.font-weight", k).copied();
    let sh = |k: &str| get(&s.shadow, "shared.shadow", k).and_then(|v| shadow(v));
    let sc = |k: &str| get(&s.scrim, "shared.scrim", k).and_then(|v| Rgba::parse(v));
    let du = |k: &str| get(&s.duration, "shared.duration", k).and_then(|v| ms(v));
    let ea = |k: &str| get(&s.easing, "shared.easing", k).and_then(|v| bezier(v));
    let sp = |k: &str| {
        get(&s.spring, "shared.spring", k).map(|r| Spring { damping: r.damping, stiffness: r.stiffness })
    };

    let shared = Shared {
        space,
        font_size: FontSizes {
            display: fs("display")?,
            title: fs("title")?,
            body: fs("body")?,
            ui: fs("ui")?,
            label: fs("label")?,
            code: fs("code")?,
            numeric: fs("numeric")?,
        },
        font_weight: FontWeights { regular: fw("regular")?, medium: fw("medium")?, semibold: fw("semibold")? },
        border_width: px(get(&s.border, "shared.border", "width")?)?,
        shadow: Shadows { overlay: sh("overlay")?, quicklook: sh("quicklook")?, flyout: sh("flyout")? },
        scrim: Scrims { modal: sc("modal")?, power: sc("power")? },
        duration: Durations {
            instant: du("instant")?,
            fast: du("fast")?,
            flyout: du("flyout")?,
            base: du("base")?,
            slow: du("slow")?,
        },
        easing: Easings {
            standard: ea("standard")?,
            enter: ea("enter")?,
            exit: ea("exit")?,
            toast: ea("toast")?,
            out_cubic: ea("out-cubic")?,
            linear: ea("linear")?,
        },
        spring: Springs {
            view: sp("view")?,
            pane_resize: sp("pane-resize")?,
            workspace_switch: sp("workspace-switch")?,
            canvas_zoom: sp("canvas-zoom")?,
            canvas_pan: sp("canvas-pan")?,
            scratchpad: sp("scratchpad")?,
            shake: sp("shake")?,
        },
    };

    let theme = |name: ThemeName| -> Result<Tokens, TokenError> {
        let path = format!("themes.{}", name.label());
        let t = raw.themes.get(name.label()).ok_or_else(|| TokenError::Missing(path.clone()))?;
        let cpath = format!("{path}.color");
        let c = |k: &str| get(&t.color, &cpath, k).and_then(|v| Rgba::parse(v));
        if t.color.len() != Colors::NAMES.len() {
            return Err(TokenError::Value(format!(
                "{cpath}: expected {} colours, found {}",
                Colors::NAMES.len(),
                t.color.len()
            )));
        }
        let rpath = format!("{path}.radius");
        let r = |k: &str| get(&t.radius, &rpath, k).and_then(|v| px(v));
        Ok(Tokens {
            name,
            colors: Colors {
                bg: c("bg")?,
                surface: c("surface")?,
                surface_2: c("surface-2")?,
                line: c("line")?,
                frame: c("frame")?,
                fg: c("fg")?,
                fg_muted: c("fg-muted")?,
                accent: c("accent")?,
                on_accent: c("on-accent")?,
                ok: c("ok")?,
                warn: c("warn")?,
                err: c("err")?,
                info: c("info")?,
                syn_keyword: c("syn-keyword")?,
                syn_string: c("syn-string")?,
                syn_number: c("syn-number")?,
                syn_comment: c("syn-comment")?,
            },
            fonts: Fonts { sans: t.font.sans.clone(), mono: t.font.mono.clone() },
            radius: Radii { panel: r("panel")?, window: r("window")?, control: r("control")? },
            shape: Shape {
                layout: t.shape.layout,
                gap: px(&t.shape.gap)?,
                rail: t.shape.rail.variant,
                rail_width: px(&t.shape.rail.width)?,
                dock: t.shape.dock.variant,
                dock_height: px(&t.shape.dock.height)?,
                scratchpad: t.shape.scratchpad,
                default_mode: t.shape.default_mode,
                label_caps: t.shape.label_caps,
            },
        })
    };

    Ok(TokenSet {
        default,
        shared,
        themes: [
            theme(ThemeName::Ash)?,
            theme(ThemeName::Ink)?,
            theme(ThemeName::Midnight)?,
            theme(ThemeName::Moss)?,
            theme(ThemeName::Dusk)?,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_values() {
        assert_eq!(px("12.5px").unwrap(), 12.5);
        assert_eq!(ms("80ms").unwrap(), 80);
        assert_eq!(bezier("cubic-bezier(.16,1,.3,1)").unwrap(), Bezier(0.16, 1.0, 0.3, 1.0));
        let s = shadow("0 24px 60px rgba(0,0,0,.45)").unwrap();
        assert_eq!((s.x, s.y, s.blur, s.spread), (0.0, 24.0, 60.0, 0.0));
        assert_eq!(s.color.a, 115);
        assert!(px("wide").is_err());
    }
}
