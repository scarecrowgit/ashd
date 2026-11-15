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

use std::path::PathBuf;

use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render,
    ScrollDelta, ScrollEvent, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px,
};
pub use flyout::RailFlyoutPanel;
use hover::RailHover;
pub use rename::RailRenamePanel;
use ashd_niri::{
    CanvasWindow, NiriState, PlaneRect, Strip,
    niri_ipc::{Action, WorkspaceReferenceArg},
};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, Slot, apps::app_for_app_id, apps::app_glyph, frame};
use ashd_tokens::{RailVariant, metrics};
use ashd_ui::{
    Icon, Theme,
    anim::{EASE_STANDARD, SPRING_WORKSPACE, SpringValue, timing},
    fx::{follow, hover_wash, mix},
    icon,
    presence::Timing,
    theme,
};

/// What a rail entry stands for.
#[derive(Clone, Debug, PartialEq)]
pub enum RailKind {
    /// A numbered desk.
    Desk,
    /// A workspace the compositor made for a fullscreen window (`fullscreen-workspaces`):
    /// drawn as a compact app entry under its origin desk, not as a desk.
    Fullscreen {
        /// The desk the window came from (its number is `RailWorkspace::idx`).
        origin: u64,
        /// The window's app id and title (`None` for the moment its window has just left).
        app_id: Option<String>,
        title: Option<String>,
    },
}

/// One row of the rail, derived from niri state.
#[derive(Clone, Debug, PartialEq)]
pub struct RailWorkspace {
    pub id: u64,
    /// The number shown: 1-based among the *desks* of the output (hidden scratch and
    /// fullscreen workspaces are not counted; niri's own `idx` is 0 for both). A fullscreen
    /// entry carries its origin desk's number (0 when that desk is gone).
    pub idx: u8,
    pub name: Option<String>,
    pub active: bool,
    pub windows: usize,
    pub kind: RailKind,
}

impl RailWorkspace {
    pub fn is_fullscreen(&self) -> bool {
        matches!(self.kind, RailKind::Fullscreen { .. })
    }
}

/// The workspaces shown for `output`, in on-screen order (hidden scratch workspaces left
/// out; a fullscreen workspace stands right after its origin desk), minus the trailing empty
/// desk niri always keeps (that one is the "new workspace" button) unless it is the active
/// one or a fullscreen window came from it.
pub fn rail_workspaces(niri: &NiriState, output: &str) -> Vec<RailWorkspace> {
    let mut desk = 0usize;
    let mut v: Vec<RailWorkspace> = niri
        .workspaces_on(output)
        .into_iter()
        .map(|w| {
            let count = niri.windows.values().filter(|win| win.workspace_id == Some(w.id)).count();
            let kind = match w.fullscreen_origin_id {
                None => {
                    desk += 1;
                    RailKind::Desk
                }
                Some(origin) => {
                    let win = niri.fullscreen_window(w.id).or_else(|| niri.windows_on(w.id).into_iter().next());
                    RailKind::Fullscreen {
                        origin,
                        app_id: win.and_then(|x| x.app_id.clone()).filter(|a| !a.is_empty()),
                        title: win.and_then(|x| x.title.clone()).filter(|t| !t.is_empty()),
                    }
                }
            };
            let number = match kind {
                RailKind::Desk => desk,
                RailKind::Fullscreen { .. } => niri.desk_number(w.id).unwrap_or(0),
            };
            RailWorkspace {
                id: w.id,
                idx: number.min(u8::MAX as usize) as u8,
                name: w.name.clone(),
                active: w.is_active,
                windows: count,
                kind,
            }
        })
        .collect();
    if let Some(last) = trailing_empty_pos(&v)
        && v.iter().filter(|w| !w.is_fullscreen()).count() > 1
        && !v[last].active
        && v[last].name.is_none()
    {
        v.remove(last);
    }
    v
}

/// Position of the last desk when it is empty and nothing fullscreen came from it.
fn trailing_empty_pos(v: &[RailWorkspace]) -> Option<usize> {
    let last = v.iter().rposition(|w| !w.is_fullscreen())?;
    let used = v.iter().any(|w| matches!(w.kind, RailKind::Fullscreen { origin, .. } if origin == v[last].id));
    (v[last].windows == 0 && !used).then_some(last)
}

