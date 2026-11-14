//! The `Theme` GPUI global, built from `ashd_tokens::Tokens`. Never hard-code a colour,
//! font or radius in a view: `let t = theme(cx).clone();` then `t.accent`,
//! `t.font_mono.clone()`, `t.radius_control` … (same field names as fm's `theme.rs`).
//!
//! Theme switches go through [`set_theme`], which crossfades every surface over
//! `motion::timing::THEME_CROSSFADE` (300 ms) by interpolating the colour tokens; fonts,
//! radii and shape switch at the midpoint.

use std::time::{Duration, Instant};

use gpui::{App, BoxShadow, Global, Hsla, Pixels, Rgba, SharedString, point, px};
use ashd_tokens::{
    DockVariant, LayoutVariant, RailVariant, ScratchpadVariant, Shadow, ThemeName, Tokens, WindowMode,
    motion,
};

pub use ashd_tokens::metrics;

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: ThemeName,
    // ── 17 colour tokens ──
    pub bg: Hsla,
    pub surface: Hsla,
    pub surface_2: Hsla,
    pub line: Hsla,
    pub frame: Hsla,
    pub fg: Hsla,
    pub fg_muted: Hsla,
    pub accent: Hsla,
    pub on_accent: Hsla,
    pub ok: Hsla,
    pub warn: Hsla,
    pub err: Hsla,
    pub info: Hsla,
    pub syn_keyword: Hsla,
    pub syn_string: Hsla,
    pub syn_number: Hsla,
    pub syn_comment: Hsla,
    // ── fonts / radii ──
    pub font_sans: SharedString,
    pub font_mono: SharedString,
    pub radius_panel: Pixels,
    pub radius_window: Pixels,
    pub radius_control: Pixels,
    // ── shape (TOKENS.md §6) ──
    pub layout: LayoutVariant,
    /// Gap between panels/windows; inset of floating panels from the screen edge.
    pub gap: Pixels,
    pub rail: RailVariant,
    pub rail_w: Pixels,
    pub dock: DockVariant,
    pub bar_h: Pixels,
    pub scratchpad: ScratchpadVariant,
    pub default_mode: WindowMode,
    /// Label role in caps (Ash, Ink, Moss) or as written (Midnight, Dusk).
    pub label_caps: bool,
}

impl Global for Theme {}

/// Shorthand for `cx.global::<Theme>()`. Clone it (`theme(cx).clone()`) when you also
/// need `cx` mutably afterwards.
pub fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

/// `ashd_tokens::Rgba` → GPUI colour.
pub fn color(c: ashd_tokens::Rgba) -> Hsla {
    gpui::rgba(c.to_rgba_u32()).into()
}

fn shadow(s: Shadow) -> BoxShadow {
    BoxShadow {
        color: color(s.color),
        offset: point(px(s.x), px(s.y)),
        blur_radius: px(s.blur),
        spread_radius: px(s.spread),
        inset: false,
    }
}

impl Theme {
    pub fn from_tokens(t: &Tokens) -> Self {
        let c = &t.colors;
        Theme {
            name: t.name,
            bg: color(c.bg),
            surface: color(c.surface),
            surface_2: color(c.surface_2),
            line: color(c.line),
            frame: color(c.frame),
            fg: color(c.fg),
            fg_muted: color(c.fg_muted),
            accent: color(c.accent),
            on_accent: color(c.on_accent),
            ok: color(c.ok),
            warn: color(c.warn),
            err: color(c.err),
            info: color(c.info),
            syn_keyword: color(c.syn_keyword),
            syn_string: color(c.syn_string),
            syn_number: color(c.syn_number),
            syn_comment: color(c.syn_comment),
            font_sans: t.fonts.sans.clone().into(),
            font_mono: t.fonts.mono.clone().into(),
            radius_panel: px(t.radius.panel),
            radius_window: px(t.radius.window),
            radius_control: px(t.radius.control),
            layout: t.shape.layout,
            gap: px(t.shape.gap),
            rail: t.shape.rail,
            rail_w: px(t.shape.rail_width),
            dock: t.shape.dock,
            bar_h: px(t.shape.dock_height),
            scratchpad: t.shape.scratchpad,
            default_mode: t.shape.default_mode,
            label_caps: t.shape.label_caps,
        }
    }

    /// `text` as the label role writes it in this theme: caps, or lower case (desk-07/09
    /// labels are all lower case: "fri 13 nov", "scroll · 2 cols").
    pub fn label_text(&self, text: &str) -> SharedString {
        if self.label_caps { text.to_uppercase().into() } else { text.to_lowercase().into() }
    }

    /// Built-in tokens, fonts as named in tokens.json.
    pub fn get(name: ThemeName) -> Self {
        Self::from_tokens(ashd_tokens::tokens(name))
    }

