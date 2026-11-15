//! Canvas minimap (desk-05 canvas branch, `right 14px; bottom 14px; width 260px`): a card
//! at the bottom-right of the main area (above the bar, left of a shown scratchpad) while
//! the display's active workspace is a canvas.
//!
//! - Header: "minimap" (click: collapse to a chip; the state is kept in the ashd state
//!   dir) and "N windows · M regions".
//! - Map 236×143: the canvas bounds (windows ∪ regions ∪ viewport, padded) scaled to fit,
//!   keeping the aspect. Windows: `line` rects at .9 (focused: accent; pinned: a small
//!   marker; windows outside the viewport: outlines only). Regions: dashed rects with tiny
//!   labels. Viewport: 1px `fg` rect, following the live `CanvasViewChanged` events.
//! - Map input: click empty map = `canvas-pan-to` centred there; click a window =
//!   `canvas-fly-to-window`; drag the viewport rect = continuous pan (one `canvas-pan-to`
//!   per frame); wheel = `canvas-zoom-in/out`.
//! - Footer: − / % / + / fit (`canvas-zoom-out`, `canvas-set-zoom 1.0`, `canvas-zoom-in`,
//!   `canvas-fit`), and the plane coordinates of the viewport centre.
//!
//! Every interactive part is inside `hit_area` (the card); the rest of the output stays
//! click-through. Re-renders only for its own workspace's view/windows.
//!
//! Idle hide ([`visibility`]): the card is shown while the view changes (pan, zoom, a window
//! moved, resized, opened or closed, a switch to the workspace) and for 1.5 s afterwards,
//! then fades out over 250 ms; a new change fades it back in over 120 ms. It stays while the
//! pointer is over it or a drag runs. Once (nearly) transparent it leaves the input region,
//! and once hidden nothing renders until the next change.
//!
//! Owner: canvas UI.

pub mod visibility;

use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Global, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollDelta,
    ScrollEvent, StatefulInteractiveElement as _, Styled as _, Task, Window, canvas, div, prelude::*, px,
};
use ashd_niri::{
    CanvasWindow, LiveViews, PlaneRect,
    canvas::{offset_centering, visible_plane_rect},
    niri_ipc::{Action, CanvasRegion, CanvasViewState},
};
use ashd_panel_canvas_hud::common::{self, Hover, ModeFade, SWAP_TRAVEL};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, hit_area};
use ashd_ui::{Icon, alpha, hover_wash, swap, theme};
use visibility::{Timings, Visibility};

/// Card offset from the main area's bottom-right corner and width (desk-05).
pub const CARD_RIGHT: f32 = 14.0;
pub const CARD_BOTTOM: f32 = 14.0;
pub const CARD_W: f32 = 260.0;
const CARD_PAD: f32 = 12.0;
const CARD_GAP: f32 = 10.0;
/// Map content box (desk-05 `236×143`, plus a 1px border outside).
pub const MAP_W: f32 = 236.0;
pub const MAP_H: f32 = 143.0;
/// Footer buttons (desk-05 `28×28`, gap 6, percent label 52 wide).
const BUTTON: f32 = 28.0;
const FOOT_GAP: f32 = 6.0;
const PCT_W: f32 = 52.0;
const PCT_SIZE: f32 = 12.0;
const COORD_SIZE: f32 = 10.0;
const REGION_LABEL_SIZE: f32 = 8.0;
/// Padding around the canvas bounds, as a share of their larger side.
const PAD_SHARE: f64 = 0.06;
/// Wheel: pixels of smooth scrolling per zoom step, and the shortest time between steps.
const WHEEL_PX_PER_STEP: f32 = 40.0;
const WHEEL_COOLDOWN: Duration = Duration::from_millis(120);
const PIN_MARK: f32 = 4.0;

/// Plane → map transform: `map = (plane - origin) * scale + shift`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapXform {
    pub origin: (f64, f64),
    pub scale: f64,
    pub shift: (f64, f64),
}

