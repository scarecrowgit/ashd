use std::fmt;

use crate::TokenError;

/// An sRGB colour with 8-bit channels and 8-bit alpha. Parsed from `#RRGGBB`,
/// `#RRGGBBAA` or `rgba(r,g,b,a)` (a in 0..1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// From `0xRRGGBB` (opaque).
    pub const fn rgb(hex: u32) -> Self {
        Self { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 0xFF }
    }

    /// From `0xRRGGBBAA`.
    pub const fn from_rgba_hex(hex: u32) -> Self {
        Self { r: (hex >> 24) as u8, g: (hex >> 16) as u8, b: (hex >> 8) as u8, a: hex as u8 }
    }

    /// `0xRRGGBB` (alpha dropped). This is what `gpui::rgb()` takes.
    pub const fn to_rgb_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// `0xRRGGBBAA`. This is what `gpui::rgba()` takes.
    pub const fn to_rgba_u32(self) -> u32 {
        ((self.r as u32) << 24) | ((self.g as u32) << 16) | ((self.b as u32) << 8) | self.a as u32
    }

    /// Alpha in 0..=1.
    pub fn alpha(self) -> f32 {
        self.a as f32 / 255.0
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a: (a.clamp(0.0, 1.0) * 255.0).round() as u8, ..self }
    }

    /// `#RRGGBB` (uppercase, alpha dropped) — the form used in tokens.json and by every
    /// renderer.
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// `#RRGGBBAA`.
    pub fn hex_alpha(self) -> String {
        format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
    }

    pub fn parse(s: &str) -> Result<Self, TokenError> {
        let bad = || TokenError::Value(format!("bad colour `{s}`"));
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            let v = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
            return match hex.len() {
                6 => Ok(Self::rgb(v)),
                8 => Ok(Self::from_rgba_hex(v)),
                _ => Err(bad()),
            };
        }
        if let Some(inner) = s.strip_prefix("rgba(").and_then(|r| r.strip_suffix(')')) {
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() != 4 {
                return Err(bad());
            }
            let ch = |p: &str| p.parse::<u8>().map_err(|_| bad());
            let a = parts[3].parse::<f32>().map_err(|_| bad())?;
            return Ok(Self { r: ch(parts[0])?, g: ch(parts[1])?, b: ch(parts[2])?, a: 0 }.with_alpha(a));
        }
        Err(bad())
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f32 {
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// WCAG contrast ratio against `other` (>= 1).
    pub fn contrast(self, other: Rgba) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 0xFF { f.write_str(&self.hex()) } else { f.write_str(&self.hex_alpha()) }
    }
}