    /// The theme with its font families swapped for installed ones (e.g. `JetBrains Mono`
    /// → `JetBrainsMono Nerd Font`, Manrope → Inter). Use this, not `get`, at runtime.
    pub fn resolved(name: ThemeName, cx: &App) -> Self {
        let mut theme = Self::get(name);
        let installed = cx.text_system().all_font_names();
        let pick = |wanted: &SharedString, fallbacks: &[&str]| -> SharedString {
            let has = |n: &str| installed.iter().any(|f| f.eq_ignore_ascii_case(n));
            let squashed = wanted.replace(' ', "");
            std::iter::once(wanted.to_string())
                .chain([format!("{squashed} Nerd Font"), format!("{squashed} NF"), squashed.clone()])
                .chain(fallbacks.iter().map(|s| s.to_string()))
                .find(|n| has(n))
                .map(SharedString::from)
                .unwrap_or_else(|| wanted.clone())
        };
        theme.font_mono = pick(
            &theme.font_mono,
            &["JetBrains Mono", "JetBrainsMono Nerd Font", "Geist Mono", "IBM Plex Mono", "DejaVu Sans Mono", "Noto Sans Mono"],
        );
        theme.font_sans = pick(
            &theme.font_sans,
            &["Inter", "Inter Variable", "Geist", "IBM Plex Sans", "Noto Sans", "DejaVu Sans", "Cantarell"],
        );
        theme
    }

    /// Menus, launcher, popups (`0 24px 60px rgba(0,0,0,.45)`).
    pub fn shadow_overlay(&self) -> Vec<BoxShadow> {
        vec![shadow(ashd_tokens::shared().shadow.overlay)]
    }

    /// The one large modal (`0 30px 80px rgba(0,0,0,.55)`).
    pub fn shadow_quicklook(&self) -> Vec<BoxShadow> {
        vec![shadow(ashd_tokens::shared().shadow.quicklook)]
    }

    /// Flyouts, toasts, submenus (`0 18px 40px rgba(0,0,0,.35)`).
    pub fn shadow_flyout(&self) -> Vec<BoxShadow> {
        vec![shadow(ashd_tokens::shared().shadow.flyout)]
    }

    /// Modal scrim (55 %).
    pub fn scrim(&self) -> Hsla {
        color(ashd_tokens::shared().scrim.modal)
    }

    /// Power / session / polkit dim (60 %).
    pub fn scrim_power(&self) -> Hsla {
        color(ashd_tokens::shared().scrim.power)
    }

    /// Primary-button hover fill as drawn on the components page: the accent lifted
    /// ~7 % in lightness (less for already light accents), then `filter: brightness(1.06)`.
    pub fn accent_hover(&self) -> Hsla {
        let lift = 0.07 * ((1.0 - self.accent.l) / 0.4).clamp(0.3, 1.0);
        let lifted = Hsla { l: (self.accent.l + lift).min(1.0), ..self.accent };
        shade(lifted, 1.06)
    }

    /// Primary-button pressed fill: `filter: brightness(.92)`.
    pub fn accent_pressed(&self) -> Hsla {
        shade(self.accent, 0.92)
    }

    /// Keyboard focus ring: 1px outline in `fg`, 2px offset, drawn as two spread shadows
    /// (`gap` fills the offset with the surface behind the control).
    pub fn focus_ring(&self, gap: Hsla) -> Vec<BoxShadow> {
        let ring = |spread: f32, color: Hsla| BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(spread),
            inset: false,
        };
        let off = metrics::FOCUS_RING_OFFSET;
        vec![ring(off + metrics::FOCUS_RING_W, self.fg), ring(off, gap)]
    }

    /// Colour interpolation between two themes (`t` in 0..=1). Non-colour fields come
    /// from `self` before the midpoint and from `to` after it.
    pub fn lerp(&self, to: &Theme, t: f32) -> Theme {
        let mix = |a: Hsla, b: Hsla| -> Hsla {
            let (a, b): (Rgba, Rgba) = (a.into(), b.into());
            Rgba {
                r: a.r + (b.r - a.r) * t,
                g: a.g + (b.g - a.g) * t,
                b: a.b + (b.b - a.b) * t,
                a: a.a + (b.a - a.a) * t,
            }
            .into()
        };
        let base = if t < 0.5 { self.clone() } else { to.clone() };
        Theme {
            bg: mix(self.bg, to.bg),
            surface: mix(self.surface, to.surface),
            surface_2: mix(self.surface_2, to.surface_2),
            line: mix(self.line, to.line),
            frame: mix(self.frame, to.frame),
            fg: mix(self.fg, to.fg),
            fg_muted: mix(self.fg_muted, to.fg_muted),
            accent: mix(self.accent, to.accent),
            on_accent: mix(self.on_accent, to.on_accent),
            ok: mix(self.ok, to.ok),
            warn: mix(self.warn, to.warn),
            err: mix(self.err, to.err),
            info: mix(self.info, to.info),
            syn_keyword: mix(self.syn_keyword, to.syn_keyword),
            syn_string: mix(self.syn_string, to.syn_string),
            syn_number: mix(self.syn_number, to.syn_number),
            syn_comment: mix(self.syn_comment, to.syn_comment),
            ..base
        }
    }
}

