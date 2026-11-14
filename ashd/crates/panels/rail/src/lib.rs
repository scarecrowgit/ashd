//! Left workspace rail (desk-05: `left 0 top 0 width 64`): header cell with the output
//! name, one button per workspace of this output (number, window-count dots, active
//! marker), the dashed "new workspace" button, and the hover flyout.
//!
//! Panels exported (register all three in `ashd-shell`):
//! - [`RailPanel`] — `Frame(Rail)`: the column itself. Click focuses a workspace, the
//!   wheel switches, the bottom button focuses the trailing empty workspace.
//! - [`RailFlyoutPanel`] — `Passive`: the workspace flyout (title, column strip, window
//!   list, Rename / Move / Canvas) next to the hovered button.
//! - [`RailRenamePanel`] — `Modal`: the inline rename field (needs the keyboard, which a
//!   passive overlay never gets) → niri `SetWorkspaceName`.
//!
//! Owner: shell frame.

mod flyout;
pub mod geom;
mod hover;
mod rename;

use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollDelta, ScrollEvent, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::*, px,
};
pub use flyout::RailFlyoutPanel;
use hover::RailHover;
pub use rename::RailRenamePanel;
use ashd_niri::{
    CanvasWindow, NiriState, PlaneRect, Strip,
    niri_ipc::{Action, WorkspaceReferenceArg},
};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, Slot, frame};
use ashd_tokens::{RailVariant, metrics};
use ashd_ui::{Icon, Theme, icon, theme};

/// One row of the rail, derived from niri state.
#[derive(Clone, Debug, PartialEq)]
pub struct RailWorkspace {
    pub id: u64,
    /// The number shown: 1-based among the *visible* workspaces of the output (niri's
    /// `idx` also counts the hidden scratch workspace).
    pub idx: u8,
    pub name: Option<String>,
    pub active: bool,
    pub windows: usize,
}

/// The workspaces shown for `output`: niri's list in order (hidden scratch workspaces
/// left out), minus the trailing empty workspace niri always keeps (that one is the "new
/// workspace" button) unless it is the active one.
pub fn rail_workspaces(niri: &NiriState, output: &str) -> Vec<RailWorkspace> {
    let mut v: Vec<RailWorkspace> = niri
        .workspaces_on(output)
        .into_iter()
        .enumerate()
        .map(|(i, w)| RailWorkspace {
            id: w.id,
            idx: (i + 1).min(u8::MAX as usize) as u8,
            name: w.name.clone(),
            active: w.is_active,
            windows: niri.windows.values().filter(|win| win.workspace_id == Some(w.id)).count(),
        })
        .collect();
    if v.len() > 1 && v.last().is_some_and(|w| w.windows == 0 && !w.active && w.name.is_none()) {
        v.pop();
    }
    v
}

/// The workspace the "new workspace" button focuses: the trailing empty one.
pub fn trailing_empty(niri: &NiriState, output: &str) -> Option<u64> {
    let all = niri.workspaces_on(output);
    let last = all.last()?;
    let empty = !niri.windows.values().any(|w| w.workspace_id == Some(last.id));
    empty.then_some(last.id)
}

/// Workspace `steps` away from the active one on `output` (clamped), for the wheel.
pub fn neighbour(niri: &NiriState, output: &str, steps: i32) -> Option<u64> {
    let all = niri.workspaces_on(output);
    let cur = all.iter().position(|w| w.is_active)? as i32;
    let to = (cur + steps).clamp(0, all.len() as i32 - 1) as usize;
    (to as i32 != cur).then(|| all[to].id)
}

/// What a workspace preview (thumbnail, flyout strip) draws.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    /// Scroll mode: the strip columns.
    Strip(Strip),
    /// Canvas mode: the windows on the plane.
    Canvas(Vec<CanvasWindow>),
}

impl Preview {
    pub fn of(niri: &NiriState, ws: u64) -> Self {
        if niri.is_canvas(ws) { Preview::Canvas(niri.canvas_windows(ws)) } else { Preview::Strip(niri.strip(ws)) }
    }
}