/// The workspace the "new workspace" button focuses: the trailing empty desk.
pub fn trailing_empty(niri: &NiriState, output: &str) -> Option<u64> {
    let all = niri.workspaces_on(output);
    let last = all.iter().rev().find(|w| w.fullscreen_origin_id.is_none())?;
    let empty = !niri.windows.values().any(|w| w.workspace_id == Some(last.id));
    let used = all.iter().any(|w| w.fullscreen_origin_id == Some(last.id));
    (empty && !used).then_some(last.id)
}

/// Workspace `steps` away from the active one on `output` (clamped), for the wheel: fullscreen
/// workspaces are on the way (workspace up/down reaches them too).
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
    /// `num` rail: the y of the active marker (spring 1.0 / 1000 like niri's workspace
    /// switch): it slides from workspace to workspace instead of jumping.
    marker: Option<SpringValue>,
    /// Its height (a fullscreen entry is shorter than a desk button).
    marker_h: Option<SpringValue>,
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
        cx.observe(&ctx.services.apps.state, |_, _, cx| cx.notify()).detach();
        // The output name arrives from niri after the surface exists.
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |_, _, cx| cx.notify()).detach();
        }
        Self { ctx, state: RailState { scroll_acc: 0.0, marker: None, marker_h: None }, hover }
    }
}

impl RailPanel {
    /// Content of a `num` button (desk-05): number and window-count dots; the active bar is
    /// the rail's sliding marker. `lit` (active or hovered) eases the colours (120 ms).
    fn num_content(ws: &RailWorkspace, lit: bool, t: &Theme) -> Vec<AnyElement> {
        let (fg, muted, accent) = (t.fg, t.fg_muted, t.accent);
        let (active, windows, idx) = (ws.active, ws.windows, ws.idx);
        let (mono, id) = (t.font_mono.clone(), ws.id);
        let fx = follow(
            gpui::ElementId::NamedInteger("rail-num-fx".into(), id),
            if lit { 1.0 } else { 0.0 },
            Timing::new(timing::SWAP, EASE_STANDARD),
            move |p| {
                let dot = if active { accent } else { mix(muted, fg, p) };
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(geom::BUTTON_INNER_GAP))
                    .text_color(mix(muted, fg, p))
                    .child(
                        div()
                            .font_family(mono)
                            .text_size(px(geom::NUMBER_SIZE))
                            .line_height(px(geom::NUMBER_SIZE * 1.2))
                            .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .child(format!("{idx:02}")),
                    )
                    .child(
                        div()
                            .flex()
                            .h(px(geom::DOT))
                            .gap(px(geom::DOT_GAP))
                            .children((0..windows.min(geom::MAX_DOTS)).map(|_| div().size(px(geom::DOT)).bg(dot))),
                    )
            },
        )
        .fade();
        vec![fx.into_any_element()]
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
            .children(map)
            // The active workspace's accent frame fades in over the line border (120 ms).
            .child(follow(
                gpui::ElementId::NamedInteger("rail-thumb-ring".into(), ws.id),
                if ws.active { 1.0 } else { 0.0 },
                Timing::new(timing::SWAP, EASE_STANDARD),
                {
                    let accent = t.accent;
                    move |p| {
                        div()
                            .absolute()
                            .top(px(-1.))
                            .left(px(-1.))
                            .right(px(-1.))
                            .bottom(px(-1.))
                            .border_1()
                            .border_color(mix(gpui::transparent_black(), accent, p))
                            .rounded(outer)
                    }
                },
            ).fade());
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
        // The active marker grows from a dot to a bar and lights up (120 ms).
        let (accent, muted, fg, mono, id, active) = (t.accent, t.fg_muted, t.fg, t.font_mono.clone(), ws.id, ws.active);
        let number = format!("{:02}", ws.idx);
        let fx = follow(
            gpui::ElementId::NamedInteger("rail-pill-fx".into(), id),
            if active { 1.0 } else { 0.0 },
            Timing::new(timing::SWAP, EASE_STANDARD),
            move |p| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(geom::PILL_LABEL_GAP))
                    .child(
                        div()
                            .flex_none()
                            .w(px(geom::PILL_DOT))
                            .h(px(geom::PILL_DOT + (geom::PILL_ACTIVE_H - geom::PILL_DOT) * p))
                            .rounded_full()
                            .bg(mix(muted, accent, p)),
                    )
                    .child(
                        div()
                            .font_family(mono)
                            .text_size(px(geom::PILL_LABEL_SIZE))
                            .line_height(px(geom::PILL_LABEL_SIZE * 1.2))
                            .text_color(mix(muted, fg, p))
                            .child(number),
                    )
            },
        )
        .fade();
        vec![fx.into_any_element()]
    }
}


