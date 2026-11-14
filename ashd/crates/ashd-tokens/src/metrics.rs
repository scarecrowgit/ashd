//! Layout constants in logical px, measured on the Ash reference board
//! (`docs/design/pages/desk-05-ash-soft-graphite-amber.html`, 1920×1080) and the details
//! pages. Theme-dependent sizes (rail width, dock height, gap) are in
//! [`crate::Shape`]; use these consts for the Ash defaults and for everything that does
//! not change between themes.
//!
//! Owner: kit. Other agents may ADD constants (list them in the final report); never
//! change an existing value without checking the design page it cites.

// ── frame surfaces (layer-shell) ──────────────────────────────────────────────

/// Left rail width (Ash `num` rail). Anchored top|bottom|left, exclusive zone = width.
pub const RAIL_W: f32 = 64.0;
/// Rail header cell (output name) height.
pub const RAIL_HEADER_H: f32 = 52.0;
/// First workspace button top offset inside the rail.
pub const RAIL_BUTTONS_TOP: f32 = 64.0;
/// Workspace button height of the `num` rail (desk-05 l.22: `width: 64px; height: 56px`;
/// the width is the rail width) and the gap between buttons. The `thumb` (74) and `pill`
/// (48) rails have their own heights (ashd-panel-rail `geom`).
pub const RAIL_BUTTON: f32 = 56.0;
pub const RAIL_BUTTON_GAP: f32 = 6.0;
/// Rail hover flyout card width (desk-05 l.79: `width: 268px`), every theme.
pub const RAIL_FLYOUT_W: f32 = 268.0;

/// Bottom bar height (Ash `bar` dock). Anchored left|right|bottom, exclusive zone = height.
pub const BAR_H: f32 = 46.0;
/// Status bar block at the bottom-left (clock, meters, network, volume, battery, layout, bell).
pub const STATUS_W: f32 = 560.0;
/// Dock centre x (desk-05: `left 822px; transform: translateX(-50%)`), from the left edge.
pub const DOCK_CENTER_X: f32 = 822.0;
/// Dock launcher button and app buttons.
pub const DOCK_BUTTON: f32 = 36.0;
/// Bottom-right group (workspace label + Scroll|Canvas segmented control) right inset.
pub const BAR_END_INSET: f32 = 14.0;

/// Gap between strip columns (desk-05 `gap 12`); also niri's `layout { gaps }`.
pub const STRIP_GAP: f32 = 12.0;
/// Height the shell's strip header reserves at the top of niri's working area (`struts { top }`),
/// every theme; floating themes add the theme gap on top.
pub const STRIP_HEADER_STRUT: f32 = 30.0;

/// Right scratchpad panel. Anchored top|bottom|right, exclusive zone = width while shown.
pub const SCRATCH_W: f32 = 340.0;

// ── overlays (drawn inside the full-output overlay surfaces) ──────────────────

/// Popups that grow upward from the status bar: gap between bar top and popup bottom.
pub const POPUP_GAP: f32 = 8.0;
pub const QUICK_SETTINGS_W: f32 = 400.0;
pub const NOTIFICATION_CENTER_W: f32 = 400.0;
/// Calendar and media popups (det-11 l.26 / l.217: `width: 340px`).
pub const CALENDAR_W: f32 = 340.0;
pub const MEDIA_W: f32 = 340.0;
/// Launcher (centred).
pub const LAUNCHER_W: f32 = 620.0;
/// Launcher top offset from the top of the usable area.
pub const LAUNCHER_TOP: f32 = 180.0;
/// Toasts stack above the status bar, left-aligned with it.
pub const TOAST_W: f32 = 380.0;
/// Compact toast of shell notices (det-12 network toasts: 340, padding 12 14).
pub const TOAST_COMPACT_W: f32 = 340.0;
pub const TOAST_GAP: f32 = 8.0;
/// OSD: bottom-centre above the dock.
pub const OSD_W: f32 = 260.0;
pub const OSD_H: f32 = 48.0;
pub const OSD_BOTTOM: f32 = BAR_H + 24.0;
/// Clipboard picker panel (det-10 l.21: `width: 860px`; the 980×620 is the board around
/// it). Height = header 50 + list/preview body 380 + footer 36.
pub const CLIPBOARD_W: f32 = 860.0;
pub const CLIPBOARD_H: f32 = 466.0;
/// Session buttons (det-13): 40×40 with a 17px icon.
pub const SESSION_BUTTON: f32 = 40.0;
pub const SESSION_ICON: f32 = 17.0;