impl MapXform {
    /// Fit `bounds` (padded) into `w × h`, keeping the aspect, centred.
    pub fn fit(bounds: PlaneRect, w: f64, h: f64) -> Self {
        let pad = bounds.w.max(bounds.h) * PAD_SHARE;
        let b = bounds.padded(pad.max(1.0));
        let scale = (w / b.w).min(h / b.h);
        let shift = ((w - b.w * scale) / 2.0, (h - b.h * scale) / 2.0);
        Self { origin: (b.x, b.y), scale, shift }
    }

    pub fn to_map(&self, p: (f64, f64)) -> (f64, f64) {
        ((p.0 - self.origin.0) * self.scale + self.shift.0, (p.1 - self.origin.1) * self.scale + self.shift.1)
    }

    pub fn to_plane(&self, m: (f64, f64)) -> (f64, f64) {
        ((m.0 - self.shift.0) / self.scale + self.origin.0, (m.1 - self.shift.1) / self.scale + self.origin.1)
    }

    pub fn rect(&self, r: &PlaneRect) -> PlaneRect {
        let (x, y) = self.to_map((r.x, r.y));
        PlaneRect::new(x, y, r.w * self.scale, r.h * self.scale)
    }
}

/// The transform for a canvas: windows ∪ regions ∪ the visible view.
pub fn map_xform(windows: &[CanvasWindow], view: Option<&CanvasViewState>) -> MapXform {
    let regions: &[CanvasRegion] = view.map(|v| v.regions.as_slice()).unwrap_or_default();
    let mut b = ashd_niri::canvas::bounds(windows, regions);
    if let Some(v) = view {
        let r = visible_plane_rect(v);
        b = Some(b.map_or(r, |b| b.union(&r)));
    }
    MapXform::fit(b.unwrap_or(PlaneRect::new(0.0, 0.0, 1920.0, 1080.0)), MAP_W as f64, MAP_H as f64)
}

/// What a press at map point `m` does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MapHit {
    /// Inside the viewport rect: start dragging it.
    Viewport,
    /// On a window (topmost wins; the focused one is drawn last).
    Window(u64),
    /// Empty map: centre the view on this plane point.
    Plane(f64, f64),
}

pub fn hit_test(m: (f64, f64), xf: &MapXform, windows: &[CanvasWindow], view: Option<&CanvasViewState>) -> MapHit {
    if let Some(v) = view
        && xf.rect(&visible_plane_rect(v)).contains(m)
    {
        // Windows inside the viewport still win: clicking one flies to it.
        if let Some(w) = draw_order(windows).iter().rev().find(|w| xf.rect(&w.rect).contains(m)) {
            return MapHit::Window(w.id);
        }
        return MapHit::Viewport;
    }
    if let Some(w) = draw_order(windows).iter().rev().find(|w| xf.rect(&w.rect).contains(m)) {
        return MapHit::Window(w.id);
    }
    let p = xf.to_plane(m);
    MapHit::Plane(p.0, p.1)
}

/// Windows in drawing order: by id, the focused one last (on top).
fn draw_order(windows: &[CanvasWindow]) -> Vec<&CanvasWindow> {
    let mut v: Vec<_> = windows.iter().collect();
    v.sort_by_key(|w| (w.focused, w.id));
    v
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ── collapsed state (shared by every display, persisted) ──────────────────────

struct CollapsedGlobal(Entity<bool>);

impl Global for CollapsedGlobal {}

fn collapsed_path() -> PathBuf {
    ashd_tokens::state::state_dir().join("minimap-collapsed")
}

fn collapsed_entity(cx: &mut App) -> Entity<bool> {
    if let Some(g) = cx.try_global::<CollapsedGlobal>() {
        return g.0.clone();
    }
    let entity = cx.new(|_| false);
    cx.set_global(CollapsedGlobal(entity.clone()));
    let task = cx.background_spawn(async move { std::fs::read_to_string(collapsed_path()).ok() });
    let weak = entity.downgrade();
    cx.spawn(async move |cx| {
        if let Some(text) = task.await {
            let on = text.trim() == "1";
            let _ = weak.update(cx, |c, cx| {
                if *c != on {
                    *c = on;
                    cx.notify();
                }
            });
        }
    })
    .detach();
    entity
}

fn set_collapsed(entity: &Entity<bool>, on: bool, cx: &mut App) {
    entity.update(cx, |c, cx| {
        *c = on;
        cx.notify();
    });
    cx.background_spawn(async move {
        let path = collapsed_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = ashd_tokens::state::write_atomic(&path, if on { b"1\n" } else { b"0\n" }) {
            tracing::warn!("minimap: cannot persist the collapsed state: {e}");
        }
    })
    .detach();
}

// ── panel ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct MiniModel {
    workspace: u64,
    windows: Vec<CanvasWindow>,
}

