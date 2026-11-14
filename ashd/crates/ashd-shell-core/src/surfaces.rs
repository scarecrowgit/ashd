//! Surface registry: the layer-shell windows of every display and the root view that
//! lays out the panels in them.
//!
//! Per display: `bar` (created first: niri arranges exclusive zones in creation order, so
//! the full-width bar claims the bottom edge and the rail/scratchpad stop above it),
//! `rail`, `scratchpad` (while `Shell::scratch_visible`; in floating themes plus the
//! invisible `scratchpad-zone`), and on demand `overlay-passive` / `overlay-modal` (see
//! `overlay.rs`).
//!
//! Exclusive zones as niri (smithay) applies them: the reserved strip is
//! `exclusive_zone + margin` on the anchored edge, and surfaces created later are laid
//! out inside what is left. Floating geometry (desk-06 Ink, gap 10, 1920×1080):
//! - bar: zone `bar_h + gap`, margin gap on left/right/bottom → reserves `bar_h + 2·gap`
//!   (72): the main area ends `gap` above the bar (1008), the bar sits at 1018..1070.
//! - rail: margin gap top/left, **0 bottom** (the bar's zone already ends `gap` above the
//!   bar) → 10..1008 = 998; zone `rail_w + gap` + margin gap → main area from x = 112.
//! - scratchpad (floating): full height like the mockups (1570, 10, 340 × 1060): zone -1
//!   (ignores the bar's zone) with margin gap on top/right/bottom; the space it takes
//!   from windows is reserved by `scratchpad-zone`, a 1 px transparent surface with zone
//!   `SCRATCH_W + 2·gap` and an empty input region (main area right edge 1560). The bar
//!   draws its row only up to the main area's right edge while the scratchpad is shown.
//! - attached (Ash): no margins; the scratchpad (zone 340) stops above the bar (1034).

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{
    AnyView, App, AppContext as _, Bounds, Context, DisplayId, Entity, IntoElement, ParentElement as _,
    Pixels, Render, Size, Styled as _, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, canvas, div, layer_shell::*, point, prelude::*, px, size,
};
use ashd_tokens::{DockVariant, LayoutVariant, metrics};
use ashd_ui::{Theme, theme};

use crate::{
    Placement, Shell, Slot, frame,
    overlay::{self, HitState},
    panel::{PanelContext, PanelInstance},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SurfaceKind {
    Rail,
    Bar,
    Scratchpad,
    OverlayPassive,
    OverlayModal,
    /// Floating themes only: invisible 1 px surface whose exclusive zone keeps windows
    /// out from under the full-height scratchpad (which itself uses zone -1).
    ScratchpadZone,
}

impl SurfaceKind {
    pub const FRAME: [SurfaceKind; 3] = [Self::Bar, Self::Rail, Self::Scratchpad];

    /// Name used in `ashd status` and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rail => "rail",
            Self::Bar => "bar",
            Self::Scratchpad => "scratchpad",
            Self::OverlayPassive => "overlay-passive",
            Self::OverlayModal => "overlay-modal",
            Self::ScratchpadZone => "scratchpad-zone",
        }
    }

    /// layer-shell namespace (niri `layer-rule`s match on it).
    pub fn namespace(self) -> &'static str {
        match self {
            Self::Rail => "ashd-rail",
            Self::Bar => "ashd-bar",
            Self::Scratchpad => "ashd-scratchpad",
            Self::OverlayPassive => "ashd-overlay-passive",
            Self::OverlayModal => "ashd-overlay-modal",
            Self::ScratchpadZone => "ashd-scratchpad-zone",
        }
    }

    /// Every kind, in the order surfaces are closed (overlays first, bar last).
    pub(crate) const CLOSE_ORDER: [SurfaceKind; 6] = [
        Self::OverlayModal,
        Self::OverlayPassive,
        Self::ScratchpadZone,
        Self::Scratchpad,
        Self::Rail,
        Self::Bar,
    ];
}