// ── type scale (TOKENS.md §3): (size, line height) ────────────────────────────

pub mod type_scale {
    pub const DISPLAY: (f32, f32) = (28.0, 34.0);
    pub const TITLE: (f32, f32) = (15.0, 20.0);
    pub const BODY: (f32, f32) = (13.0, 20.0);
    pub const UI: (f32, f32) = (12.5, 16.0);
    pub const LABEL: (f32, f32) = (10.5, 14.0);
    pub const CODE: (f32, f32) = (12.0, 19.0);
    pub const NUMERIC: (f32, f32) = (16.0, 18.0);
}

/// Icon sizes (TOKENS.md §8): 13–14 dense rows/buttons, 16 status/menus, 18 dock, 22 empty states.
pub mod icon_size {
    pub const DENSE: f32 = 14.0;
    pub const STATUS: f32 = 16.0;
    pub const DOCK: f32 = 18.0;
    pub const EMPTY: f32 = 22.0;
}

/// Standard control heights.
pub const CONTROL_H: f32 = 30.0;
pub const INPUT_H: f32 = 34.0;
pub const ROW_H: f32 = 30.0;

// ── component sizes (docs/design/pages/sys-04-components-ash.html) ────────────

/// Text button height; the small variant; the square icon button.
pub const BUTTON_H: f32 = 32.0;
pub const BUTTON_SM_H: f32 = 26.0;
pub const ICON_BUTTON: f32 = 32.0;
/// Toggle: 34×18 incl. 1px border, 12×12 knob, 2px inset.
pub const TOGGLE_W: f32 = 34.0;
pub const TOGGLE_H: f32 = 18.0;
pub const TOGGLE_KNOB: f32 = 12.0;
/// Checkbox / radio box.
pub const CHECK: f32 = 14.0;
/// Slider: 4px track, 2×14 thumb. Meter: 40×4.
pub const SLIDER_TRACK_H: f32 = 4.0;
pub const SLIDER_THUMB_W: f32 = 2.0;
pub const SLIDER_THUMB_H: f32 = 14.0;
pub const METER_W: f32 = 40.0;
pub const METER_H: f32 = 4.0;
/// Segmented control button height (inside a 2px padded, 1px bordered box).
pub const SEGMENT_H: f32 = 28.0;
/// Tab height (2px accent underline).
pub const TAB_H: f32 = 34.0;
pub const BADGE_H: f32 = 22.0;
pub const STATUS_DOT: f32 = 6.0;
/// Menu: 240 wide, 4px padding, 30px rows.
pub const MENU_W: f32 = 240.0;
pub const MENU_PAD: f32 = 4.0;
pub const MENU_ROW_H: f32 = 30.0;
/// Rich list row (icon, mono text, trailing label).
pub const RICH_ROW_H: f32 = 40.0;
/// Progress bar heights.
pub const PROGRESS_THIN: f32 = 2.0;
pub const PROGRESS_MID: f32 = 3.0;
pub const PROGRESS_THICK: f32 = 4.0;
/// Toast card width on the components page (notification toasts use [`TOAST_W`]).
pub const TOAST_CARD_W: f32 = 360.0;
/// Keyboard focus ring: 1px outline, 2px offset.
pub const FOCUS_RING_W: f32 = 1.0;
pub const FOCUS_RING_OFFSET: f32 = 2.0;