struct Drag {
    start_mouse: Point<Pixels>,
    start_offset: (f64, f64),
    xform: MapXform,
    target: (f64, f64),
    /// A `canvas-pan-to` is scheduled for the next frame.
    scheduled: bool,
    /// The target moved since the last action.
    dirty: bool,
}

pub struct MinimapPanel {
    pub ctx: PanelContext,
    views: Entity<Arc<LiveViews>>,
    collapsed: Entity<bool>,
    model: Option<MiniModel>,
    view: Option<CanvasViewState>,
    hover: Hover,
    drag: Option<Drag>,
    /// Map content bounds (window coords) at the last paint.
    map_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    wheel_acc: f32,
    last_wheel: Option<Instant>,
    /// Idle-hide state machine (see [`visibility`]).
    vis: Visibility,
    /// The pointer is over the card (any part of it, not just a control).
    card_hovered: bool,
    /// The scratchpad visibility the card was last laid out for.
    scratch_seen: bool,
    /// The one pending wake-up for the end of the hold: when, and the timer.
    wake: Option<(Instant, Task<()>)>,
    /// The card comes and goes with the mode (Scroll <-> Canvas, 280 ms out-cubic); the last
    /// model and view are kept while it leaves.
    fade: ModeFade<(MiniModel, Option<CanvasViewState>)>,
}

fn hover_of(this: &mut MinimapPanel) -> &mut Hover {
    &mut this.hover
}

impl MinimapPanel {
    fn compute(&self, cx: &App) -> Option<MiniModel> {
        let t = theme(cx);
        let d = common::display_ctx(&self.ctx, t, cx)?;
        let niri = self.ctx.niri.read(cx);
        if common::suppressed(niri, d.output.as_deref()) {
            return None;
        }
        let ws = common::active_workspace(niri, d.output.as_deref())?;
        if !niri.is_canvas(ws.id) {
            return None;
        }
        Some(MiniModel { workspace: ws.id, windows: niri.canvas_windows(ws.id) })
    }

    fn compute_view(&self, cx: &App) -> Option<CanvasViewState> {
        let ws = self.model.as_ref()?.workspace;
        self.views.read(cx).canvas(ws).cloned().or_else(|| self.ctx.niri.read(cx).canvas_view(ws).cloned())
    }

    /// Tell the mode fade what is shown now.
    fn sync_fade(&mut self) {
        let cur = self.model.clone().map(|m| (m, self.view.clone()));
        self.fade.sync(cur.as_ref());
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = self.compute(cx);
        let flips = next.is_some() != self.model.is_some();
        let changed = next != self.model;
        self.model = next;
        if self.model.is_none() {
            self.drag = None;
            self.card_hovered = false;
            self.vis.set_held(false, Instant::now());
            self.vis.set_hovered(false, Instant::now());
        }
        let view = self.compute_view(cx);
        if changed || view != self.view {
            self.view = view;
            if self.model.is_some() {
                self.vis.poke(Instant::now());
            }
            self.sync_fade();
            cx.notify();
        }
        if flips {
            self.ctx.shell.refresh_overlays(cx);
        }
    }

    fn refresh_view(&mut self, cx: &mut Context<Self>) {
        let view = self.compute_view(cx);
        if view != self.view {
            self.view = view;
            if self.model.is_some() {
                self.vis.poke(Instant::now());
            }
            self.sync_fade();
            cx.notify();
        }
    }