/// Frame surfaces in creation order (= niri's exclusive-zone order) for `theme`.
pub(crate) fn frame_kinds(scratch: bool, theme: &Theme) -> Vec<SurfaceKind> {
    let mut kinds = vec![SurfaceKind::Bar, SurfaceKind::Rail];
    if scratch {
        kinds.extend(scratch_kinds(theme));
    }
    kinds
}

/// The scratchpad surface(s): the panel and, for floating themes, its zone keeper.
pub(crate) fn scratch_kinds(theme: &Theme) -> Vec<SurfaceKind> {
    if theme.layout == LayoutVariant::Floating {
        vec![SurfaceKind::Scratchpad, SurfaceKind::ScratchpadZone]
    } else {
        vec![SurfaceKind::Scratchpad]
    }
}

/// A GPUI display as the shell sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    pub id: DisplayId,
    /// niri output (connector) name, once niri told us (`DP-1`). Matched through GPUI's
    /// display uuid, which is `Uuid::new_v5(NAMESPACE_DNS, wl_output.name)`.
    pub output: Option<String>,
    /// Logical bounds of the output.
    pub bounds: Bounds<Pixels>,
}

impl DisplayInfo {
    pub fn output_label(&self) -> String {
        self.output.clone().unwrap_or_else(|| format!("display-{}", u64::from(self.id)))
    }

    pub fn size(&self) -> Size<Pixels> {
        self.bounds.size
    }

    /// A rect in bar-surface coordinates → output-local coordinates (for modal anchors).
    pub fn bar_to_output(&self, rect: Bounds<Pixels>, theme: &Theme) -> Bounds<Pixels> {
        let bar_top = self.bounds.size.height - theme.bar_h - self.floating_inset(theme);
        Bounds { origin: point(rect.origin.x + self.floating_inset(theme), rect.origin.y + bar_top), size: rect.size }
    }

    /// A rect in rail-surface coordinates → output-local coordinates.
    pub fn rail_to_output(&self, rect: Bounds<Pixels>, theme: &Theme) -> Bounds<Pixels> {
        let inset = self.floating_inset(theme);
        Bounds { origin: point(rect.origin.x + inset, rect.origin.y + inset), size: rect.size }
    }

    fn floating_inset(&self, theme: &Theme) -> Pixels {
        if theme.layout == LayoutVariant::Floating { theme.gap } else { px(0.) }
    }
}

/// `uuid::Uuid::new_v5(NAMESPACE_DNS, name)` — how GPUI derives a Wayland display uuid.
pub fn output_uuid(name: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes())
}

/// The parts of a theme's `Shape` baked into frame surfaces at creation (size, exclusive
/// zone, margins). A theme switch that changes any of them recreates the frame surfaces;
/// everything else (colours, fonts, radii) is a re-render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeKey {
    pub layout: LayoutVariant,
    pub gap: Pixels,
    pub rail_w: Pixels,
    pub bar_h: Pixels,
}

impl ShapeKey {
    pub fn of(theme: &Theme) -> Self {
        Self { layout: theme.layout, gap: theme.gap, rail_w: theme.rail_w, bar_h: theme.bar_h }
    }
}

