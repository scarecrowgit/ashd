//! Geometry of `docs/design/pages/det-07-lock-screen.html` (1920×1080 board, CSS px).
//! Boxes are CSS outer sizes: `<span>`s are content-box (44 + 2 × 1 px border = 46),
//! `<button>`s are border-box (30).
//!
//! Other output sizes: everything is scaled by [`fit`] (anchored to the same corners
//! with proportional margins, clamped on small and huge outputs).

/// Design board.
pub const BOARD_W: f64 = 1920.0;
pub const BOARD_H: f64 = 1080.0;

/// Background grid: 120 px cells of 1 px `surface` lines at opacity .8.
pub const GRID: f64 = 120.0;
pub const GRID_OPACITY: f32 = 0.8;

/// Clock block: left 120 / top 120, column gap 10.
pub const CLOCK_X: f64 = 120.0;
pub const CLOCK_Y: f64 = 120.0;
pub const CLOCK_GAP: f64 = 10.0;
/// mono 168 / 500, line-height .9, letter-spacing −0.05em.
pub const CLOCK_SIZE: f32 = 168.0;
pub const CLOCK_WEIGHT: f32 = 500.0;
pub const CLOCK_LINE: f32 = 0.9;
pub const CLOCK_TRACKING_EM: f32 = -0.05;
/// Date: sans 20, fg-muted.
pub const DATE_SIZE: f32 = 20.0;

/// Auth block: left 120 / bottom 140, width 440, column gap 14.
pub const AUTH_X: f64 = 120.0;
pub const AUTH_BOTTOM: f64 = 140.0;
pub const AUTH_W: f64 = 440.0;
pub const AUTH_GAP: f64 = 14.0;
/// Avatar 44 × 44 + 1 px accent border; initial mono 500 16.
pub const AVATAR: f64 = 46.0;
pub const AVATAR_FONT: f32 = 16.0;
/// Gap avatar → name column; name 16 / 600, caps line 10.5 mono, gap 2.
pub const NAME_GAP: f64 = 14.0;
pub const NAME_SIZE: f32 = 16.0;
pub const NAME_WEIGHT: f32 = 600.0;
pub const NAME_COL_GAP: f64 = 2.0;
/// Label role (TOKENS.md §3): mono 10.5 caps +0.08em.
pub const LABEL_SIZE: f32 = 10.5;
pub const LABEL_TRACKING_EM: f32 = 0.08;
/// Password field: 48 + 2 border, padding 0 16, item gap 12, icon 15.
pub const FIELD_H: f64 = 50.0;
pub const FIELD_PAD: f64 = 16.0;
pub const FIELD_GAP: f64 = 12.0;
pub const FIELD_ICON: f64 = 15.0;
/// Dots: 8 px pitch, radius 3 (radial-gradient 3px → transparent 3.5px), row 8 high.
pub const DOT_PITCH: f64 = 8.0;
pub const DOT_R: f64 = 3.25;
/// Caret 1 × 18 accent.
pub const CARET_H: f64 = 18.0;
/// Spinner while checking (in place of the ↵ hint).
pub const SPINNER: f64 = 14.0;
/// Badges: 22 + 2 border, padding 0 8, mono 500 10.5; row gap 14.
pub const BADGE_H: f64 = 24.0;
pub const BADGE_PAD: f64 = 8.0;
pub const BADGE_GAP: f64 = 14.0;
/// Error text: sans 12.5, err.
pub const ERROR_SIZE: f32 = 12.5;
/// Hint line (det-07: the fingerprint hint; here PAM info text): sans 12, fg-muted.
pub const HINT_SIZE: f32 = 12.0;
/// Wrong-password shake: first excursion.
pub const SHAKE_PX: f64 = 10.0;

/// Right column (media card + notification summary): right 120 / bottom 140, width 380.
/// Not drawn in this milestone (no data source) — the rect is the layout hook.
pub const SIDE_RIGHT: f64 = 120.0;
pub const SIDE_BOTTOM: f64 = 140.0;
pub const SIDE_W: f64 = 380.0;

/// Footer: left/right 120, bottom 48, padding-top 20, 1 px top rule, item gap 24.
pub const FOOTER_X: f64 = 120.0;
pub const FOOTER_BOTTOM: f64 = 48.0;
pub const FOOTER_PAD_TOP: f64 = 20.0;
pub const FOOTER_GAP: f64 = 24.0;
/// Status items: icon 14, gap 6, label role in fg.
pub const STATUS_ICON: f64 = 14.0;
pub const STATUS_ICON_GAP: f64 = 6.0;
/// Buttons: 30 high (border-box), padding 0 10, gap 8, icon 13, sans 400 12, fg-muted.
pub const BUTTON_H: f64 = 30.0;
pub const BUTTON_PAD: f64 = 10.0;
pub const BUTTON_ICON: f64 = 13.0;
pub const BUTTON_ICON_GAP: f64 = 8.0;
pub const BUTTON_FONT: f32 = 12.0;

/// Design-px → logical-px factor for an output of `size` logical px: proportional to
/// the 1920×1080 board, clamped to [0.75, 3] (below 0.75 the 10.5 px labels would drop
/// under 8 px; the layout still fits down to ~800×600 at 0.75).
pub fn fit(size: (f64, f64)) -> f64 {
    (size.0 / BOARD_W).min(size.1 / BOARD_H).clamp(MIN_FIT, 3.0)
}

pub const MIN_FIT: f64 = 0.75;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_is_proportional_and_clamped() {
        assert_eq!(fit((1920.0, 1080.0)), 1.0);
        assert_eq!(fit((2560.0, 1440.0)), 2560.0 / 1920.0);
        assert_eq!(fit((1600.0, 900.0)), 900.0 / 1080.0);
        assert_eq!(fit((1366.0, 768.0)), MIN_FIT);
        assert_eq!(fit((640.0, 360.0)), MIN_FIT);
        assert_eq!(fit((3440.0, 1440.0)), 1440.0 / 1080.0, "ultrawide: height bound");
    }
}