    /// The pointer entered / left the card: it stays shown while hovered.
    fn set_card_hovered(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.card_hovered != on {
            self.card_hovered = on;
            self.vis.set_hovered(on, Instant::now());
            cx.notify();
        }
    }

    /// Keep exactly one timer for the end of the hold (`None`: nothing to wake for).
    fn arm_wake(&mut self, at: Option<Instant>, cx: &mut Context<Self>) {
        match (at, self.wake.as_ref().map(|w| w.0)) {
            (None, _) => self.wake = None,
            (Some(a), Some(b)) if a == b => {}
            (Some(a), _) => {
                let delay = a.saturating_duration_since(Instant::now());
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    // Clear first: a wake-up that comes early re-arms from the next render.
                    let _ = this.update(cx, |this, cx| {
                        this.wake = None;
                        cx.notify();
                    });
                });
                self.wake = Some((a, task));
            }
        }
    }

    /// Run canvas actions on this display's workspace.
    fn act(&self, actions: Vec<Action>, cx: &mut App) {
        let Some(ws) = self.model.as_ref().map(|m| m.workspace) else { return };
        let focus = !self.ctx.niri.read(cx).workspaces.get(&ws).is_some_and(|w| w.is_focused);
        common::run_actions(&self.ctx.niri_handle, focus.then_some(ws), actions, cx);
    }

    fn xform(&self) -> Option<MapXform> {
        let m = self.model.as_ref()?;
        Some(self.drag.as_ref().map(|d| d.xform).unwrap_or_else(|| map_xform(&m.windows, self.view.as_ref())))
    }

    fn on_map_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let (Some(b), Some(xf), Some(m)) = (self.map_bounds.get(), self.xform(), self.model.as_ref()) else { return };
        let local = (f64::from(f32::from(ev.position.x - b.origin.x)), f64::from(f32::from(ev.position.y - b.origin.y)));
        match hit_test(local, &xf, &m.windows, self.view.as_ref()) {
            MapHit::Window(id) => self.act(vec![Action::CanvasFlyToWindow { id: Some(id) }], cx),
            MapHit::Plane(x, y) => {
                if let Some(v) = &self.view {
                    let (ox, oy) = offset_centering(v, (x, y));
                    self.act(vec![Action::CanvasPanTo { x: ox, y: oy }], cx);
                }
            }
            MapHit::Viewport => {
                if let Some(v) = &self.view {
                    self.vis.set_held(true, Instant::now());
                    self.drag = Some(Drag {
                        start_mouse: ev.position,
                        start_offset: v.target_offset,
                        xform: xf,
                        target: v.target_offset,
                        scheduled: false,
                        dirty: false,
                    });
                    cx.notify();
                }
            }
        }
    }

    fn on_drag_move(&mut self, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.as_mut() else { return };
        let dx = f64::from(f32::from(pos.x - d.start_mouse.x)) / d.xform.scale;
        let dy = f64::from(f32::from(pos.y - d.start_mouse.y)) / d.xform.scale;
        d.target = (d.start_offset.0 + dx, d.start_offset.1 + dy);
        d.dirty = true;
        if !d.scheduled {
            d.scheduled = true;
            // One action per frame however fast the pointer reports.
            cx.on_next_frame(window, |this, _, cx| this.flush_drag(cx));
        }
    }

    fn flush_drag(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.drag.as_mut() else { return };
        d.scheduled = false;
        if !d.dirty {
            return;
        }
        d.dirty = false;
        let (x, y) = d.target;
        self.act(vec![Action::CanvasPanTo { x, y }], cx);
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.as_ref().is_some_and(|d| d.dirty) {
            if let Some(d) = self.drag.as_mut() {
                d.scheduled = false;
            }
            self.flush_drag(cx);
        }
        self.drag = None;
        self.vis.set_held(false, Instant::now());
        cx.notify();
    }

    fn on_wheel(&mut self, ev: &ScrollEvent, cx: &mut Context<Self>) {
        let dy = match ev.delta {
            ScrollDelta::Lines(p) => p.y * WHEEL_PX_PER_STEP,
            ScrollDelta::Pixels(p) => f32::from(p.y),
        };
        if dy * self.wheel_acc < 0.0 {
            self.wheel_acc = 0.0;
        }
        self.wheel_acc += dy;
        if self.wheel_acc.abs() < WHEEL_PX_PER_STEP {
            return;
        }
        let up = self.wheel_acc > 0.0;
        self.wheel_acc = 0.0;
        let now = Instant::now();
        if self.last_wheel.is_some_and(|t| now.duration_since(t) < WHEEL_COOLDOWN) {
            return;
        }
        self.last_wheel = Some(now);
        // Wheel up (positive y) zooms in, like the canvas binds.
        let action = if up { Action::CanvasZoomIn { at_cursor: false } } else { Action::CanvasZoomOut { at_cursor: false } };
        self.act(vec![action], cx);
    }
}