/// The layer-shell options for a frame/overlay surface on `display` with `theme`.
/// Public so tests and the verification agent can assert geometry.
pub fn layer_options(kind: SurfaceKind, display: &DisplayInfo, theme: &Theme) -> WindowOptions {
    let floating = theme.layout == LayoutVariant::Floating;
    let gap = if floating { theme.gap } else { px(0.) };
    let full = Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
    let zero = px(0.);
    let scratch_w = px(metrics::SCRATCH_W);
    // Margins are (top, right, bottom, left). See the module docs for the zone arithmetic.
    let (layer, anchor, sz, zone, margin, keyboard) = match kind {
        SurfaceKind::Bar => (
            Layer::Top,
            Anchor::LEFT | Anchor::RIGHT | Anchor::BOTTOM,
            size(zero, theme.bar_h),
            Some(theme.bar_h + gap),
            (zero, gap, gap, gap),
            KeyboardInteractivity::None,
        ),
        SurfaceKind::Rail => (
            Layer::Top,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT,
            size(theme.rail_w, zero),
            Some(theme.rail_w + gap),
            (gap, zero, zero, gap),
            KeyboardInteractivity::None,
        ),
        SurfaceKind::Scratchpad if floating => (
            Layer::Top,
            Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT,
            size(scratch_w, zero),
            Some(px(-1.)),
            (gap, gap, gap, zero),
            KeyboardInteractivity::OnDemand,
        ),
        SurfaceKind::Scratchpad => (
            Layer::Top,
            Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT,
            size(scratch_w, zero),
            Some(scratch_w),
            (zero, zero, zero, zero),
            KeyboardInteractivity::OnDemand,
        ),
        SurfaceKind::ScratchpadZone => (
            Layer::Top,
            Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT,
            size(px(1.), zero),
            Some(scratch_w + gap * 2.),
            (zero, zero, zero, zero),
            KeyboardInteractivity::None,
        ),
        SurfaceKind::OverlayPassive => {
            (Layer::Overlay, full, size(zero, zero), Some(px(-1.)), (zero, zero, zero, zero), KeyboardInteractivity::None)
        }
        SurfaceKind::OverlayModal => (
            Layer::Overlay,
            full,
            size(zero, zero),
            Some(px(-1.)),
            (zero, zero, zero, zero),
            KeyboardInteractivity::Exclusive,
        ),
    };
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(zero, zero), size: sz })),
        app_id: Some("ashd-shell".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        display_id: Some(display.id),
        focus: matches!(kind, SurfaceKind::OverlayModal),
        // Layer surfaces never count as "active" windows; without this GPUI throttles
        // them to 30 fps. The shell animates at the display rate (180 Hz).
        inactive_frame_interval: None,
        low_power_frame_interval: None,
        thermal_frame_interval: None,
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: kind.namespace().into(),
            layer,
            anchor,
            exclusive_zone: zone,
            exclusive_edge: None,
            margin: Some(margin),
            keyboard_interactivity: keyboard,
        }),
        ..Default::default()
    }
}

/// Surfaces + panel instances of one display.
pub(crate) struct DisplaySurfaces {
    pub info: DisplayInfo,
    /// Frame and passive panel instances for this display.
    pub panels: Vec<PanelInstance>,
    pub windows: HashMap<SurfaceKind, WindowHandle<SurfaceRoot>>,
}

impl DisplaySurfaces {
    /// Sizes of the visible surfaces (`ashd status`; the zone keeper is not listed).
    pub fn surface_sizes(&self, cx: &App) -> Vec<(SurfaceKind, Size<Pixels>)> {
        let mut out: Vec<_> = self
            .windows
            .iter()
            .filter(|(k, _)| **k != SurfaceKind::ScratchpadZone)
            .filter_map(|(k, h)| h.read(cx).ok().map(|root| (*k, root.size)))
            .collect();
        out.sort_by_key(|(k, _)| k.name());
        out
    }

    fn slot_views(&self, slot: Slot) -> Vec<AnyView> {
        self.panels.iter().filter(|p| p.placement == Placement::Frame(slot)).map(|p| p.view.clone()).collect()
    }

    pub fn passive_views(&self) -> Vec<AnyView> {
        self.panels.iter().filter(|p| p.placement == Placement::Passive).map(|p| p.view.clone()).collect()
    }
}

