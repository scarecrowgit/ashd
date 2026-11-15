//! Frame geometry shared by shell-core (surfaces) and the frame panels (rail, bar, dock):
//! the per-theme shape of the bottom row and the fullscreen signal.
//!
//! Bottom row per `DockVariant` (desk-05..09):
//! - `Bar` (Ash): one full-width bar `bar_h` high, status block 560 with a right border,
//!   dock centred on `DOCK_CENTER_X`, workspace label + mode switch + session right.
//! - `Split` (Ink, Moss): a 560 status pill, then — between the status pill and the right
//!   edge of the main area — three pills spread with `space-between`: launcher + workspace
//!   label, dock apps, mode switch + session.
//! - `Pill` (Midnight, Dusk): the 560 status pill, then one pill (radius = 2 × panel
//!   radius) centred in the rest of the row: launcher │ apps │ mode switch │ session.
//!
//! Floating layouts inset everything by the theme gap; the main area (windows) is the
//! rectangle between rail, scratchpad and bottom row, also `gap` apart from each.

use gpui::{Div, Pixels, Styled as _, div, px};
use ashd_niri::NiriState;
use ashd_tokens::{DockVariant, LayoutVariant, metrics};
use ashd_ui::Theme;

/// Pill padding / inner gap (desk-06 l.1665 `gap: 12px; padding: 0 12px`; desk-07 l.1631
/// `padding: 0 8px`).
pub const SPLIT_PILL_PAD_X: f32 = 12.0;
pub const DOCK_PILL_PAD_X: f32 = 8.0;
pub const PILL_GAP: f32 = 12.0;
/// Separator inside the dock pill: 1×24 `line`.
pub const PILL_SEP_H: f32 = 24.0;

/// The theme's gap if the layout is floating, else 0.
pub fn inset(t: &Theme) -> Pixels {
    if t.layout == LayoutVariant::Floating { t.gap } else { px(0.) }
}

/// A floating frame box: `surface` fill, 1px `frame` border, panel radius (status pill,
/// split pills, the rail / scratchpad of floating themes).
pub fn frame_box(t: &Theme) -> Div {
    div().bg(t.surface).border_1().border_color(t.frame).rounded(t.radius_panel)
}

/// Corner radius of the `Pill` dock (desk-07 `16px` = 2 × 8, desk-09 `24px` = 2 × 12).
pub fn dock_pill_radius(t: &Theme) -> Pixels {
    t.radius_panel * 2.
}

/// Dock app button (desk-05 `34`, desk-06..09 `38`).
pub fn dock_app_size(t: &Theme) -> f32 {
    match t.dock {
        DockVariant::Bar => 34.0,
        DockVariant::Split | DockVariant::Pill => 38.0,
    }
}

/// Inner radius of a control nested in a bordered control (segment in the mode switch,
/// column in a rail thumbnail): the control radius minus the 1px border (desk-07 `3px`
/// in `4px`, desk-09 `5px` in `6px`).
pub fn nested_radius(t: &Theme) -> Pixels {
    (t.radius_control - px(1.)).max(px(0.))
}

/// Right padding of the bottom row inside the bar surface: in floating layouts the row
/// ends at the main area's right edge, i.e. left of the scratchpad while it is shown
/// (desk-06: dock row `580 + 980 = 1560` = scratchpad left − gap).
pub fn bar_row_right_pad(t: &Theme, scratch_visible: bool) -> Pixels {
    if t.layout == LayoutVariant::Floating && scratch_visible { px(metrics::SCRATCH_W) + t.gap } else { px(0.) }
}

/// Whether a fullscreen window covers `output`: the visible workspace is a fullscreen
/// workspace (the compositor's `fullscreen-workspaces`; exact), else the active window is
/// fullscreen. Only the *passive overlay chrome* (strip header, canvas chip, minimap) asks:
/// it lives on the overlay layer, which the compositor draws over fullscreen windows. The
/// bar and the rail are on the top layer and never ask: the compositor covers them and
/// reveals them on an edge dwell, and they must always render their real content.
///
/// niri-ipc has no fullscreen flag for a window on an ordinary workspace;
/// a fullscreen tile is exactly the output's logical size (measured in the nested session:
/// `fullscreen-window` → tile 1920×1080; `maximize-column` → 1424×984, never the output
/// size while the bar's exclusive zone exists). Floating windows are excluded (a floating
/// window can be sized to the output by hand).
pub fn is_fullscreen(niri: &NiriState, output: &str) -> bool {
    let Some(ws) = niri.active_workspace(output) else { return false };
    if ashd_niri::is_fullscreen_workspace(ws) {
        return true;
    }
    let Some(o) = niri.outputs.get(output).and_then(|o| o.logical) else { return false };
    let Some(w) = ws.active_window_id.and_then(|id| niri.windows.get(&id)) else { return false };
    let (tw, th) = w.layout.tile_size;
    !w.is_floating && (tw - o.width as f64).abs() < 0.5 && (th - o.height as f64).abs() < 0.5
}