impl Panel for MinimapPanel {
    const ID: PanelId = PanelId("minimap");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let views = common::live_views(&ctx.niri_handle, cx);
        let collapsed = collapsed_entity(cx);
        cx.observe(&ctx.niri, |this, _, cx| this.refresh(cx)).detach();
        cx.observe(&views, |this, _, cx| this.refresh_view(cx)).detach();
        cx.observe(&collapsed, |_, _, cx| cx.notify()).detach();
        if let Some(shell) = ctx.shell.entity() {
            // Scratchpad shown/hidden moves the card; any other shell change is not ours.
            cx.observe(&shell, |this, shell, cx| {
                this.refresh(cx);
                let shown = shell.read(cx).scratch_visible;
                if std::mem::replace(&mut this.scratch_seen, shown) != shown {
                    cx.notify();
                }
            })
            .detach();
        }
        let mut this = Self {
            ctx,
            views,
            collapsed,
            model: None,
            view: None,
            hover: Hover::default(),
            drag: None,
            map_bounds: Rc::default(),
            wheel_acc: 0.0,
            last_wheel: None,
            vis: Visibility::new(Timings::current(), Instant::now()),
            card_hovered: false,
            scratch_seen: false,
            wake: None,
            fade: ModeFade::new(None),
        };
        this.scratch_seen = this.ctx.shell.entity().is_some_and(|s| s.read(cx).scratch_visible);
        this.model = this.compute(cx);
        this.view = this.compute_view(cx);
        if this.model.is_some() {
            this.vis.poke(Instant::now());
        }
        this.fade = ModeFade::new(this.model.clone().map(|m| (m, this.view.clone())));
        this
    }

    fn is_live(&self, _cx: &App) -> bool {
        self.fade.is_live()
    }
}

/// The card's input region: while it is (nearly) transparent it is left out, so the
/// invisible minimap never eats a click.
fn input_area(el: impl IntoElement, interactive: bool) -> gpui::Div {
    if interactive { hit_area(el) } else { div().relative().child(el) }
}

/// Clears the card hover when the pointer leaves the surface (no `on_hover(false)` reaches
/// the card then, GPUI-NOTES G8).
fn card_exit_reset(cx: &mut Context<MinimapPanel>) -> impl IntoElement {
    let this = cx.entity().downgrade();
    canvas(
        |_, _, _| {},
        move |_, _, window: &mut Window, _| {
            let this = this.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, _, _, cx| {
                let _ = this.update(cx, |t, cx| t.set_card_hovered(false, cx));
            });
        },
    )
    .absolute()
    .size_0()
}

impl Render for MinimapPanel {
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
        let Some(common::Shown { content: (m, shown_view), fade, travel, leaving }) = shown else {
            self.wake = None;
            return root;
        };
        let t = theme(cx).clone();
        let Some(d) = common::display_ctx(&self.ctx, &t, cx) else { return root };