/// Root view of every shell surface.
pub struct SurfaceRoot {
    pub kind: SurfaceKind,
    pub display: DisplayId,
    pub(crate) shell: WeakEntity<Shell>,
    /// Panel views shown in this surface: per slot for frame surfaces.
    pub(crate) slots: Vec<(Slot, Vec<AnyView>)>,
    pub(crate) hit: Rc<RefCell<HitState>>,
    /// Viewport size at the last render (reported by `ashd status`).
    pub size: Size<Pixels>,
    /// Bar only: the active window of this display is fullscreen → the bottom row is not
    /// drawn ("dock: hide on fullscreen"; niri also draws the top layer below fullscreen
    /// windows, so this mostly saves the dock's re-renders).
    pub(crate) fullscreen: bool,
    /// Modal overlay only: the root focus target (key-ups must reach the root).
    pub(crate) focus: Option<gpui::FocusHandle>,
    /// Modal overlay only: closed, kept until the keys pressed in it are released
    /// (`overlay.rs`, "key repeat across modal surfaces").
    pub(crate) lingering: bool,
}

impl SurfaceRoot {
    fn slot(&self, slot: Slot) -> Vec<AnyView> {
        self.slots.iter().find(|(s, _)| *s == slot).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    fn scratch_visible(&self, cx: &App) -> bool {
        self.shell.upgrade().is_some_and(|s| s.read(cx).scratch_visible)
    }

    /// The bottom row for the theme's `DockVariant` (see [`crate::frame`]).
    fn render_bar(&self, t: &Theme, cx: &App) -> gpui::AnyElement {
        if self.fullscreen {
            return div().size_full().into_any_element();
        }
        let floating = t.layout == LayoutVariant::Floating;
        let start = || self.slot(Slot::BarStart);
        let center = || self.slot(Slot::BarCenter).into_iter().map(|v| div().flex_none().h_full().child(v));
        let end = || self.slot(Slot::BarEnd).into_iter().map(|v| div().flex_none().h_full().child(v));
        let base = div().size_full().text_color(t.fg).font_family(t.font_sans.clone());
        match t.dock {
            // Ash: one bar, status block with a right border, dock centred on DOCK_CENTER_X.
            DockVariant::Bar => base
                .bg(t.surface)
                .border_color(t.frame)
                .when(floating, |d| d.border_1().rounded(t.radius_panel))
                .when(!floating, |d| d.border_t_1())
                .flex()
                .flex_row()
                .child(
                    div()
                        .flex_none()
                        .h_full()
                        .w(px(metrics::STATUS_W))
                        .border_r_1()
                        .border_color(t.line)
                        .flex()
                        .flex_row()
                        .children(start()),
                )
                .child(div().flex_1().min_w_0().h_full().relative().flex().flex_row().children(self.slot(Slot::BarCenter)))
                .child(div().flex_none().h_full().flex().flex_row().children(self.slot(Slot::BarEnd)))
                .into_any_element(),
            // Split / Pill: transparent surface; status pill, gap, then the row that ends
            // at the main area's right edge.
            DockVariant::Split | DockVariant::Pill => {
                let status = frame::frame_box(t).flex_none().h_full().w(px(metrics::STATUS_W)).flex().flex_row().children(start());
                let row = if t.dock == DockVariant::Split {
                    // The dock view spreads its two pills; the end view is the third pill.
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_row()
                        .gap(t.gap)
                        .children(self.slot(Slot::BarCenter).into_iter().map(|v| div().flex_1().min_w_0().h_full().child(v)))
                        .children(end())
                } else {
                    let sep = div().flex_none().w(px(1.)).h(px(frame::PILL_SEP_H)).bg(t.line);
                    div().flex_1().min_w_0().h_full().flex().justify_center().child(
                        frame::frame_box(t)
                            .rounded(frame::dock_pill_radius(t))
                            .flex_none()
                            .h_full()
                            .px(px(frame::DOCK_PILL_PAD_X))
                            .flex()
                            .items_center()
                            .gap(px(frame::PILL_GAP))
                            .children(center())
                            .child(sep)
                            .children(end()),
                    )
                };
                base.flex()
                    .flex_row()
                    .gap(t.gap)
                    .pr(frame::bar_row_right_pad(t, self.scratch_visible(cx)))
                    .child(status)
                    .child(row)
                    .into_any_element()
            }
        }
    }
}

impl Render for SurfaceRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.size = window.viewport_size();
        let t = theme(cx).clone();
        let floating = t.layout == LayoutVariant::Floating;
        let frame = |d: gpui::Div| {
            d.size_full()
                .bg(t.surface)
                .text_color(t.fg)
                .font_family(t.font_sans.clone())
                .border_color(t.frame)
                .when(floating, |d| d.border_1().rounded(t.radius_panel))
        };
        let el = match self.kind {
            SurfaceKind::Rail => frame(div()).when(!floating, |d| d.border_r_1()).flex().flex_col().children(self.slot(Slot::Rail)).into_any_element(),
            SurfaceKind::Scratchpad => frame(div())
                .when(!floating, |d| d.border_l_1())
                .flex()
                .flex_col()
                .children(self.slot(Slot::Scratchpad))
                .into_any_element(),
            SurfaceKind::Bar => self.render_bar(&t, cx),
            SurfaceKind::OverlayPassive => overlay::render_passive(self, window, cx).into_any_element(),
            SurfaceKind::OverlayModal => overlay::render_modal(self, window, cx).into_any_element(),
            SurfaceKind::ScratchpadZone => {
                // Nothing to draw, nothing to click: an empty input region, set once.
                let hit = self.hit.clone();
                div()
                    .size_full()
                    .child(
                        canvas(
                            move |_, window, _| {
                                let mut hit = hit.borrow_mut();
                                if hit.applied.is_none() {
                                    window.set_input_region(Some(&[]));
                                    hit.applied = Some(Vec::new());
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .size_0(),
                    )
                    .into_any_element()
            }
        };
        // Every surface: hover never outlives the pointer (see `pointer_exit_reset`).
        div().size_full().relative().child(el).child(overlay::pointer_exit_reset())
    }
}

// Window lifecycle. Rule (GPUI-NOTES G3): never open/close a window or build panels
// inside a `Shell` update — `open_window` renders the first frame synchronously and every
// panel may read the shell. These functions take the entity and do small, explicit updates.

/// Open one surface of `kind` on `display`.
pub(crate) fn open_surface(
    shell: &Entity<Shell>,
    display: DisplayId,
    kind: SurfaceKind,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<SurfaceRoot>> {
    let (opts, slots) = {
        let s = shell.read(cx);
        anyhow::ensure!(!s.shutting_down, "shell is shutting down");
        let ds = s.displays.get(&display).ok_or_else(|| anyhow::anyhow!("unknown display {display:?}"))?;
        if let Some(h) = ds.windows.get(&kind) {
            return Ok(*h);
        }
        let opts = layer_options(kind, &ds.info, theme(cx));
        let slots: Vec<(Slot, Vec<AnyView>)> = match kind {
            SurfaceKind::Rail => vec![(Slot::Rail, ds.slot_views(Slot::Rail))],
            SurfaceKind::Scratchpad => vec![(Slot::Scratchpad, ds.slot_views(Slot::Scratchpad))],
            SurfaceKind::Bar => {
                [Slot::BarStart, Slot::BarCenter, Slot::BarEnd].into_iter().map(|s| (s, ds.slot_views(s))).collect()
            }
            _ => Vec::new(),
        };
        (opts, slots)
    };
    let weak = shell.downgrade();
    let niri = shell.read(cx).services.niri.state.clone();
    let handle = cx.open_window(opts, move |_, cx| {
        cx.new(|cx| {
            if let Some(shell) = weak.upgrade() {
                cx.observe(&shell, |_, _, cx| cx.notify()).detach();
            }
            if kind == SurfaceKind::Bar {
                // Re-render only when this display's fullscreen state flips.
                cx.observe(&niri, |root: &mut SurfaceRoot, niri, cx| {
                    let output = root.shell.upgrade().and_then(|s| s.read(cx).displays.get(&root.display)?.info.output.clone());
                    let full = output.is_some_and(|o| frame::is_fullscreen(niri.read(cx), &o));
                    if full != root.fullscreen {
                        root.fullscreen = full;
                        cx.notify();
                    }
                })
                .detach();
            }
            SurfaceRoot {
                kind,
                display,
                shell: weak,
                slots,
                hit: Default::default(),
                size: Size::default(),
                fullscreen: false,
                focus: (kind == SurfaceKind::OverlayModal).then(|| cx.focus_handle()),
                lingering: false,
            }
        })
    })?;
    let id = display;
    tracing::info!("opened {} on {id:?}", kind.name());
    shell.update(cx, |s, _| {
        if let Some(ds) = s.displays.get_mut(&display) {
            ds.windows.insert(kind, handle);
        }
    });
    Ok(handle)
}

pub(crate) fn close_surface(shell: &Entity<Shell>, display: DisplayId, kind: SurfaceKind, cx: &mut App) {
    let handle = shell.update(cx, |s, _| s.displays.get_mut(&display).and_then(|d| d.windows.remove(&kind)));
    if let Some(h) = handle {
        overlay::forget_window(h.window_id(), cx);
        remove_window(h, kind, 0, cx);
        let id = display;
        tracing::info!("closed {} on {id:?}", kind.name());
    }
}

/// `remove_window` fails while the window is being dispatched to (a close triggered by
/// its own event handler): retry at the end of the effect cycle so no layer surface is
/// left behind.
pub(crate) fn remove_window(h: WindowHandle<SurfaceRoot>, kind: SurfaceKind, attempt: u32, cx: &mut App) {
    if h.update(cx, |_, window, _| window.remove_window()).is_ok() {
        return;
    }
    if attempt >= 3 {
        // Already gone (the platform closed it) or never coming back: nothing to remove.
        tracing::debug!("{} window {:?} not removable (already closed?)", kind.name(), h.window_id());
        return;
    }
    cx.defer(move |cx| remove_window(h, kind, attempt + 1, cx));
}

/// Create surfaces + panel instances for displays that appeared, drop those that went,
/// refresh output names.
pub(crate) fn sync_displays(shell: &Entity<Shell>, cx: &mut App) {
    let current: Vec<(DisplayId, Option<uuid::Uuid>, Bounds<Pixels>)> =
        cx.displays().iter().map(|d| (d.id(), d.uuid().ok(), d.bounds())).collect();

    let gone: Vec<DisplayId> =
        shell.read(cx).displays.keys().copied().filter(|id| !current.iter().any(|(c, _, _)| c == id)).collect();
    for id in gone {
        tracing::info!("display {id:?} removed");
        // A modal shown there is dismissed properly (`on_close`), not just forgotten.
        if shell.read(cx).modal.as_ref().is_some_and(|m| m.display == id) {
            overlay::close_modal(shell, cx);
        }
        for kind in SurfaceKind::CLOSE_ORDER {
            close_surface(shell, id, kind, cx);
        }
        // Dropping the DisplaySurfaces releases that display's frame/passive panels.
        shell.update(cx, |s, _| {
            s.displays.remove(&id);
        });
        crate::anchors::forget_display(id, cx);
    }
    if shell.read(cx).shutting_down {
        return;
    }

    let outputs: Vec<String> = shell.read(cx).services.niri.state.read(cx).outputs.keys().cloned().collect();
    let mut changed = false;
    for (id, uuid, bounds) in current {
        let output = uuid.and_then(|u| outputs.iter().find(|n| output_uuid(n) == u).cloned());
        let known = shell.update(cx, |s, _| {
            let Some(ds) = s.displays.get_mut(&id) else { return false };
            let info = DisplayInfo { id, output: output.clone().or(ds.info.output.clone()), bounds };
            if ds.info != info {
                ds.info = info;
                changed = true;
            }
            true
        });
        if known {
            continue;
        }
        let info = DisplayInfo { id, output, bounds };
        tracing::info!("display {id:?} added: {} {:?}", info.output_label(), bounds.size);
        let (registry, ctx, scratch) = {
            let s = shell.read(cx);
            let ctx = PanelContext {
                shell: crate::ShellHandle(shell.downgrade()),
                services: s.services.clone(),
                niri: s.services.niri.state.clone(),
                niri_handle: s.services.niri.handle.clone(),
                display: Some(info.clone()),
            };
            (s.registry.clone(), ctx, s.scratch_visible)
        };
        let panels = registry.build(|p| !matches!(p, Placement::Modal(_)), &ctx, cx);
        shell.update(cx, |s, _| {
            s.displays.insert(id, DisplaySurfaces { info, panels, windows: HashMap::new() });
        });
        changed = true;
        for kind in frame_kinds(scratch, theme(cx)) {
            if let Err(e) = open_surface(shell, id, kind, cx) {
                tracing::error!("cannot open {} on {id:?}: {e:#}", kind.name());
            }
        }
    }
    if changed {
        shell.update(cx, |_, cx| cx.notify());
    }
}

/// Recreate the frame surfaces of every display if the theme's frame geometry changed
/// since they were created (rail width, bar height, floating gap/margins). Layer-shell
/// margins and anchors cannot change on a live surface (GPUI-NOTES §2 item 9), so this
/// closes bar/rail/scratchpad and reopens them in creation order (bar first: exclusive
/// zones). Panel instances are kept — only their windows are new.
pub(crate) fn sync_shape(shell: &Entity<Shell>, cx: &mut App) {
    let key = ShapeKey::of(theme(cx));
    let (old, ids, scratch) = {
        let s = shell.read(cx);
        (s.shape, s.displays.keys().copied().collect::<Vec<_>>(), s.scratch_visible)
    };
    if old == Some(key) || shell.read(cx).shutting_down {
        return;
    }
    shell.update(cx, |s, _| s.shape = Some(key));
    let Some(old) = old else { return };
    tracing::info!("frame shape changed {old:?} -> {key:?}: recreating frame surfaces");
    for id in ids {
        for kind in [SurfaceKind::ScratchpadZone, SurfaceKind::Scratchpad, SurfaceKind::Rail, SurfaceKind::Bar] {
            close_surface(shell, id, kind, cx);
        }
        for kind in frame_kinds(scratch, theme(cx)) {
            if let Err(e) = open_surface(shell, id, kind, cx) {
                tracing::error!("cannot reopen {} on {id:?}: {e:#}", kind.name());
            }
        }
    }
    shell.update(cx, |_, cx| cx.notify());
}

pub(crate) fn set_scratch_visible(shell: &Entity<Shell>, visible: bool, cx: &mut App) {
    let ids: Vec<DisplayId> = shell.update(cx, |s, _| {
        if s.scratch_visible == visible {
            return Vec::new();
        }
        s.scratch_visible = visible;
        s.displays.keys().copied().collect()
    });
    for id in ids {
        if visible {
            // Appended after the bar and rail: the floating scratchpad (zone -1) stacks
            // above the bar's transparent right end, its zone keeper only narrows windows.
            for kind in scratch_kinds(theme(cx)) {
                if let Err(e) = open_surface(shell, id, kind, cx) {
                    tracing::error!("cannot open {}: {e:#}", kind.name());
                }
            }
        } else {
            for kind in [SurfaceKind::ScratchpadZone, SurfaceKind::Scratchpad] {
                close_surface(shell, id, kind, cx);
            }
        }
    }
    shell.update(cx, |_, cx| cx.notify());
}
