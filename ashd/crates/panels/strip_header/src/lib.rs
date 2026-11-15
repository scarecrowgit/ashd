//! Strip header (desk-05 `left 12px; right 12px; top 10px; height 24px` inside the main
//! area): a passive-overlay row on each display while its active workspace is in Scroll
//! (strip) mode and has at least one column:
//!
//! `02 code` · `scroll` · column map · `col 3/5 · 62%` ······ `‹` `›` · real key hint
//!
//! - Column map: one 10px bar per column, width proportional to the column width (a
//!   column as wide as the working area is [`MAP_FULL`] px), 3px apart; the focused column
//!   is accent-filled; a column with several windows is drawn as stacked segments. Click
//!   focuses the column's first window (`focus-window`).
//! - Viewport marker: a 1px `fg` frame over the map spanning what the output shows right
//!   now, driven by the live `StripViewChanged` events (no spring: it follows them).
//! - Readout: focused column index / count and its width as % of the working area.
//! - `‹` `›`: `focus-column-left/right` (on this display's workspace).
//!
//! Re-renders: on structural niri changes that alter the model, and on live strip view
//! changes of this display's workspace only.
//!
//! Owner: canvas UI.

use std::sync::Arc;

use gpui::{
    App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use ashd_niri::{LiveViews, niri_ipc::Action};
use ashd_panel_canvas_hud::common::{self, Hover, KeyBinds, ModeFade, SWAP_TRAVEL};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, hit_area};
use ashd_tokens::metrics;
use ashd_ui::{
    Icon,
    anim::{EASE_STANDARD, motion::timing},
    follow, mix,
    presence::Timing,
    swap, theme,
};

/// Row offset inside the main area (desk-05 `left 12px; top 10px`), height 24, gap 12.
pub const HEADER_LEFT: f32 = 12.0;
pub const HEADER_TOP: f32 = metrics::STRIP_HEADER_TOP;
pub const HEADER_H: f32 = metrics::STRIP_HEADER_H;
const ROW_GAP: f32 = 12.0;
/// Column map: bar height 10, 3 apart (desk-05 l.349).
const BAR_H: f32 = 10.0;
const BAR_GAP: f32 = 3.0;
/// Bar width of a column exactly as wide as the working area (desk-05: 927 of 1492 =
/// 62% → 66px).
pub const MAP_FULL: f32 = 106.0;
const BAR_MIN: f32 = 6.0;
/// The map never grows past this share of the main-area width.
const MAP_MAX_SHARE: f32 = 0.4;
/// Prev/next buttons (desk-05 `24×24`).
const BUTTON: f32 = 24.0;
/// The viewport marker sticks out of the bars by this much (top and bottom).
const MARKER_OUT: f32 = 2.0;
/// What niri's top strut must reserve for this header in the attached layout: its top inset
/// and height plus the focus ring's reach ([`metrics::STRIP_HEADER_STRUT`]). The mockup stops
/// 8 px under the row (desk-05: strip at `top 42`), which leaves 3 px between the key chips and
/// the focused window's ring; the ring's 5 px are added so the chips clear it by the layout
/// gap. Floating themes reserve `gap + header + gap` instead (`render::niri_top_strut`): the
/// chips clear the work-area outline by one gap.
pub const STRUT_NEEDED: f32 = HEADER_TOP + HEADER_H + metrics::WINDOW_RING_GAP + metrics::WINDOW_RING_W;

#[derive(Clone, Debug, PartialEq)]
pub struct HeaderColumn {
    pub index: usize,
    pub first_window: u64,
    pub tiles: usize,
    pub width: f64,
    pub focused: bool,
}

/// What the header shows (structural part).
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderModel {
    pub workspace: u64,
    pub label: String,
    pub columns: Vec<HeaderColumn>,
    pub hint: Option<String>,
    /// Fallback working-area width before the first strip view arrives.
    pub area_width: f64,
}

/// The live part: the strip view of the header's workspace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveStrip {
    pub view_offset: f64,
    pub working_area_width: f64,
    pub column_xs: Vec<f64>,
}