        // Idle hide: frames only while the opacity moves, one timer while it holds, and
        // nothing at all once it is hidden.
        self.vis.set_timings(Timings::current());
        let frame = self.vis.frame(Instant::now());
        if frame.animating {
            window.request_animation_frame();
        }
        self.arm_wake(frame.wake_at, cx);
        if !frame.drawn() {
            return root;
        }
        let interactive = frame.interactive() && !leaving;
        let view = shown_view;
        let collapsed = *self.collapsed.read(cx);
        let zoom_pct = view.as_ref().map(|v| format!("{}%", (v.zoom * 100.0).round() as i64)).unwrap_or_else(|| "—".into());
        let right = d.area.origin.x + d.area.size.width - px(CARD_RIGHT);
        let bottom = d.area.origin.y + d.area.size.height - px(CARD_BOTTOM);
        let place = |el: gpui::Div| {
            div()
                .absolute()
                .left(right - px(CARD_W))
                .top(px(SWAP_TRAVEL * (1.0 - travel)))
                .w(px(CARD_W))
                .h(bottom)
                .flex()
                .flex_col()
                .justify_end()
                .opacity(frame.opacity * fade)
                // chip <-> card: the card dips and comes back when it is (un)collapsed
                .child(swap("minimap-body", collapsed, move |p| div().opacity(p).child(el)))
        };