/// Canvas window rects (focused flag, rect) scaled into a `w × h` box keeping the
/// aspect, centred.
pub fn canvas_preview_rects(windows: &[CanvasWindow], w: f64, h: f64) -> Vec<(bool, PlaneRect)> {
    let Some(b) = ashd_niri::canvas::bounds(windows, &[]) else { return Vec::new() };
    let scale = (w / b.w.max(1.0)).min(h / b.h.max(1.0));
    let (sx, sy) = ((w - b.w * scale) / 2.0, (h - b.h * scale) / 2.0);
    windows
        .iter()
        .map(|win| {
            let r = win.rect;
            (win.focused, PlaneRect::new((r.x - b.x) * scale + sx, (r.y - b.y) * scale + sy, r.w * scale, r.h * scale))
        })
        .collect()
}

/// A tiny canvas map: window rects in a `w × h` box (`surface_2` fill, `line` border;
/// focused `accent` when `lit`).
pub(crate) fn canvas_map(windows: &[CanvasWindow], w: f32, h: f32, lit: bool, t: &Theme) -> gpui::Div {
    let inner = px((f32::from(t.radius_control) / 3.0).floor());
    div().relative().w(px(w)).h(px(h)).flex_none().children(canvas_preview_rects(windows, w as f64, h as f64).into_iter().map(
        |(focused, r)| {
            div()
                .absolute()
                .left(px(r.x as f32))
                .top(px(r.y as f32))
                .w(px((r.w as f32).max(2.0)))
                .h(px((r.h as f32).max(2.0)))
                .bg(t.surface_2)
                .border_1()
                .border_color(if lit && focused { t.accent } else { t.line })
                .rounded(inner)
        },
    ))
}

/// "02 · code" — index and name as in the flyout title.
pub fn workspace_title(idx: u8, name: Option<&str>) -> String {
    match name {
        Some(n) if !n.is_empty() => format!("{idx:02} · {n}"),
        _ => format!("{idx:02}"),
    }
}

pub struct RailState {
    /// Smooth-scroll accumulator (pixels), for touchpads.
    pub scroll_acc: f32,
}

pub struct RailPanel {
    pub ctx: PanelContext,
    pub state: RailState,
    hover: Entity<RailHover>,
}

impl RailPanel {
    fn output(&self, cx: &gpui::App) -> Option<String> {
        let id = self.ctx.display.as_ref()?.id;
        self.ctx.shell.display(id, cx).and_then(|d| d.output)
    }

    fn focus(&self, id: u64, cx: &gpui::App) {
        self.ctx.shell.niri_action(Action::FocusWorkspace { reference: WorkspaceReferenceArg::Id(id) }, cx);
    }

    fn on_scroll(&mut self, ev: &ScrollEvent, cx: &mut Context<Self>) {
        // Wheel down (negative y) = next workspace, like niri's own bindings.
        let steps = match ev.delta {
            ScrollDelta::Lines(p) => -p.y.signum() as i32 * (p.y.abs() >= 0.5) as i32,
            ScrollDelta::Pixels(p) => {
                let dy = f32::from(p.y);
                if dy * self.state.scroll_acc < 0.0 {
                    self.state.scroll_acc = 0.0; // direction changed
                }
                self.state.scroll_acc += dy;
                let n = (self.state.scroll_acc / geom::SCROLL_STEP_PX).trunc();
                self.state.scroll_acc -= n * geom::SCROLL_STEP_PX;
                -(n as i32)
            }
        };
        if steps == 0 {
            return;
        }
        let Some(output) = self.output(cx) else { return };
        if let Some(id) = neighbour(self.ctx.niri.read(cx), &output, steps) {
            self.focus(id, cx);
        }
    }
}