/// Bar widths for `columns` (px), scaled down to fit `max_total` with their gaps.
pub fn bar_widths(columns: &[HeaderColumn], working_w: f64, max_total: f32) -> Vec<f32> {
    let ww = if working_w > 1.0 { working_w } else { 1.0 };
    let mut w: Vec<f32> = columns.iter().map(|c| ((c.width / ww) as f32 * MAP_FULL).max(BAR_MIN)).collect();
    let gaps = BAR_GAP * columns.len().saturating_sub(1) as f32;
    let total: f32 = w.iter().sum::<f32>() + gaps;
    if total > max_total && max_total > gaps {
        let k = (max_total - gaps) / (total - gaps);
        for x in &mut w {
            *x = (*x * k).max(2.0);
        }
    }
    w
}

/// Map a strip x (logical px, strip coordinates) onto the column map, piecewise linear:
/// inside a column proportionally to its bar, across a gap proportionally to the 3px gap,
/// beyond the ends at the end column's scale.
pub fn strip_to_map(x: f64, lefts: &[f64], widths: &[f64], bars: &[f32]) -> f32 {
    let n = lefts.len().min(widths.len()).min(bars.len());
    if n == 0 {
        return 0.0;
    }
    let mut bar_left = Vec::with_capacity(n);
    let mut acc = 0.0f32;
    for b in &bars[..n] {
        bar_left.push(acc);
        acc += b + BAR_GAP;
    }
    let scale = |i: usize| if widths[i] > 0.0 { bars[i] as f64 / widths[i] } else { 0.0 };
    if x < lefts[0] {
        return bar_left[0] + ((x - lefts[0]) * scale(0)) as f32;
    }
    for i in 0..n {
        let (l, r) = (lefts[i], lefts[i] + widths[i]);
        if x <= r {
            return bar_left[i] + ((x - l).max(0.0) * scale(i)) as f32;
        }
        if i + 1 < n && x < lefts[i + 1] {
            let gap = lefts[i + 1] - r;
            let f = if gap > 0.0 { ((x - r) / gap) as f32 } else { 0.0 };
            return bar_left[i] + bars[i] + f * BAR_GAP;
        }
    }
    let last = n - 1;
    bar_left[last] + bars[last] + ((x - lefts[last] - widths[last]) * scale(last)) as f32
}

pub struct StripHeaderPanel {
    pub ctx: PanelContext,
    views: Entity<Arc<LiveViews>>,
    binds: Entity<KeyBinds>,
    model: Option<HeaderModel>,
    live: Option<LiveStrip>,
    hover: Hover,
    /// The header comes and goes with the mode (280 ms out-cubic).
    fade: ModeFade<HeaderModel>,
}

impl StripHeaderPanel {
    fn compute(&self, cx: &App) -> Option<HeaderModel> {
        let t = theme(cx);
        let d = common::display_ctx(&self.ctx, t, cx)?;
        let niri = self.ctx.niri.read(cx);
        if common::suppressed(niri, d.output.as_deref()) {
            return None;
        }
        let ws = common::active_workspace(niri, d.output.as_deref())?;
        if niri.is_canvas(ws.id) {
            return None;
        }
        let strip = niri.strip(ws.id);
        if strip.is_empty() {
            return None;
        }
        let columns = strip
            .columns
            .iter()
            .filter_map(|c| {
                Some(HeaderColumn {
                    index: c.index,
                    first_window: *c.windows.first()?,
                    tiles: c.windows.len(),
                    width: c.width,
                    focused: c.focused,
                })
            })
            .collect();
        Some(HeaderModel {
            workspace: ws.id,
            label: common::workspace_label(niri, ws),
            columns,
            hint: self.binds.read(cx).pair("focus-column-left", "focus-column-right", false),
            area_width: f64::from(f32::from(d.area.size.width)),
        })
    }