        if collapsed {
            let chip = common::track_hover(
                common::card(&t, t.radius_control)
                    .id("minimap-expand")
                    .flex()
                    .items_center()
                    .gap(px(CARD_GAP))
                    .px(px(10.))
                    .py(px(6.))
                    .cursor_pointer()
                    .child(hover_wash(t.surface_2, None, t.radius_control))
                    .child(common::label("minimap", t.fg, &t))
                    .child(common::label(&zoom_pct, t.fg_muted, &t)),
                "expand",
                hover_of,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| set_collapsed(&this.collapsed, false, cx)));
            let chip = div()
                .flex()
                .justify_end()
                .child(input_area(chip, interactive).id("minimap-chip-area").on_hover(cx.listener(|this, inside: &bool, _, cx| this.set_card_hovered(*inside, cx))))
                .child(common::hover_reset(hover_of, cx))
                .child(card_exit_reset(cx));
            return root.child(place(chip));
        }

        let regions: Vec<CanvasRegion> = view.as_ref().map(|v| v.regions.clone()).unwrap_or_default();
        let xf = self.xform().unwrap_or_else(|| map_xform(&m.windows, view.as_ref()));
        let visible = view.as_ref().map(visible_plane_rect);

        // ── map content ──
        let rect_el = |r: PlaneRect| {
            div()
                .absolute()
                .left(px(r.x as f32))
                .top(px(r.y as f32))
                .w(px((r.w as f32).max(2.0)))
                .h(px((r.h as f32).max(2.0)))
        };
        let region_els = regions.iter().map(|r| {
            let mr = xf.rect(&PlaneRect::new(r.x, r.y, r.w, r.h));
            rect_el(mr)
                .border_1()
                .border_dashed()
                .border_color(t.line)
                .overflow_hidden()
                .child(
                    div()
                        .pl(px(2.))
                        .font_family(t.font_mono.clone())
                        .text_size(px(REGION_LABEL_SIZE))
                        .line_height(px(REGION_LABEL_SIZE + 2.0))
                        .text_color(t.fg_muted)
                        .whitespace_nowrap()
                        .child(t.label_text(&r.name)),
                )
        });
        let window_els = draw_order(&m.windows)
            .into_iter()
            .map(|w| {
                let mr = xf.rect(&w.rect);
                let on_screen = visible.as_ref().is_none_or(|v| v.intersects(&w.rect));
                let el = rect_el(mr);
                let el = if w.focused {
                    el.bg(t.accent)
                } else if on_screen {
                    el.bg(alpha(t.line, 0.9))
                } else {
                    el.border_1().border_color(alpha(t.fg_muted, 0.6))
                };
                el.when(w.pinned, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .right(px(1.))
                            .size(px(PIN_MARK))
                            .bg(if w.focused { t.on_accent } else { t.fg }),
                    )
                })
            })
            .collect::<Vec<_>>();
        let viewport_el = visible.map(|v| {
            let mr = xf.rect(&v);
            rect_el(mr).border_1().border_color(t.fg)
        });
        let dragging = self.drag.is_some();
        let bounds_cell = self.map_bounds.clone();
        let this = cx.entity().downgrade();
        let recorder = canvas(
            move |bounds, _, _| bounds_cell.set(Some(bounds)),
            move |_, _, window, _| {
                if !dragging {
                    return;
                }
                // While dragging, follow the pointer anywhere on the surface (the press
                // keeps an implicit grab), and end on release wherever it happens.
                let mv = this.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase == gpui::DispatchPhase::Bubble {
                        let _ = mv.update(cx, |t, cx| t.on_drag_move(e.position, window, cx));
                    }
                });
                let up = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Bubble {
                        let _ = up.update(cx, |t, cx| t.end_drag(cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let map = div()
            .id("minimap-map")
            .relative()
            .flex_none()
            .w(px(MAP_W + 2.0))
            .h(px(MAP_H + 2.0))
            .bg(t.bg)
            .border_1()
            .border_color(t.line)
            .overflow_hidden()
            .cursor_pointer()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(MAP_W))
                    .h(px(MAP_H))
                    .children(region_els)
                    .children(window_els)
                    .children(viewport_el)
                    .child(recorder),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| this.on_map_down(ev, cx)))
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, _, cx| this.on_wheel(ev, cx)));

        // ── header ──
        let n_regions = regions.len();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                common::track_hover(
                    div()
                        .id("minimap-collapse")
                        .cursor_pointer()
                        .child(common::label("minimap", if self.hover.is("collapse") { t.fg } else { t.fg_muted }, &t)),
                    "collapse",
                    hover_of,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| set_collapsed(&this.collapsed, true, cx))),
            )
            .child(common::label(
                format!("{} · {}", plural(m.windows.len(), "window", "windows"), plural(n_regions, "region", "regions")),
                t.fg_muted,
                &t,
            ));

        // ── footer ──
        let button = |id: &'static str, ic: Icon, hovered: bool| common::square_button(id, ic, BUTTON, hovered, &t);
        let zout = common::track_hover(button("minimap-zoom-out", Icon::Minus, self.hover.is("zout")), "zout", hover_of, cx)
            .on_click(cx.listener(|this, _, _, cx| this.act(vec![Action::CanvasZoomOut { at_cursor: false }], cx)));
        let zin = common::track_hover(button("minimap-zoom-in", Icon::Plus, self.hover.is("zin")), "zin", hover_of, cx)
            .on_click(cx.listener(|this, _, _, cx| this.act(vec![Action::CanvasZoomIn { at_cursor: false }], cx)));
        let fit = common::track_hover(button("minimap-fit", Icon::Fit, self.hover.is("fit")), "fit", hover_of, cx)
            .on_click(cx.listener(|this, _, _, cx| this.act(vec![Action::CanvasFit {}], cx)));
        let pct = common::track_hover(
            div()
                .id("minimap-zoom-reset")
                .flex_none()
                .w(px(PCT_W))
                .h(px(BUTTON))
                .flex()
                .items_center()
                .justify_center()
                .rounded(ashd_shell_core::frame::nested_radius(&t))
                .cursor_pointer()
                .child(hover_wash(t.surface_2, None, ashd_shell_core::frame::nested_radius(&t)))
                .font_family(t.font_mono.clone())
                .text_size(px(PCT_SIZE))
                .text_color(t.fg)
                .child(zoom_pct),
            "pct",
            hover_of,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| this.act(vec![Action::CanvasSetZoom { zoom: 1.0 }], cx)));
        let coords = visible
            .map(|v| {
                let (x, y) = v.center();
                format!("x {} y {}", x.round() as i64, y.round() as i64)
            })
            .unwrap_or_default();
        let footer = div()
            .flex()
            .items_center()
            .gap(px(FOOT_GAP))
            .child(zout)
            .child(pct)
            .child(zin)
            .child(fit)
            // The readout takes the rest of the row, right-aligned, clipped when long.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .justify_end()
                    .child(common::label_sized(coords, COORD_SIZE, t.fg_muted, &t)),
            );

        let card = common::card(&t, t.radius_panel)
            .w(px(CARD_W))
            .p(px(CARD_PAD))
            .flex()
            .flex_col()
            .gap(px(CARD_GAP))
            .child(header)
            .child(map)
            .child(footer)
            .child(common::hover_reset(hover_of, cx))
            .child(card_exit_reset(cx));
        let card = input_area(card, interactive)
            .id("minimap-card-area")
            .on_hover(cx.listener(|this, inside: &bool, _, cx| this.set_card_hovered(*inside, cx)));
        root.child(place(div().child(card)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u64, x: f64, y: f64, w: f64, h: f64, focused: bool) -> CanvasWindow {
        CanvasWindow { id, rect: PlaneRect::new(x, y, w, h), focused, pinned: false }
    }

    fn view(offset: (f64, f64), zoom: f64) -> CanvasViewState {
        CanvasViewState {
            workspace_id: 1,
            offset,
            zoom,
            target_offset: offset,
            target_zoom: zoom,
            view_size: (1920.0, 1080.0),
            animating: false,
            regions: vec![],
        }
    }

    #[test]
    fn transform_fits_and_round_trips() {
        let xf = MapXform::fit(PlaneRect::new(0.0, 0.0, 2800.0, 1700.0), 236.0, 143.0);
        let r = xf.rect(&PlaneRect::new(0.0, 0.0, 2800.0, 1700.0));
        assert!(r.x >= 0.0 && r.y >= 0.0 && r.right() <= 236.0 + 1e-6 && r.bottom() <= 143.0 + 1e-6);
        // The constraining side is padded on both ends; the other is centred.
        assert!((r.x - (236.0 - r.w) / 2.0).abs() < 1e-6 && (r.y - (143.0 - r.h) / 2.0).abs() < 1e-6);
        let p = (1234.0, -56.0);
        let back = xf.to_plane(xf.to_map(p));
        assert!((back.0 - p.0).abs() < 1e-9 && (back.1 - p.1).abs() < 1e-9);
    }

    #[test]
    fn xform_includes_the_viewport() {
        let windows = [win(1, 0.0, 0.0, 100.0, 100.0, false)];
        let v = view((5000.0, 0.0), 1.0);
        let xf = map_xform(&windows, Some(&v));
        let vr = xf.rect(&visible_plane_rect(&v));
        assert!(vr.right() <= MAP_W as f64 + 1e-6);
        assert_eq!(map_xform(&[], None), MapXform::fit(PlaneRect::new(0.0, 0.0, 1920.0, 1080.0), 236.0, 143.0));
    }

    #[test]
    fn presses_hit_windows_viewport_or_plane() {
        let windows = [win(1, 0.0, 0.0, 1000.0, 800.0, false), win(2, 500.0, 0.0, 400.0, 300.0, true), win(3, 3000.0, 2000.0, 400.0, 300.0, false)];
        let v = view((0.0, 0.0), 1.0); // shows 0..1920 × 0..1080
        let xf = map_xform(&windows, Some(&v));
        let at = |p: (f64, f64)| hit_test(xf.to_map(p), &xf, &windows, Some(&v));
        assert_eq!(at((600.0, 100.0)), MapHit::Window(2), "focused window is on top");
        assert_eq!(at((100.0, 100.0)), MapHit::Window(1));
        assert_eq!(at((1500.0, 900.0)), MapHit::Viewport);
        assert_eq!(at((3100.0, 2100.0)), MapHit::Window(3));
        match at((2500.0, 1500.0)) {
            MapHit::Plane(x, y) => assert!((x - 2500.0).abs() < 1e-6 && (y - 1500.0).abs() < 1e-6),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "window", "windows"), "1 window");
        assert_eq!(plural(0, "region", "regions"), "0 regions");
    }
}