#[cfg(test)]
mod tests {
    use ashd_niri::niri_ipc::{Event, LogicalOutput, Output, Transform, Window, WindowLayout, Workspace};

    use super::*;

    fn output() -> Output {
        Output {
            name: "DP-1".into(),
            make: String::new(),
            model: String::new(),
            serial: None,
            physical_size: None,
            modes: vec![],
            current_mode: None,
            is_custom_mode: false,
            vrr_supported: false,
            vrr_enabled: false,
            logical: Some(LogicalOutput { x: 0, y: 0, width: 1920, height: 1080, scale: 1.0, transform: Transform::Normal }),
            max_bpc: None,
        }
    }

    fn win(id: u64, tile: (f64, f64), floating: bool) -> Window {
        Window {
            id,
            title: None,
            app_id: None,
            pid: None,
            workspace_id: Some(1),
            is_focused: true,
            is_floating: floating,
            is_urgent: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: Some((1, 1)),
                tile_size: tile,
                window_size: (tile.0 as i32, tile.1 as i32),
                tile_pos_in_workspace_view: None,
                window_offset_in_tile: (0.0, 0.0),
                pos_in_canvas: None,
                is_pinned: false,
            },
            focus_timestamp: None,
            hidden_from_workspace_id: None,
        }
    }

    fn state(w: Window) -> NiriState {
        let mut n = NiriState::default();
        n.outputs.insert("DP-1".into(), output());
        n.apply(Event::WorkspacesChanged {
            workspaces: vec![Workspace {
                id: 1,
                idx: 1,
                name: None,
                output: Some("DP-1".into()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: Some(w.id),
                mode: Default::default(),
                is_hidden: false,
                fullscreen_window_id: None,
                fullscreen_origin_id: None,
            }],
        });
        n.apply(Event::WindowsChanged { windows: vec![w] });
        n
    }

    #[test]
    fn fullscreen_is_an_output_sized_tile() {
        assert!(is_fullscreen(&state(win(7, (1920.0, 1080.0), false)), "DP-1"));
        assert!(!is_fullscreen(&state(win(7, (1424.0, 984.0), false)), "DP-1"));
        assert!(!is_fullscreen(&state(win(7, (1920.0, 1080.0), true)), "DP-1"));
        assert!(!is_fullscreen(&state(win(7, (1920.0, 1080.0), false)), "HDMI-A-1"));
    }

    #[test]
    fn a_fullscreen_workspace_is_fullscreen_whatever_the_tile_size() {
        let mut n = state(win(7, (1424.0, 984.0), false));
        assert!(!is_fullscreen(&n, "DP-1"));
        let mut fs = n.workspaces[&1].clone();
        fs.id = 2;
        fs.idx = 0;
        fs.fullscreen_origin_id = Some(1);
        fs.fullscreen_window_id = None; // its window has just left: still covered until the switch
        n.apply(Event::WorkspacesChanged { workspaces: vec![fs] });
        assert!(is_fullscreen(&n, "DP-1"));
    }

    #[test]
    fn label_caps_per_theme() {
        use ashd_tokens::ThemeName::*;
        for (name, caps) in [(Ash, true), (Ink, true), (Midnight, false), (Moss, true), (Dusk, false)] {
            let t = Theme::get(name);
            assert_eq!(t.label_caps, caps, "{name}");
            assert_eq!(t.label_text("scroll · 2 cols").as_ref(), if caps { "SCROLL · 2 COLS" } else { "scroll · 2 cols" });
        }
    }

    #[test]
    fn bottom_row_geometry() {
        let ink = Theme::get(ashd_tokens::ThemeName::Ink);
        assert_eq!(bar_row_right_pad(&ink, true), px(350.));
        assert_eq!(bar_row_right_pad(&ink, false), px(0.));
        assert_eq!(bar_row_right_pad(&Theme::get(ashd_tokens::ThemeName::Ash), true), px(0.));
        assert_eq!(dock_pill_radius(&Theme::get(ashd_tokens::ThemeName::Dusk)), px(24.));
        assert_eq!(nested_radius(&Theme::get(ashd_tokens::ThemeName::Midnight)), px(3.));
    }
}