impl Panel for RailPanel {
    const ID: PanelId = PanelId("rail");
    const PLACEMENT: Placement = Placement::Frame(Slot::Rail);

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.niri, |_, _, cx| cx.notify()).detach();
        let hover = hover::for_display(ctx.display.as_ref().map(|d| d.id), &ctx.shell, cx);
        cx.observe(&hover, |_, _, cx| cx.notify()).detach();
        // The output name arrives from niri after the surface exists.
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |_, _, cx| cx.notify()).detach();
        }
        Self { ctx, state: RailState { scroll_acc: 0.0 }, hover }
    }
}

impl RailPanel {
    /// Content of a `num` button (desk-05): active bar, number, window-count dots.
    fn num_content(ws: &RailWorkspace, t: &Theme) -> Vec<AnyElement> {
        let dot = if ws.active { t.accent } else { t.fg_muted };
        let mut out = Vec::new();
        if ws.active {
            out.push(
                div()
                    .absolute()
                    .left_0()
                    .top(px(geom::ACTIVE_BAR_INSET))
                    .bottom(px(geom::ACTIVE_BAR_INSET))
                    .w(px(geom::ACTIVE_BAR_W))
                    .bg(t.accent)
                    .into_any_element(),
            );
        }
        out.push(
            div()
                .font_family(t.font_mono.clone())
                .text_size(px(geom::NUMBER_SIZE))
                .line_height(px(geom::NUMBER_SIZE * 1.2))
                .font_weight(if ws.active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .child(format!("{:02}", ws.idx))
                .into_any_element(),
        );
        out.push(
            div()
                .flex()
                .h(px(geom::DOT))
                .gap(px(geom::DOT_GAP))
                .children((0..ws.windows.min(geom::MAX_DOTS)).map(|_| div().size(px(geom::DOT)).bg(dot)))
                .into_any_element(),
        );
        out
    }

    /// Content of a `thumb` button (desk-06/09): 72×40 column preview (canvas workspaces:
    /// a tiny map of the window rects) + "02 code".
    fn thumb_content(ws: &RailWorkspace, preview: &Preview, t: &Theme) -> Vec<AnyElement> {
        let outer = frame::nested_radius(t);
        let inner = px((f32::from(t.radius_control) / 3.0).floor());
        let empty = Strip::default();
        let (strip, map) = match preview {
            Preview::Strip(s) => (s, None),
            Preview::Canvas(w) => {
                let (iw, ih) = (geom::THUMB_W - 2.0 * (geom::THUMB_PAD + 1.0), geom::THUMB_H - 2.0 * (geom::THUMB_PAD + 1.0));
                (&empty, Some(canvas_map(w, iw, ih, ws.active, t)))
            }
        };
        let total: f64 = strip.columns.iter().map(|c| c.width.max(1.0)).sum();
        let cols = strip.columns.iter().map(|c| {
            let focused = ws.active && c.focused;
            let mut col = div()
                .h_full()
                .min_w(px(2.))
                .flex_basis(px(0.))
                .bg(t.surface_2)
                .border_1()
                .border_color(if focused { t.accent } else { t.line })
                .rounded(inner);
            col.style().flex_grow = Some((c.width.max(1.0) / total * 100.0) as f32);
            col
        });
        let thumb = div()
            .w(px(geom::THUMB_W))
            .h(px(geom::THUMB_H))
            .flex_none()
            .flex()
            .gap(px(geom::THUMB_COL_GAP))
            .p(px(geom::THUMB_PAD))
            .bg(t.bg)
            .border_1()
            .border_color(if ws.active { t.accent } else { t.line })
            .rounded(outer)
            .children(cols)
            .children(map);
        let name = ws.name.as_deref().filter(|n| !n.is_empty());
        let text = match name {
            Some(n) => format!("{:02} {n}", ws.idx),
            None => format!("{:02}", ws.idx),
        };
        let label = div()
            .max_w(t.rail_w - px(8.))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_family(t.font_mono.clone())
            .text_size(px(metrics::type_scale::LABEL.0))
            .line_height(px(metrics::type_scale::LABEL.1))
            .text_color(if ws.active { t.fg } else { t.fg_muted })
            .child(text);
        vec![thumb.into_any_element(), label.into_any_element()]
    }

    /// Content of a `pill` button (desk-07): 10px dot (active: 10×28 accent) + number.
    fn pill_content(ws: &RailWorkspace, t: &Theme) -> Vec<AnyElement> {
        let marker = div()
            .flex_none()
            .w(px(geom::PILL_DOT))
            .h(px(if ws.active { geom::PILL_ACTIVE_H } else { geom::PILL_DOT }))
            .rounded_full()
            .bg(if ws.active { t.accent } else { t.fg_muted });
        let number = div()
            .font_family(t.font_mono.clone())
            .text_size(px(geom::PILL_LABEL_SIZE))
            .line_height(px(geom::PILL_LABEL_SIZE * 1.2))
            .text_color(if ws.active { t.fg } else { t.fg_muted })
            .child(format!("{:02}", ws.idx));
        vec![marker.into_any_element(), number.into_any_element()]
    }
}

impl Render for RailPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let output = self.output(cx);
        let niri = self.ctx.niri.read(cx);
        let workspaces = output.as_deref().map(|o| rail_workspaces(niri, o)).unwrap_or_default();
        let previews: Vec<Preview> =
            if t.rail == RailVariant::Thumb { workspaces.iter().map(|w| Preview::of(niri, w.id)).collect() } else { Vec::new() };
        let trailing = output.as_deref().and_then(|o| trailing_empty(niri, o));
        let hovered = self.hover.read(cx).shown.map(|(id, _)| id);
        let (s2, muted) = (t.surface_2, t.fg_muted);
        let button_h = geom::button_h(t.rail);

