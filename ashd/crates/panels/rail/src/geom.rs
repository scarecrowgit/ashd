//! Rail geometry measured on the desktop boards (line numbers of
//! `docs/design/pages/desk-0N-*.html`): desk-05 Ash / desk-08 Moss (`num`), desk-06 Ink /
//! desk-09 Dusk (`thumb`), desk-07 Midnight (`pill`). The rail width comes from the theme
//! (`t.rail_w`); the button height from the theme's [`RailVariant`].

use ashd_tokens::{LayoutVariant, RailVariant, metrics};
use ashd_ui::Theme;

/// Gap between workspace buttons (desk-05 l.21 `gap: 6px`, every variant).
pub const BUTTON_GAP: f32 = metrics::RAIL_BUTTON_GAP;
/// First button top (l.21 `top: 64px`, every variant).
pub const BUTTONS_TOP: f32 = metrics::RAIL_BUTTONS_TOP;
/// Header: mono 12/500 (l.20); its bottom border is inset 8 px on floating rails
/// (desk-06..09 l.23 `margin: 0 8px`), full width on Ash.
pub const HEADER_SIZE: f32 = 12.0;
pub const HEADER_INSET_FLOATING: f32 = 8.0;

// ── num (desk-05 l.22–32, desk-08 l.24–34): 64 × 56 ──
/// Gap number ↔ dots inside a button (l.22 `gap: 6px`).
pub const BUTTON_INNER_GAP: f32 = 6.0;
/// Workspace number: mono 15 (l.23).
pub const NUMBER_SIZE: f32 = 15.0;
/// Window-count dots: 3×3, gap 3 (l.24–25); at most this many are drawn.
pub const DOT: f32 = 3.0;
pub const DOT_GAP: f32 = 3.0;
pub const MAX_DOTS: usize = 8;
/// Active marker: 2px bar at the left edge, inset 10 top/bottom (l.32).
pub const ACTIVE_BAR_W: f32 = 2.0;
pub const ACTIVE_BAR_INSET: f32 = 10.0;

// ── thumb (desk-06 l.25–47, desk-09 l.24–46): 92 × 74 ──
pub const THUMB_BUTTON_H: f32 = 74.0;
/// Mini column preview 72×40, padding 3, column gap 2, `bg` fill, 1px border (accent on
/// the active workspace); columns `surface-2` with a 1px border (accent: focused column).
pub const THUMB_W: f32 = 72.0;
pub const THUMB_H: f32 = 40.0;
pub const THUMB_PAD: f32 = 3.0;
pub const THUMB_COL_GAP: f32 = 2.0;
/// Preview ↔ "02 code" label (mono 10.5, as written, fg on the active workspace).
pub const THUMB_LABEL_GAP: f32 = 6.0;

// ── pill (desk-07 l.24–44): 60 × 48 ──
pub const PILL_BUTTON_H: f32 = 48.0;
/// 10px dot (fg-muted); the active workspace's becomes a 10×28 accent pill.
pub const PILL_DOT: f32 = 10.0;
pub const PILL_ACTIVE_H: f32 = 28.0;
/// Dot ↔ number (mono 10).
pub const PILL_LABEL_GAP: f32 = 5.0;
pub const PILL_LABEL_SIZE: f32 = 10.0;

/// "New workspace" button: 36×36 dashed, 14 from the bottom, 14px plus (every variant).
pub const NEW_BUTTON: f32 = 36.0;
pub const NEW_BUTTON_BOTTOM: f32 = 14.0;
pub const NEW_ICON: f32 = 14.0;

