//! Bar geometry measured on desk-05 (bottom bar, l.1589–1772), det-12 (network button
//! states) and det-13 (session button). Theme-dependent sizes come from the theme.

/// Status block: `padding: 0 14px; gap: 12px` (l.1591).
pub const PAD_X: f32 = 14.0;
pub const GAP: f32 = 12.0;
/// Clock: mono 16/500, line-height 1.1; date: mono 9.5 caps (l.1593–1594).
pub const CLOCK_SIZE: f32 = 16.0;
pub const DATE_SIZE: f32 = 9.5;
pub const CLOCK_LINE: f32 = 1.1;
/// Separators: 1×22 `line` (l.1596).
pub const SEP_H: f32 = 22.0;
/// Meter groups: gap 6, caption mono 10, meter 34×4, value mono 10.5 (l.1598–1604).
pub const METER_GAP: f32 = 6.0;
pub const CAPTION_SIZE: f32 = 10.0;
pub const VALUE_SIZE: f32 = 10.5;
pub const METER_W: f32 = 34.0;
/// Status glyphs are 14 (l.1617); icon ↔ value gap 5 (volume) / 6 (network, det-12).
pub const STATUS_ICON: f32 = 14.0;
pub const ICON_GAP: f32 = 5.0;
pub const NET_GAP: f32 = 6.0;
/// The network/volume control: 30 high, padding 0 8 (det-12 l.79).
pub const CONTROL_H: f32 = 30.0;
pub const CONTROL_PAD_X: f32 = 8.0;
/// Open-popup marker: 2px accent underline (det-12 `inset 0 -2px 0 accent`).
pub const UNDERLINE_H: f32 = 2.0;
/// "no internet" warn dot: 6×6 at right −3, top −2 of the glyph (det-12).
pub const NET_DOT: f32 = 6.0;
/// Keyboard layout chip: 22 high, padding 0 7, mono 10.5/500, accent border (l.1646).
pub const CHIP_H: f32 = 22.0;
pub const CHIP_PAD_X: f32 = 7.0;
/// Bell: 26×26, 15px glyph, unread dot 6×6 at top 3 right 3 (l.1647–1654).
pub const BELL: f32 = 26.0;
pub const BELL_ICON: f32 = 15.0;
pub const BELL_DOT: f32 = 6.0;
pub const BELL_DOT_INSET: f32 = 3.0;

/// Bar end (l.1745–1770): right 14, gap 14; label group gap 10 (index mono 11, name /
/// mode mono 10.5); segmented: padding 2, gap 2, 1px line border, segments 28 high,
/// padding 0 10, gap 6, 14px glyph, sans 12/500.
pub const END_GAP: f32 = 14.0;
pub const END_LABEL_GAP: f32 = 10.0;
pub const END_INDEX_SIZE: f32 = 11.0;
pub const SEG_PAD: f32 = 2.0;
pub const SEG_H: f32 = 28.0;
pub const SEG_PAD_X: f32 = 10.0;
pub const SEG_ICON_GAP: f32 = 6.0;
pub const SEG_ICON: f32 = 14.0;
pub const SEG_TEXT: f32 = 12.0;
/// Session button: 32×32, 15px power glyph (det-13 l.284).
pub const SESSION: f32 = 32.0;
pub const SESSION_ICON: f32 = 15.0;

/// Tray (not in the mockups; sized like the bell): 24×24 hit boxes around 16px icons,
/// 2px apart, a 16×24 chevron with a 12px glyph for what does not fit, 6px warn dot at
/// top 2 right 2 for NeedsAttention.
pub const TRAY_BOX: f32 = 24.0;
pub const TRAY_ICON: f32 = 16.0;
pub const TRAY_GAP: f32 = 2.0;
pub const TRAY_CHEVRON_W: f32 = 16.0;
pub const TRAY_CHEVRON_ICON: f32 = 12.0;
pub const TRAY_DOT: f32 = 6.0;
pub const TRAY_DOT_INSET: f32 = 2.0;
/// Width assumed for the tray area until the first layout has been measured.
pub const TRAY_INITIAL_W: f32 = 4.0 * (TRAY_BOX + TRAY_GAP);
/// Tooltip: appears after this long over an icon, 6px above the bar.
pub const TRAY_TOOLTIP_DELAY_MS: u64 = 400;
pub const TRAY_TOOLTIP_GAP: f32 = 6.0;
/// One wheel notch in the `Scroll` unit apps expect (KDE: multiples of 120); a
/// touchpad needs this many pixels per notch.
pub const TRAY_SCROLL_NOTCH: i32 = 120;
pub const TRAY_SCROLL_PIXELS: f32 = 40.0;
/// A dbusmenu separator row: 1px rule with 4px margins.
pub const MENU_SEPARATOR_H: f32 = 9.0;
/// Leading slot (check / radio / icon) of a tray menu row.
pub const MENU_LEAD: f32 = 14.0;
/// Gap of the status row while the tray is shown. The Ash status block is 560 px with
/// only ~10 px to spare (a VPN label in the network control eats the rest), so the row
/// tightens from `GAP` (12) to make room for at least the chevron.
pub const TRAY_ROW_GAP: f32 = 8.0;
/// A tooltip hides itself after this long even if the pointer never "left" (GPUI keeps the
/// last position when the pointer exits a layer surface quickly).
pub const TRAY_TOOLTIP_TTL_MS: u64 = 5000;

/// Now-playing control in the tray area (not in the mockups; sized like a tray icon):
/// a 24 high box, 14px music glyph, title (mono value size) after a 6px gap, at most
/// 120 and at least 40 wide, otherwise glyph only.
pub const NOW_PLAYING_GLYPH_W: f32 = TRAY_BOX;
pub const NOW_PLAYING_ICON: f32 = STATUS_ICON;
pub const NOW_PLAYING_TITLE_GAP: f32 = 6.0;
pub const NOW_PLAYING_TITLE_MIN: f32 = 40.0;
pub const NOW_PLAYING_TITLE_MAX: f32 = 120.0;
/// The VPN name in the network control is cut at this width (a long name used to eat the
/// whole status pill).
pub const VPN_LABEL_MAX: f32 = 64.0;
