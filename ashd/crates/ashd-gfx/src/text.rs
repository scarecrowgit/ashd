//! Text shaping with parley; fonts come from the system through fontique (fontconfig on
//! Linux). Every theme names a sans/mono pair in tokens; [`font_stack`] adds fallbacks so
//! a missing family degrades to Inter / JetBrains Mono / the generic family instead of
//! tofu.

use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFeatures, FontWeight, Layout, LayoutContext, LineHeight,
    StyleProperty,
};
use ashd_tokens::Rgba;

/// Text shaping state (parley), reused across frames.
#[derive(Default)]
pub struct Text {
    pub fonts: FontContext,
    pub layouts: LayoutContext<Rgba>,
}

/// Generic family appended to a font stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Generic {
    Sans,
    Mono,
}

/// CSS `font-family` list for a token font name with fallbacks. Nerd-font packagings
/// of JetBrains Mono are common on NixOS (see GPUI-NOTES §4), so they are tried too.
pub fn font_stack(primary: &str, generic: Generic) -> String {
    let mut names: Vec<String> = vec![primary.to_string()];
    let compact = primary.replace(' ', "");
    names.push(format!("{compact} Nerd Font"));
    match generic {
        Generic::Sans => names.extend(["Inter".into(), "Inter Variable".into(), "DejaVu Sans".into()]),
        Generic::Mono => names.extend([
            "JetBrains Mono".into(),
            "JetBrainsMono Nerd Font".into(),
            "DejaVu Sans Mono".into(),
        ]),
    }
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.to_lowercase()));
    let mut css: Vec<String> = names.iter().map(|n| format!("\"{}\"", n.replace('"', ""))).collect();
    css.push(match generic {
        Generic::Sans => "sans-serif".into(),
        Generic::Mono => "monospace".into(),
    });
    css.join(", ")
}

/// One run of uniformly styled text. Sizes are logical px.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// CSS `font-family` list (see [`font_stack`]).
    pub family: String,
    pub size: f32,
    pub weight: f32,
    /// Multiple of the font size (CSS unitless `line-height`); `None` = the font's own
    /// metrics (CSS `normal`).
    pub line_height: Option<f32>,
    /// Extra advance after every character, logical px (CSS `letter-spacing`).
    pub letter_spacing: f32,
    pub color: Rgba,
    /// `font-variant-numeric: tabular-nums`.
    pub tabular: bool,
}

impl TextStyle {
    pub fn new(family: impl Into<String>, size: f32, color: Rgba) -> Self {
        Self { family: family.into(), size, weight: 400.0, line_height: None, letter_spacing: 0.0, color, tabular: false }
    }
    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }
    pub fn line_height(mut self, factor: f32) -> Self {
        self.line_height = Some(factor);
        self
    }
    /// Letter spacing in em (CSS `letter-spacing: -0.05em`).
    pub fn tracking_em(mut self, em: f32) -> Self {
        self.letter_spacing = em * self.size;
        self
    }
    pub fn tabular(mut self) -> Self {
        self.tabular = true;
        self
    }
    pub fn color(mut self, color: Rgba) -> Self {
        self.color = color;
        self
    }
}

/// A shaped, line-broken text, laid out at a physical scale.
pub struct TextLayout {
    pub(crate) layout: Layout<Rgba>,
    pub(crate) scale: f32,
}

impl TextLayout {
    /// Advance width, logical px.
    pub fn width(&self) -> f64 {
        f64::from(self.layout.full_width() / self.scale)
    }
    /// Height of the line boxes, logical px.
    pub fn height(&self) -> f64 {
        f64::from(self.layout.height() / self.scale)
    }
    /// First baseline from the top, logical px.
    pub fn baseline(&self) -> f64 {
        f64::from(self.layout.first_baseline().unwrap_or(0.0) / self.scale)
    }
    pub fn is_empty(&self) -> bool {
        self.layout.is_empty()
    }
}

impl Text {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shape `s` with `style` at `scale` (logical → physical). Single line unless `max_width`.
    pub fn layout(&mut self, s: &str, style: &TextStyle, scale: f64) -> TextLayout {
        self.layout_wrapped(s, style, scale, None)
    }

    pub fn layout_wrapped(&mut self, s: &str, style: &TextStyle, scale: f64, max_width: Option<f64>) -> TextLayout {
        let scale = scale as f32;
        let mut b = self.layouts.ranged_builder(&mut self.fonts, s, scale, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(style.family.as_str().into())));
        b.push_default(StyleProperty::FontSize(style.size));
        b.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
        if let Some(lh) = style.line_height {
            b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(lh)));
        }
        if style.letter_spacing != 0.0 {
            b.push_default(StyleProperty::LetterSpacing(style.letter_spacing));
        }
        if style.tabular {
            b.push_default(StyleProperty::FontFeatures(FontFeatures::Source("\"tnum\" 1".into())));
        }
        b.push_default(StyleProperty::Brush(style.color));
        let mut layout = b.build(s);
        layout.break_all_lines(max_width.map(|w| w as f32 * scale));
        layout.set_alignment(Alignment::Start, AlignmentOptions::default());
        TextLayout { layout, scale }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_have_fallbacks() {
        let s = font_stack("JetBrains Mono", Generic::Mono);
        assert!(s.starts_with("\"JetBrains Mono\", \"JetBrainsMono Nerd Font\""));
        assert!(s.ends_with("monospace"));
        assert_eq!(s.matches("\"JetBrains Mono\"").count(), 1);
        let s = font_stack("Manrope", Generic::Sans);
        assert!(s.contains("\"Inter\"") && s.ends_with("sans-serif"));
    }

    #[test]
    fn layout_scales_back_to_logical() {
        let mut t = Text::new();
        let style = TextStyle::new(font_stack("Inter", Generic::Sans), 20.0, Rgba::default());
        let a = t.layout("Friday, 13 November", &style, 1.0);
        let b = t.layout("Friday, 13 November", &style, 2.0);
        // Hinting/quantisation may differ by a pixel; logical sizes must agree.
        assert!((a.width() - b.width()).abs() < 2.0, "{} vs {}", a.width(), b.width());
        assert!((a.height() - b.height()).abs() < 2.0);
    }
}