impl RailPanel {
    /// Content of a fullscreen workspace's entry (every rail variant): the window's app icon
    /// in a compact row, tied to the desk above by a 1 px elbow. `lit` (active or hovered)
    /// eases the connector and the icon up (120 ms).
    fn fullscreen_content(ws: &RailWorkspace, icon_path: Option<PathBuf>, glyph: Icon, lit: bool, t: &Theme) -> Vec<AnyElement> {
        let (line, accent, muted, fg, active, id) = (t.line, t.accent, t.fg_muted, t.fg, ws.active, ws.id);
        let fx = follow(
            gpui::ElementId::NamedInteger("rail-fs-fx".into(), id),
            if lit { 1.0 } else { 0.0 },
            Timing::new(timing::SWAP, EASE_STANDARD),
            move |p| {
                let tie = if active { accent } else { mix(line, muted, p) };
                let icon_el: AnyElement = match &icon_path {
                    Some(path) => img(path.clone())
                        .size(px(geom::FS_ICON))
                        .flex_none()
                        .object_fit(ObjectFit::Contain)
                        .opacity(0.6 + 0.4 * p)
                        .with_fallback(move || icon(glyph, geom::FS_ICON).text_color(mix(muted, fg, p)).into_any_element())
                        .into_any_element(),
                    None => icon(glyph, geom::FS_ICON).text_color(mix(muted, fg, p)).into_any_element(),
                };
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    // elbow: down from the desk above, then across to the icon
                    .child(div().absolute().left(px(geom::FS_STEM_X)).top(px(-geom::BUTTON_GAP)).w(px(1.)).h(px(geom::FS_H / 2.0 + geom::BUTTON_GAP)).bg(tie))
                    .child(div().absolute().left(px(geom::FS_STEM_X)).top(px(geom::FS_H / 2.0)).w(px(geom::FS_FOOT)).h(px(1.)).bg(tie))
                    .child(icon_el)
            },
        )
        .fade();
        vec![fx.into_any_element()]
    }
}