/// CSS `filter: brightness(f)` on an sRGB colour (channel multiply, clamped).
pub fn shade(c: Hsla, f: f32) -> Hsla {
    let c: Rgba = c.into();
    Rgba { r: (c.r * f).min(1.0), g: (c.g * f).min(1.0), b: (c.b * f).min(1.0), a: c.a }.into()
}

/// Install the initial theme (resolved fonts). Call once at startup, before opening
/// windows.
pub fn init(name: ThemeName, cx: &mut App) {
    let theme = Theme::resolved(name, cx);
    cx.set_global(theme);
}

/// The theme a running crossfade is heading to (equals `theme(cx).name` when idle).
#[derive(Clone)]
struct ThemeTarget {
    to: Theme,
    generation: u64,
}

impl Global for ThemeTarget {}

/// The current target theme name (what `ashd theme get` reports).
pub fn current_theme_name(cx: &App) -> ThemeName {
    cx.try_global::<ThemeTarget>().map(|t| t.to.name).unwrap_or_else(|| theme(cx).name)
}

/// Switch every surface to `name`. With `animate`, crossfades over 300 ms (colour
/// interpolation, refreshing all windows each frame); without, switches at once.
pub fn set_theme(name: ThemeName, animate: bool, cx: &mut App) {
    let to = Theme::resolved(name, cx);
    let from = theme(cx).clone();
    let generation = cx.try_global::<ThemeTarget>().map(|t| t.generation + 1).unwrap_or(0);
    cx.set_global(ThemeTarget { to: to.clone(), generation });
    if !animate || from == to {
        cx.set_global(to);
        cx.refresh_windows();
        return;
    }
    let total = Duration::from_millis(crate::anim::duration_ms(motion::timing::THEME_CROSSFADE));
    let start = Instant::now();
    cx.spawn(async move |cx| {
        loop {
            // ~180 Hz; GPUI coalesces refreshes to the display's frame rate anyway.
            cx.background_executor().timer(Duration::from_millis(5)).await;
            let done = cx.update(|cx| {
                if cx.try_global::<ThemeTarget>().map(|t| t.generation) != Some(generation) {
                    return true; // superseded by a newer switch
                }
                let t = (start.elapsed().as_secs_f32() / total.as_secs_f32()).min(1.0);
                let eased = motion::EASE_STANDARD.eval(t);
                cx.set_global(if t >= 1.0 { to.clone() } else { from.lerp(&to, eased) });
                cx.refresh_windows();
                t >= 1.0
            });
            if done {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ash_accent() {
        let t = Theme::get(ThemeName::Ash);
        let a: Rgba = t.accent.into();
        assert!((a.r - 0xE0 as f32 / 255.0).abs() < 1e-3);
        assert_eq!(t.rail_w, px(metrics::RAIL_W));
        let mid = t.lerp(&Theme::get(ThemeName::Ink), 0.5);
        assert_eq!(mid.name, ThemeName::Ink);
    }

    #[test]
    fn crossfade_lerp_endpoints_and_midpoint_swap() {
        let (a, b) = (Theme::get(ThemeName::Ash), Theme::get(ThemeName::Dusk));
        assert_eq!(a.lerp(&b, 0.0).bg, a.bg);
        assert_eq!(a.lerp(&b, 1.0).accent, b.accent);
        let before = a.lerp(&b, 0.49);
        let after = a.lerp(&b, 0.51);
        // fonts / radii / shape swap at the midpoint, colours are continuous
        assert_eq!((before.font_sans.clone(), before.radius_panel, before.rail_w), (a.font_sans.clone(), a.radius_panel, a.rail_w));
        assert_eq!((after.font_sans.clone(), after.radius_panel, after.rail_w), (b.font_sans.clone(), b.radius_panel, b.rail_w));
        let (x, y): (Rgba, Rgba) = (before.accent.into(), after.accent.into());
        assert!((x.r - y.r).abs() < 0.03);
        let q: Rgba = a.lerp(&b, 0.5).bg.into();
        let (p0, p1): (Rgba, Rgba) = (a.bg.into(), b.bg.into());
        assert!((q.r - (p0.r + p1.r) / 2.0).abs() < 1e-4);
    }

    #[test]
    fn primary_hover_and_pressed() {
        let t = Theme::get(ThemeName::Ash);
        let hover: Rgba = t.accent_hover().into();
        let acc: Rgba = t.accent.into();
        let pressed: Rgba = t.accent_pressed().into();
        assert!(hover.r > acc.r && pressed.r < acc.r);
        assert_eq!(t.focus_ring(t.surface).len(), 2);
    }
}