    fn compute_live(&self, cx: &App) -> Option<LiveStrip> {
        let ws = self.model.as_ref()?.workspace;
        let v = self.views.read(cx).strip(ws).cloned().or_else(|| self.ctx.niri.read(cx).strip_view(ws).cloned())?;
        Some(LiveStrip { view_offset: v.view_offset, working_area_width: v.working_area_width, column_xs: v.column_xs })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = self.compute(cx);
        let changed = next != self.model;
        let flips = next.is_some() != self.model.is_some();
        self.model = next;
        if changed {
            self.fade.sync(self.model.as_ref());
        }
        let live = self.compute_live(cx);
        if changed || live != self.live {
            self.live = live;
            cx.notify();
        }
        if flips {
            self.ctx.shell.refresh_overlays(cx);
        }
    }

    fn refresh_live(&mut self, cx: &mut Context<Self>) {
        let live = self.compute_live(cx);
        if live != self.live {
            self.live = live;
            cx.notify();
        }
    }

    fn column_action(&self, action: Action, cx: &mut App) {
        let Some(ws) = self.model.as_ref().map(|m| m.workspace) else { return };
        let focus = !self.ctx.niri.read(cx).workspaces.get(&ws).is_some_and(|w| w.is_focused);
        common::run_actions(&self.ctx.niri_handle, focus.then_some(ws), vec![action], cx);
    }
}

impl Panel for StripHeaderPanel {
    const ID: PanelId = PanelId("strip-header");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let views = common::live_views(&ctx.niri_handle, cx);
        let binds = common::keybinds(cx);
        cx.observe(&ctx.niri, |this, _, cx| this.refresh(cx)).detach();
        cx.observe(&views, |this, _, cx| this.refresh_live(cx)).detach();
        cx.observe(&binds, |this, _, cx| this.refresh(cx)).detach();
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |this, _, cx| this.refresh(cx)).detach();
        }
        let mut this = Self { ctx, views, binds, model: None, live: None, hover: Hover::default(), fade: ModeFade::new(None) };
        this.model = this.compute(cx);
        this.fade = ModeFade::new(this.model.clone());
        this.live = this.compute_live(cx);
        this
    }

    fn is_live(&self, _cx: &App) -> bool {
        self.fade.is_live()
    }
}

fn hover_of(this: &mut StripHeaderPanel) -> &mut Hover {
    &mut this.hover
}