impl Render for RailPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let output = self.output(cx);
        let niri = self.ctx.niri.read(cx);
        let workspaces = output.as_deref().map(|o| rail_workspaces(niri, o)).unwrap_or_default();
        let previews: Vec<Preview> = if t.rail == RailVariant::Thumb {
            workspaces.iter().map(|w| if w.is_fullscreen() { Preview::Strip(Strip::default()) } else { Preview::of(niri, w.id) }).collect()
        } else {
            Vec::new()
        };
        let trailing = output.as_deref().and_then(|o| trailing_empty(niri, o));
        let hovered = self.hover.read(cx).shown.map(|(id, _)| id);
        let (s2, muted) = (t.surface_2, t.fg_muted);
        let apps = self.ctx.services.apps.state.read(cx).apps.clone();
        let boxes = geom::entry_boxes(t.rail, &workspaces.iter().map(RailWorkspace::is_fullscreen).collect::<Vec<_>>());

        // The active marker (num rail) glides to the active entry's slot (and size: a fullscreen
        // entry is shorter than a desk button).
        let marker_target = (t.rail == RailVariant::Num)
            .then(|| workspaces.iter().position(|w| w.active))
            .flatten()
            .map(|ix| {
                let (top, h) = boxes[ix];
                let inset = if workspaces[ix].is_fullscreen() { geom::FS_BAR_INSET } else { geom::ACTIVE_BAR_INSET };
                (top + inset, h - 2.0 * inset)
            });
        let marker = match (marker_target, self.state.marker.as_mut(), self.state.marker_h.as_mut()) {
            (Some((y, h)), Some(m), Some(mh)) => {
                m.set_target(y);
                mh.set_target(h);
                let (at, moving) = m.drawn_now();
                let (hat, hmoving) = mh.drawn_now();
                if moving || hmoving {
                    window.request_animation_frame();
                }
                Some((at, hat))
            }
            (Some((y, h)), _, _) => {
                self.state.marker = Some(SpringValue::new(y, SPRING_WORKSPACE));
                self.state.marker_h = Some(SpringValue::new(h, SPRING_WORKSPACE));
                Some((y, h))
            }
            (None, _, _) => None,
        };
        let buttons = workspaces.into_iter().enumerate().map(|(index, ws)| {
            let id = ws.id;
            let top = boxes[index].0;
            let lit = ws.active || hovered == Some(id);
            let held = hovered == Some(id) && !ws.active;
            let (gap, content) = match (&ws.kind, t.rail) {
                (RailKind::Fullscreen { app_id, .. }, _) => {
                    let app_id = app_id.clone().unwrap_or_default();
                    let app = app_for_app_id(&apps, &app_id);
                    let path = app.and_then(|a| a.icon_path.clone());
                    (0.0, Self::fullscreen_content(&ws, path, app_glyph(app, &app_id), lit, &t))
                }
                (_, RailVariant::Num) => (0.0, Self::num_content(&ws, lit, &t)),
                (_, RailVariant::Thumb) => (geom::THUMB_LABEL_GAP, Self::thumb_content(&ws, &previews[index], &t)),
                (_, RailVariant::Pill) => (geom::PILL_LABEL_GAP, Self::pill_content(&ws, &t)),
            };
            div()
                .id(("rail-ws", id))
                .relative()
                .w_full()
                .h(px(boxes[index].1))
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(gap))
                .cursor_pointer()
                .text_color(if lit { t.fg } else { t.fg_muted })
                // Lit while its flyout is open (the pointer may be in the flyout): a plate
                // that fades with the flyout, under the hover wash.
                .child(
                    follow(
                        gpui::ElementId::NamedInteger("rail-plate".into(), id),
                        if held { 1.0 } else { 0.0 },
                        Timing::new(timing::SUBMENU_IN, EASE_STANDARD),
                        move |p| div().absolute().top_0().left_0().size_full().bg(mix(gpui::transparent_black(), s2, p)),
                    )
                    .fade(),
                )
                .child(hover_wash(s2, None, px(0.)))
                .children(content)
                .on_hover(cx.listener(move |this, inside: &bool, _, cx| {
                    let inside = *inside;
                    this.hover.update(cx, |h, cx| if inside { h.enter_button(id, top, cx) } else { h.leave_rail(cx) });
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
            .child(hover_wash(s2, None, t.radius_control))
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
            .children(marker.map(|(y, h)| div().absolute().left_0().top(px(y)).w(px(geom::ACTIVE_BAR_W)).h(px(h.max(0.0))).bg(t.accent)))
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
            fullscreen_window_id: None,
            fullscreen_origin_id: None,
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

    fn fs(id: u64, origin: u64, window: Option<u64>, active: bool) -> Workspace {
        let mut w = ws(id, 0, active);
        w.fullscreen_origin_id = Some(origin);
        w.fullscreen_window_id = window;
        w
    }

    fn app_win(id: u64, ws: u64, app: &str, title: &str) -> Window {
        let mut w = win(id, ws);
        w.app_id = Some(app.into());
        w.title = Some(title.into());
        w
    }

    /// Desks 1, 2, 3 (ids 1..3, 3 empty and trailing), a fullscreen workspace (id 10) from desk 2
    /// holding a video player.
    fn with_fullscreen() -> NiriState {
        let mut n = NiriState::default();
        n.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, false), ws(2, 2, false), fs(10, 2, Some(20), true), ws(3, 3, false)] });
        n.apply(Event::WindowsChanged {
            windows: vec![win(11, 1), win(12, 2), app_win(20, 10, "mpv", "movie.mkv"), win(13, 2)],
        });
        n
    }

    #[test]
    fn a_fullscreen_workspace_is_an_app_entry_under_its_desk_and_the_numbers_do_not_move() {
        let n = with_fullscreen();
        let v = rail_workspaces(&n, "DP-1");
        // desks keep 1, 2; the entry follows desk 2 and carries its number, not a number of its own
        assert_eq!(v.iter().map(|w| (w.id, w.idx, w.is_fullscreen())).collect::<Vec<_>>(), vec![(1, 1, false), (2, 2, false), (10, 2, true), ]);
        assert_eq!(
            v[2].kind,
            RailKind::Fullscreen { origin: 2, app_id: Some("mpv".into()), title: Some("movie.mkv".into()) }
        );
        assert!(v[2].active && v[2].windows == 1);
        // the trailing empty desk is still the "+" target and still not listed
        assert_eq!(trailing_empty(&n, "DP-1"), Some(3));
        // the wheel passes through it: desk 2 -> fullscreen entry -> (trailing desk 3)
        assert_eq!(neighbour(&n, "DP-1", -1), Some(2));
        assert_eq!(neighbour(&n, "DP-1", 1), Some(3));
    }

    #[test]
    fn two_fullscreen_workspaces_from_one_desk_the_newest_nearest() {
        let mut n = with_fullscreen();
        // workspace 11 is newer than 10: it stands right under desk 2
        n.apply(Event::WorkspacesChanged {
            workspaces: vec![ws(1, 1, false), ws(2, 2, false), fs(10, 2, Some(20), false), fs(11, 2, Some(21), true), ws(3, 3, false)],
        });
        n.apply(Event::WindowOpenedOrChanged { window: app_win(21, 11, "firefox", "video") });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.id, w.idx)).collect::<Vec<_>>(), vec![(1, 1), (2, 2), (11, 2), (10, 2)]);
        assert!(v[2].active && !v[3].active);
        assert_eq!(trailing_empty(&n, "DP-1"), Some(3));
    }

    #[test]
    fn a_fullscreen_workspace_whose_window_just_left_is_still_listed_without_an_app() {
        let mut n = with_fullscreen();
        // the window left: fullscreen_window_id is None and the workspace holds nothing any more
        n.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, false), ws(2, 2, false), fs(10, 2, None, true), ws(3, 3, false)] });
        n.apply(Event::WindowClosed { id: 20 });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.id, w.idx, w.windows)).collect::<Vec<_>>(), vec![(1, 1, 1), (2, 2, 2), (10, 2, 0)]);
        assert_eq!(v[2].kind, RailKind::Fullscreen { origin: 2, app_id: None, title: None });
    }

    #[test]
    fn a_desk_that_only_exists_as_an_origin_is_not_the_trailing_empty_one() {
        let mut n = NiriState::default();
        // desk 1 with nothing on it but a fullscreen window's origin; desk 2 is niri's trailing empty one
        n.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, false), fs(10, 1, Some(20), true), ws(2, 2, false)] });
        n.apply(Event::WindowsChanged { windows: vec![app_win(20, 10, "mpv", "m")] });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.id, w.idx)).collect::<Vec<_>>(), vec![(1, 1), (10, 1)]);
        assert_eq!(trailing_empty(&n, "DP-1"), Some(2));
        // and when the trailing desk is itself the origin it stays a button and is not offered as "+"
        let mut m = NiriState::default();
        m.apply(Event::WorkspacesChanged { workspaces: vec![ws(1, 1, false), ws(2, 2, false), fs(10, 2, Some(20), true)] });
        m.apply(Event::WindowsChanged { windows: vec![win(11, 1), app_win(20, 10, "mpv", "m")] });
        assert_eq!(rail_workspaces(&m, "DP-1").iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2, 10]);
        assert_eq!(trailing_empty(&m, "DP-1"), None);
    }

    #[test]
    fn workspaces_made_by_index_may_leave_empty_ones_in_the_middle() {
        let mut n = NiriState::default();
        // desks 1, 2 (empty), 3 (empty), 4 and the trailing empty 5: the middle ones are listed
        n.apply(Event::WorkspacesChanged {
            workspaces: vec![ws(1, 1, true), ws(7, 2, false), ws(8, 3, false), ws(4, 4, false), ws(5, 5, false)],
        });
        n.apply(Event::WindowsChanged { windows: vec![win(10, 1), win(11, 4)] });
        let v = rail_workspaces(&n, "DP-1");
        assert_eq!(v.iter().map(|w| (w.id, w.idx, w.windows)).collect::<Vec<_>>(), vec![(1, 1, 1), (7, 2, 0), (8, 3, 0), (4, 4, 1)]);
        assert_eq!(trailing_empty(&n, "DP-1"), Some(5));
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