/// Flyout (l.79–80): wrapper at `left: rail_w; padding-left: 16px` (floating: gap + 6 —
/// Ink/Midnight 16, Moss 14, Dusk 18), card 268 wide, padding 12, gap 10, top = button
/// top − 4.
pub const FLYOUT_OFFSET_ATTACHED: f32 = 16.0;
pub const FLYOUT_OFFSET_FLOATING_EXTRA: f32 = 6.0;
pub const FLYOUT_OFFSET_Y: f32 = -4.0;
pub const FLYOUT_W: f32 = metrics::RAIL_FLYOUT_W;
pub const FLYOUT_PAD: f32 = 12.0;
pub const FLYOUT_GAP: f32 = 10.0;
/// Flyout meta label ("scroll · 6 cols") and row index: mono 10 (l.83, l.100).
pub const FLYOUT_META_SIZE: f32 = 10.0;
/// Column-proportion strip: 26 high segments, gap 3 (l.85–86).
pub const STRIP_H: f32 = 26.0;
pub const STRIP_GAP: f32 = 3.0;
/// Window rows: 30 high, padding 0 10, gap 10, 14px glyph, list gap 1 (l.91–93).
pub const ROW_GAP: f32 = 1.0;
pub const ROW_ICON: f32 = metrics::icon_size::DENSE;
/// Action buttons: 28 high, gap 6, sans 11.5, separated by a top border + 10 padding (l.111–112).
pub const ACTION_H: f32 = 28.0;
pub const ACTION_GAP: f32 = 6.0;
pub const ACTION_TEXT: f32 = 11.5;
/// Pixels of smooth (touchpad) scrolling that count as one workspace step. GPUI's
/// Wayland backend scales axis values ×3, so this is ~80 raw axis units: one deliberate
/// swipe = one workspace. Mouse wheels arrive as `Lines` (one step per notch).
pub const SCROLL_STEP_PX: f32 = 240.0;
/// Flyout rows drawn before the list is cut (keeps the card on screen).
pub const MAX_ROWS: usize = 12;

/// Workspace button height of a rail variant.
pub fn button_h(variant: RailVariant) -> f32 {
    match variant {
        RailVariant::Num => metrics::RAIL_BUTTON,
        RailVariant::Thumb => THUMB_BUTTON_H,
        RailVariant::Pill => PILL_BUTTON_H,
    }
}

/// Rail-local y of workspace button `index`.
pub fn button_top(variant: RailVariant, index: usize) -> f32 {
    BUTTONS_TOP + index as f32 * (button_h(variant) + BUTTON_GAP)
}

/// Horizontal distance rail edge → flyout card (the hover bridge).
pub fn flyout_offset_x(t: &Theme) -> f32 {
    match t.layout {
        LayoutVariant::Attached => FLYOUT_OFFSET_ATTACHED,
        LayoutVariant::Floating => f32::from(t.gap) + FLYOUT_OFFSET_FLOATING_EXTRA,
    }
}

/// Header border inset (0 on attached rails).
pub fn header_inset(t: &Theme) -> f32 {
    if t.layout == LayoutVariant::Floating { HEADER_INSET_FLOATING } else { 0.0 }
}

/// Height of a flyout card with `rows` window rows (for keeping it on screen).
pub fn flyout_height(rows: usize) -> f32 {
    let list = if rows == 0 { metrics::ROW_H } else { rows as f32 * metrics::ROW_H + (rows - 1) as f32 * ROW_GAP };
    2.0 * FLYOUT_PAD
        + metrics::type_scale::TITLE.1
        + FLYOUT_GAP
        + STRIP_H
        + FLYOUT_GAP
        + list
        + FLYOUT_GAP
        + 1.0
        + FLYOUT_GAP
        + ACTION_H
        + 2.0
}

#[cfg(test)]
mod tests {
    use ashd_tokens::ThemeName;

    use super::*;

    #[test]
    fn flyout_tops_match_the_boards() {
        let tops = |v| (0..5).map(|i| button_top(v, i) + FLYOUT_OFFSET_Y).collect::<Vec<f32>>();
        // desk-05 / desk-08 (num): 60, 122, 184, 246, 308.
        assert_eq!(tops(RailVariant::Num), vec![60.0, 122.0, 184.0, 246.0, 308.0]);
        // desk-06 / desk-09 (thumb): o0 60, o1 140.
        assert_eq!(tops(RailVariant::Thumb)[..2], [60.0, 140.0]);
        // desk-07 (pill): o1 114.
        assert_eq!(tops(RailVariant::Pill)[1], 114.0);
    }

    #[test]
    fn flyout_offsets_match_the_boards() {
        let off = |n| flyout_offset_x(&Theme::get(n));
        assert_eq!(
            [ThemeName::Ash, ThemeName::Ink, ThemeName::Midnight, ThemeName::Moss, ThemeName::Dusk].map(off),
            [16.0, 16.0, 16.0, 14.0, 18.0]
        );
        assert_eq!(FLYOUT_W, 268.0);
        assert_eq!(button_h(Theme::get(ThemeName::Ash).rail), 56.0);
    }
}