        let buttons = workspaces.into_iter().enumerate().map(|(index, ws)| {
            let id = ws.id;
            let lit = ws.active || hovered == Some(id);
            let (gap, content) = match t.rail {
                RailVariant::Num => (geom::BUTTON_INNER_GAP, Self::num_content(&ws, &t)),
                RailVariant::Thumb => (geom::THUMB_LABEL_GAP, Self::thumb_content(&ws, &previews[index], &t)),
                RailVariant::Pill => (geom::PILL_LABEL_GAP, Self::pill_content(&ws, &t)),
            };
            div()
                .id(("rail-ws", id))
                .relative()
                .w_full()
                .h(px(button_h))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(gap))
                .cursor_pointer()
                .text_color(if lit { t.fg } else { t.fg_muted })
                .when(hovered == Some(id) && !ws.active, |d| d.bg(s2))
                .hover(move |s| s.bg(s2))
                .children(content)
                .on_hover(cx.listener(move |this, inside: &bool, _, cx| {
                    let inside = *inside;
                    this.hover.update(cx, |h, cx| if inside { h.enter_button(id, index, cx) } else { h.leave_rail(cx) });
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.focus(id, cx);
                    this.hover.update(cx, |h, cx| h.dismiss(cx));
                }))
        });

        let new_ws = div()
            .id("rail-new-workspace")
            .size(px(geom::NEW_BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_dashed()
            .border_color(t.line)
            .rounded(t.radius_control)
            .text_color(muted)
            .cursor_pointer()
            .hover(move |s| s.bg(s2))
            .child(icon(Icon::Plus, geom::NEW_ICON).text_color(muted))
            .on_click(cx.listener(move |this, _, _, cx| match trailing {
                Some(id) => this.focus(id, cx),
                // Config without an empty trailing workspace: go one past the last.
                None => this.ctx.shell.niri_action(Action::FocusWorkspaceDown {}, cx),
            }));

        div()
            .id("rail")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, _, cx| this.on_scroll(ev, cx)))
            .child(
                div()
                    .h(px(metrics::RAIL_HEADER_H))
                    .flex_none()
                    .mx(px(geom::header_inset(&t)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_b_1()
                    .border_color(t.line)
                    .font_family(t.font_mono.clone())
                    .text_size(px(geom::HEADER_SIZE))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.fg)
                    .child(output.unwrap_or_else(|| "—".into())),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(geom::BUTTONS_TOP))
                    .flex()
                    .flex_col()
                    .gap(px(geom::BUTTON_GAP))
                    .children(buttons),
            )
            .child(div().absolute().left_0().right_0().bottom(px(geom::NEW_BUTTON_BOTTOM)).flex().justify_center().child(new_ws))
    }
}