impl Render for StripHeaderPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().relative();
        let shown = self.fade.view();
        if self.fade.animating() {
            window.request_animation_frame();
        }
        if self.fade.take_gone() {
            // the exit is over: the overlay host may drop the surface
            self.ctx.shell.refresh_overlays(cx);
        }
        let Some(common::Shown { content: m, fade, travel, leaving }) = shown else { return root };
        let t = theme(cx).clone();
        let Some(d) = common::display_ctx(&self.ctx, &t, cx) else { return root };
        let live = self.live.clone();
        let working_w = live.as_ref().map(|l| l.working_area_width).filter(|w| *w > 1.0).unwrap_or(m.area_width);
        let max_map = f32::from(d.area.size.width) * MAP_MAX_SHARE;
        let bars = bar_widths(&m.columns, working_w, max_map);
        let map_w: f32 = bars.iter().sum::<f32>() + BAR_GAP * bars.len().saturating_sub(1) as f32;
        let bar_radius = t.radius_control.min(px(2.));

        let n = m.columns.len();
        let focused_pos = m.columns.iter().position(|c| c.focused);
        let readout = match focused_pos {
            Some(i) => {
                let pct = (m.columns[i].width / working_w * 100.0).round() as i64;
                format!("col {}/{n} · {pct}%", i + 1)
            }
            None => format!("{n} cols"),
        };

        let bar_els = m.columns.iter().zip(&bars).map(|(c, w)| {
            let tiles = c.tiles.clamp(1, 4);
            let seg_h = (BAR_H - (tiles - 1) as f32) / tiles as f32;
            let (id, index, focused, width) = (c.first_window, c.index as u64, c.focused, *w);
            let (accent, line) = (t.accent, t.line);
            let ease = Timing::new(timing::DOCK_INDICATOR + 80, EASE_STANDARD); // 240 ms standard (the focus ring's)
            let on_click = cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                this.ctx.shell.niri_action(Action::FocusWindow { id }, cx);
            });
            // The bar's width follows its column (a resize glides), the focus ring's accent fades
            // from column to column.
            follow(("strip-header-col-w", index), width, ease, move |w| {
                div()
                    .id(("strip-header-col", index))
                    .flex_none()
                    .w(px(w))
                    .h(px(BAR_H))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .cursor_pointer()
                    .child(follow(("strip-header-col-f", index), if focused { 1.0 } else { 0.0 }, ease, move |p| {
                        div().size_full().flex().flex_col().gap(px(1.)).children((0..tiles).map(move |_| {
                            div()
                                .w_full()
                                .h(px(seg_h))
                                .border_1()
                                .border_color(mix(line, accent, p))
                                .rounded(bar_radius)
                                .bg(mix(gpui::transparent_black(), accent, p))
                        }))
                    }).fade())
                    .on_click(on_click)
            })
        });

        // Viewport marker over the map.
        let marker = live.as_ref().filter(|l| l.column_xs.len() == m.columns.len()).map(|l| {
            let widths: Vec<f64> = m.columns.iter().map(|c| c.width).collect();
            let a = strip_to_map(l.view_offset, &l.column_xs, &widths, &bars).max(-MARKER_OUT);
            let b = strip_to_map(l.view_offset + working_w, &l.column_xs, &widths, &bars).min(map_w + MARKER_OUT);
            div()
                .absolute()
                .left(px(a - 1.0))
                .top(px(-MARKER_OUT - 1.0))
                .w(px((b - a + 2.0).max(2.0)))
                .h(px(BAR_H + 2.0 * MARKER_OUT + 2.0))
                .border_1()
                .border_color(t.fg)
                .rounded(bar_radius)
        });
        let map = div()
            .relative()
            .flex()
            .items_center()
            .gap(px(BAR_GAP))
            .h(px(HEADER_H))
            .children(bar_els)
            .children(marker.map(|mk| div().absolute().left_0().top(px((HEADER_H - BAR_H) / 2.0)).child(mk)));

        let prev = common::track_hover(
            common::square_button("strip-header-prev", Icon::ChevronLeft, BUTTON, self.hover.is("prev"), &t),
            "prev",
            hover_of,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| this.column_action(Action::FocusColumnLeft {}, cx)));
        let next = common::track_hover(
            common::square_button("strip-header-next", Icon::ChevronRight, BUTTON, self.hover.is("next"), &t),
            "next",
            hover_of,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| this.column_action(Action::FocusColumnRight {}, cx)));
        let buttons = div().flex().items_center().gap(px(BAR_GAP * 2.0)).child(prev).child(next);
        // While the header leaves (the other mode's chips are coming in) it takes no clicks.
        let (map, buttons) =
            if leaving { (div().relative().child(map), div().relative().child(buttons)) } else { (hit_area(map), hit_area(buttons)) };

        let row = div()
            .absolute()
            .left(d.area.origin.x + px(HEADER_LEFT))
            .top(d.area.origin.y + px(HEADER_TOP) - px(SWAP_TRAVEL * (1.0 - travel)))
            .opacity(fade)
            .w(d.area.size.width - px(2.0 * HEADER_LEFT))
            .h(px(HEADER_H))
            .flex()
            .items_center()
            .gap(px(ROW_GAP))
            // a changed workspace name or readout dips and comes back (120 ms)
            .child({
                let (text, tt) = (m.label.clone(), t.clone());
                swap("strip-header-label", text.clone(), move |p| common::label(&text, tt.fg_muted, &tt).opacity(p))
            })
            .child(common::label("scroll", t.fg, &t))
            .child(map)
            .child({
                let tt = t.clone();
                swap("strip-header-readout", readout.clone(), move |p| common::label(&readout, tt.fg_muted, &tt).opacity(p))
            })
            .child(div().flex_1())
            .child(buttons)
            .children(m.hint.map(|h| common::label(h, t.fg_muted, &t)))
            .child(common::hover_reset(hover_of, cx));
        root.child(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(index: usize, width: f64, focused: bool) -> HeaderColumn {
        HeaderColumn { index, first_window: index as u64, tiles: 1, width, focused }
    }

    #[test]
    fn bars_follow_the_mockup_scale() {
        // desk-05: widths 442 624 927 624 442 in a 1492 working area → 31/44/66/44/31.
        let cols: Vec<_> = [442.0, 624.0, 927.0, 624.0, 442.0].iter().enumerate().map(|(i, w)| col(i + 1, *w, i == 2)).collect();
        let bars = bar_widths(&cols, 1492.0, 1000.0);
        assert_eq!(bars.iter().map(|b| b.round() as i32).collect::<Vec<_>>(), vec![31, 44, 66, 44, 31]);
        // Too many columns: scaled down to the cap, gaps kept.
        let many: Vec<_> = (1..=40).map(|i| col(i, 1492.0, false)).collect();
        let bars = bar_widths(&many, 1492.0, 400.0);
        let total: f32 = bars.iter().sum::<f32>() + BAR_GAP * 39.0;
        assert!((total - 400.0).abs() < 1.0, "{total}");
    }

    #[test]
    fn strip_coordinates_map_piecewise() {
        let lefts = [0.0, 454.0, 1090.0];
        let widths = [442.0, 624.0, 927.0];
        let bars = [31.0, 44.0, 66.0];
        assert_eq!(strip_to_map(0.0, &lefts, &widths, &bars), 0.0);
        assert!((strip_to_map(442.0, &lefts, &widths, &bars) - 31.0).abs() < 1e-3);
        // Middle of the gap between columns 1 and 2 → middle of the 3px gap.
        assert!((strip_to_map(448.0, &lefts, &widths, &bars) - 32.5).abs() < 1e-3);
        assert!((strip_to_map(454.0, &lefts, &widths, &bars) - 34.0).abs() < 1e-3);
        // End of the last column, and beyond it at the last column's scale.
        let end = 31.0 + 3.0 + 44.0 + 3.0 + 66.0;
        assert!((strip_to_map(2017.0, &lefts, &widths, &bars) - end).abs() < 1e-3);
        assert!(strip_to_map(2500.0, &lefts, &widths, &bars) > end);
        assert!(strip_to_map(-100.0, &lefts, &widths, &bars) < 0.0);
        assert_eq!(strip_to_map(5.0, &[], &[], &[]), 0.0);
    }

    #[test]
    fn strut_leaves_the_layout_gap_between_the_chips_and_the_nearest_edge_below() {
        use ashd_tokens::{LayoutVariant, ThemeName, render::niri_gaps, render::niri_top_strut, tokens};
        let ring = metrics::WINDOW_RING_GAP + metrics::WINDOW_RING_W;
        assert_eq!(STRUT_NEEDED, 39.0);
        for name in ThemeName::ALL {
            let t = tokens(name);
            let gap = niri_gaps(t);
            let strut = niri_top_strut(t);
            // The row's bottom edge: the main area's inset (floating themes) + top + height.
            let (inset, nearest) = match t.shape.layout {
                // Attached: no outline; the topmost window's ring is the nearest edge.
                LayoutVariant::Attached => {
                    assert_eq!(strut, STRUT_NEEDED, "{name}");
                    (0.0, strut + gap - ring)
                }
                // Floating: the work-area outline sits at the strut.
                LayoutVariant::Floating => (t.shape.gap, strut),
            };
            let header_bottom = inset + HEADER_TOP + HEADER_H;
            assert_eq!(nearest - header_bottom, gap, "{name}: chips to the nearest edge below");
            // And the windows themselves are further away than before (the mockup: 8).
            assert!(strut + gap - header_bottom > 8.0, "{name}");
        }
    }
}