#[cfg(test)]
mod tests {
    use ashd_niri::niri_ipc::{Event, Window, WindowLayout, Workspace};

    use super::*;

    fn ws(id: u64, idx: u8, active: bool) -> Workspace {
        Workspace {
            id,
            idx,
            name: None,
            output: Some("DP-1".into()),
            is_urgent: false,
            is_active: active,
            is_focused: active,
            active_window_id: None,
            mode: Default::default(),
            is_hidden: false,
        }
    }

    fn win(id: u64, ws: u64) -> Window {
        Window {
            id,
            title: None,
            app_id: None,
            pid: None,
            workspace_id: Some(ws),
            is_focused: false,
            is_floating: false,
            is_urgent: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: Some((1, 1)),
                tile_size: (100.0, 100.0),
                window_size: (100, 100),
                tile_pos_in_workspace_view: None,
                window_offset_in_tile: (0.0, 0.0),
                pos_in_canvas: None,
                is_pinned: false,
            },
            focus_timestamp: None,
            hidden_from_workspace_id: None,
        }
    }

    #[test]
    fn trailing_empty_is_hidden_and_targeted() {
        let mut n = NiriState::default();
        n.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, true), ws(2, 2, false), ws(3, 3, false)] });
        n.apply(Event::WindowsChanged { windows: vec![win(10, 1), win(11, 1), win(12, 2)] });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.idx, w.windows)).collect::<Vec<_>>(), vec![(1, 2), (2, 1)]);
        assert_eq!(trailing_empty(&n, "DP-1"), Some(3));
        assert_eq!(neighbour(&n, "DP-1", 1), Some(2));
        assert_eq!(neighbour(&n, "DP-1", -1), None);
        assert_eq!(workspace_title(2, Some("code")), "02 · code");

        // A hidden scratch workspace (idx 1) is not shown and does not shift the numbers.
        let mut scratch = ws(4, 1, false);
        scratch.is_hidden = true;
        scratch.name = Some("scratch".into());
        n.apply(Event::WorkspacesChanged { workspaces: vec![scratch, ws(1, 2, true), ws(2, 3, false), ws(3, 4, false)] });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.id, w.idx)).collect::<Vec<_>>(), vec![(1, 1), (2, 2)]);
        assert_eq!(trailing_empty(&n, "DP-1"), Some(3));

        // The current fork: hidden workspaces have idx 0, the rest their visual index.
        let mut scratch = ws(4, 0, false);
        scratch.is_hidden = true;
        n.apply(Event::WorkspacesChanged { workspaces: vec![scratch, ws(1, 1, true), ws(2, 2, false), ws(3, 3, false)] });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.id, w.idx)).collect::<Vec<_>>(), vec![(1, 1), (2, 2)]);
        assert_eq!(trailing_empty(&n, "DP-1"), Some(3));
    }

    #[test]
    fn canvas_preview_maps_window_rects() {
        let windows = [
            CanvasWindow { id: 1, rect: PlaneRect::new(0.0, 0.0, 1000.0, 500.0), focused: true, pinned: false },
            CanvasWindow { id: 2, rect: PlaneRect::new(1000.0, 500.0, 1000.0, 500.0), focused: false, pinned: false },
        ];
        let r = canvas_preview_rects(&windows, 60.0, 30.0);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|(_, m)| m.x >= -1e-6 && m.y >= -1e-6 && m.right() <= 60.0 + 1e-6 && m.bottom() <= 30.0 + 1e-6));
        assert!(r[0].0 && !r[1].0);
        assert!(canvas_preview_rects(&[], 60.0, 30.0).is_empty());
    }
}
