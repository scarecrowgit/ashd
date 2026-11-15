//! Dock (desk-05 l.1659–1744, sys-04 "Rail and dock"): bordered launcher button, a 1×24
//! separator, then the app buttons — pinned apps (`shell.json` `dock`) followed by other
//! running apps (niri windows matched to desktop entries).
//!
//! App button 34×34, 18px icon (`App::icon_path`, else a stroke glyph). States: idle
//! (fg-muted glyph, no indicator), running (4×2 fg-muted indicator), focused (surface-2
//! background, 14×2 accent indicator, 160 ms standard transition), hover (surface-2).
//! Click: focus the app's most recent window, or launch it. Middle click: new instance.
//! Wheel: cycle the app's windows. Centred on `DOCK_CENTER_X` of the output.
//!
//! Owner: shell frame.

use std::{collections::HashMap, sync::Arc, time::Instant};

use gpui::{
    AnimationExt as _, App, ClickEvent, Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollDelta, ScrollEvent, StatefulInteractiveElement as _, Styled as _, Window, div, img, prelude::*,
    px,
};
use ashd_ipc::{PanelOp, Request};
use ashd_niri::{NiriState, niri_ipc::Action};
use ashd_shell_core::{
    ModalAnchor, Panel, PanelContext, PanelId, Placement, Slot,
    apps::{app_for_app_id, app_glyph},
    frame,
};
use ashd_svc_apps::App as DesktopApp;
use ashd_tokens::{DockVariant, metrics};
use ashd_ui::{
    Icon,
    anim::{EASE_OUT_EXPO, EASE_OUT_QUAD, EASE_STANDARD, timing, tween},
    fx::{follow, hover_wash, mix},
    icon,
    presence::{PresenceList, Timing},
    theme,
};

/// Id of the launcher panel (no crate dependency between panels: ids only).
pub const LAUNCHER: PanelId = PanelId("launcher");

/// Geometry (desk-05 l.1659–1682, sys-04 l.~760).
pub mod geom {
    /// Dock row gap (launcher │ apps) and app-button gap.
    pub const GAP: f32 = 10.0;
    pub const APP_GAP: f32 = 4.0;
    /// Launcher glyph 15 in a 36 box (`metrics::DOCK_BUTTON`).
    pub const LAUNCHER_ICON: f32 = 15.0;
    pub const SEP_H: f32 = 24.0;
    /// Workspace label in the split dock's launcher pill (desk-06 l.1669): gap 10, index
    /// mono 11 (name / mode: the label size).
    pub const LABEL_GAP: f32 = 10.0;
    pub const LABEL_INDEX_SIZE: f32 = 11.0;
    /// App button of the Ash bar (split / pill docks: 38, `ashd_shell_core::frame::dock_app_size`)
    /// and its indicator (bottom 2, 4×2 running / 14×2 focused).
    pub const APP: f32 = 34.0;
    pub const IND_H: f32 = 2.0;
    pub const IND_BOTTOM: f32 = 2.0;
    pub const IND_RUNNING: f32 = 4.0;
    pub const IND_FOCUSED: f32 = 14.0;
    /// Opacity of a pinned, not running app's (colour) icon: reads as fg-muted.
    pub const IDLE_OPACITY: f32 = 0.55;
    /// Touchpad pixels per window step (GPUI scales Wayland axis values ×3).
    pub const SCROLL_STEP_PX: f32 = 240.0;
}

/// One dock button.
#[derive(Clone, Debug, PartialEq)]
pub struct DockItem {
    /// Desktop id if known, else the niri app id.
    pub key: String,
    pub app: Option<DesktopApp>,
    /// niri app id the windows carry (for matching / cycling).
    pub app_id: String,
    pub pinned: bool,
    /// Window ids, most recently focused first.
    pub windows: Vec<u64>,
    pub focused: bool,
}

/// Windows of `app_id`, most recently focused first (focused window, then
/// `focus_timestamp`, then newest id).
pub fn windows_mru(niri: &NiriState, app_id: &str) -> Vec<u64> {
    let mut v: Vec<_> = niri.windows.values().filter(|w| w.app_id.as_deref() == Some(app_id)).collect();
    v.sort_by(|a, b| {
        b.is_focused
            .cmp(&a.is_focused)
            .then_with(|| {
                let ts = |w: &&ashd_niri::niri_ipc::Window| w.focus_timestamp.map(|t| (t.secs, t.nanos));
                ts(b).cmp(&ts(a))
            })
            .then(b.id.cmp(&a.id))
    });
    v.into_iter().map(|w| w.id).collect()
}

/// Pinned apps (in config order) then running apps (by first window id).
pub fn dock_items(pins: &[String], apps: &[DesktopApp], niri: &NiriState) -> Vec<DockItem> {
    let focused_app = niri.focused_window().and_then(|w| w.app_id.clone());
    let mut items: Vec<DockItem> = Vec::new();
    for pin in pins {
        let app = apps.iter().find(|a| &a.id == pin).cloned();
        let app_id = app.as_ref().map(|a| a.window_app_id().to_string()).unwrap_or_else(|| pin.clone());
        if items.iter().any(|i| i.key == *pin) {
            continue;
        }
        items.push(DockItem { key: pin.clone(), app, app_id, pinned: true, windows: Vec::new(), focused: false });
    }
    // Running app ids in order of their oldest window.
    let mut running: Vec<(u64, String)> = Vec::new();
    for w in niri.windows.values() {
        let Some(id) = w.app_id.clone().filter(|s| !s.is_empty()) else { continue };
        match running.iter_mut().find(|(_, a)| *a == id) {
            Some(r) => r.0 = r.0.min(w.id),
            None => running.push((w.id, id)),
        }
    }
    running.sort();
    for (_, app_id) in running {
        let app = app_for_app_id(apps, &app_id);
        // A pinned item owns the windows of its app.
        let slot = items.iter().position(|i| {
            i.app_id.eq_ignore_ascii_case(&app_id) || app.is_some_and(|a| a.id == i.key)
        });
        let ix = match slot {
            Some(ix) => ix,
            None => {
                items.push(DockItem {
                    key: app.map(|a| a.id.clone()).unwrap_or_else(|| app_id.clone()),
                    app: app.cloned(),
                    app_id: app_id.clone(),
                    pinned: false,
                    windows: Vec::new(),
                    focused: false,
                });
                items.len() - 1
            }
        };
        let item = &mut items[ix];
        // The windows carry this app id (it may differ from the desktop guess).
        item.app_id = app_id.clone();
        item.windows.extend(windows_mru(niri, &app_id));
        item.focused |= focused_app.as_deref() == Some(app_id.as_str());
    }
    items
}

pub struct DockState {
    /// Per item: (focused, generation) — a change restarts the 160 ms indicator tween.
    pub indicator: HashMap<String, (bool, u64)>,
    pub scroll_acc: f32,
    /// The buttons as drawn: a new app's button grows in, a closed app's shrinks away and
    /// the others glide (the row is laid out each frame while it moves).
    items: PresenceList<String, DockItem>,
}

impl Default for DockState {
    fn default() -> Self {
        Self {
            indicator: HashMap::new(),
            scroll_acc: 0.0,
            items: PresenceList::new(
                Timing::new(timing::ITEM_IN, EASE_OUT_EXPO),
                Timing::new(timing::ITEM_OUT, EASE_OUT_QUAD),
            ),
        }
    }
}

pub struct DockPanel {
    pub ctx: PanelContext,
    pub state: DockState,
    launcher_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
}

impl DockPanel {
    fn activate(&self, item: &DockItem, cx: &App) {
        match item.windows.first() {
            Some(&id) => self.ctx.shell.niri_action(Action::FocusWindow { id }, cx),
            None => self.launch(item, cx),
        }
    }

    fn launch(&self, item: &DockItem, cx: &App) {
        let what = item.app.as_ref().map(|a| a.id.clone()).unwrap_or_else(|| item.app_id.clone());
        self.ctx.shell.launch(&what, cx);
    }

    /// Focus the next (`steps` > 0) / previous window of `item`, in window-id order.
    fn cycle(&self, item: &DockItem, steps: i32, cx: &App) {
        let mut ids = item.windows.clone();
        if ids.is_empty() {
            return;
        }
        ids.sort_unstable();
        let niri = self.ctx.niri.read(cx);
        let cur = niri.focused_window().and_then(|w| ids.iter().position(|id| *id == w.id));
        let n = ids.len() as i32;
        let to = match cur {
            Some(c) => (c as i32 + steps).rem_euclid(n),
            None => 0,
        };
        self.ctx.shell.niri_action(Action::FocusWindow { id: ids[to as usize] }, cx);
    }

    /// (number the user sees, name, canvas mode) of the active workspace of this panel's
    /// output (focused one if the output is unknown) — the split dock's launcher pill shows
    /// it. The number skips hidden (scratchpad) workspaces, unlike niri's raw `idx`.
    fn workspace(&self, cx: &App) -> Option<(u8, Option<String>, bool)> {
        let niri = self.ctx.niri.read(cx);
        let output = self.ctx.display.as_ref().and_then(|d| self.ctx.shell.display(d.id, cx)).and_then(|d| d.output);
        let ws = match output {
            Some(o) => niri.active_workspace(&o),
            None => niri.focused_workspace(),
        }?;
        // a fullscreen workspace is labelled as its desk
        let desk = niri.desk(ws.id).unwrap_or(ws);
        let n = niri.visible_number(desk.id).map_or(desk.idx.max(1), |n| n.min(u8::MAX as usize) as u8);
        Some((n, desk.name.clone(), niri.is_canvas(ws.id)))
    }

    fn launcher_anchor(&self, cx: &App) -> Option<ModalAnchor> {
        let b = self.launcher_bounds.get()?;
        let info = self.ctx.shell.display(self.ctx.display.as_ref()?.id, cx)?;
        Some(info.bar_to_output(b, theme(cx)))
    }
}

impl Panel for DockPanel {
    const ID: PanelId = PanelId("dock");
    const PLACEMENT: Placement = Placement::Frame(Slot::BarCenter);

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        // Re-render only when the dock's inputs change (not on every window move): windows,
        // their apps and focus, and (split dock: the launcher pill's label) the active
        // workspace of this output.
        type Key = (Vec<(u64, Option<String>, bool)>, Option<(u8, Option<String>, bool)>);
        cx.observe(&ctx.niri, {
            let mut last: Option<Key> = None;
            move |this: &mut Self, niri, cx| {
                let mut wins: Vec<_> =
                    niri.read(cx).windows.values().map(|w| (w.id, w.app_id.clone(), w.is_focused)).collect();
                wins.sort_unstable();
                let key = (wins, this.workspace(cx));
                if last.as_ref() != Some(&key) {
                    last = Some(key);
                    cx.notify();
                }
            }
        })
        .detach();
        cx.observe(&ctx.services.apps.state, |_, _, cx| cx.notify()).detach();
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |_, _, cx| cx.notify()).detach();
        }
        Self { ctx, state: DockState::default(), launcher_bounds: Default::default() }
    }
}

impl Render for DockPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let apps: Arc<Vec<DesktopApp>> = self.ctx.services.apps.state.read(cx).apps.clone();
        let pins = ashd_shell_core::config(cx).dock.clone();
        let items = dock_items(&pins, &apps, self.ctx.niri.read(cx));
        let (fg, muted, s2, accent) = (t.fg, t.fg_muted, t.surface_2, t.accent);

        // Indicator transitions: bump the generation of items whose focus flipped.
        let mut seen = Vec::with_capacity(items.len());
        for it in &items {
            let e = self.state.indicator.entry(it.key.clone()).or_insert((it.focused, 0));
            if e.0 != it.focused {
                *e = (it.focused, e.1 + 1);
            }
            seen.push(it.key.clone());
        }
        self.state.indicator.retain(|k, _| seen.contains(k));
        let now = Instant::now();
        self.state.items.sync(now, items.iter().map(|i| (i.key.clone(), i.clone())));
        if self.state.items.is_animating(now) {
            window.request_animation_frame();
        }
        let shown: Vec<_> = self.state.items.entries(now).to_vec();

        let launcher_open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == LAUNCHER);
        let bounds = self.launcher_bounds.clone();
        let launcher = div()
            .id("dock-launcher")
            .relative()
            .flex_none()
            .size(px(metrics::DOCK_BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_control)
            .cursor_pointer()
            .when(launcher_open, |d| d.bg(s2).border_color(accent))
            .when(!launcher_open, |d| d.child(hover_wash(s2, None, t.radius_control)))
            .child(icon(Icon::Grid, geom::LAUNCHER_ICON).text_color(fg))
            .child(
                gpui::canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                let anchor = this.launcher_anchor(cx);
                this.ctx.shell.request(Request::Launcher { op: PanelOp::Toggle, mode: None }, anchor, cx);
            }));

        let app_size = frame::dock_app_size(&t);
        let buttons = shown.into_iter().map(|entry| {
            let (fade, grow, alive) = (entry.presence.value(now), entry.presence.movement(now), entry.is_present());
            let item = entry.value;
            let running = !item.windows.is_empty();
            let generation = self.state.indicator.get(&item.key).map(|e| e.1).unwrap_or(0);
            let glyph = app_glyph(item.app.as_ref(), &item.app_id);
            let icon_el = match item.app.as_ref().and_then(|a| a.icon_path.clone()) {
                Some(path) => img(path)
                    .size(px(metrics::icon_size::DOCK))
                    .flex_none()
                    .when(!running, |i| i.opacity(geom::IDLE_OPACITY))
                    .into_any_element(),
                None => icon(glyph, metrics::icon_size::DOCK).text_color(if running { fg } else { muted }).into_any_element(),
            };
            let indicator = running.then(|| {
                let (from, to, c_from, c_to): (f32, f32, Hsla, Hsla) = if item.focused {
                    (geom::IND_RUNNING, geom::IND_FOCUSED, muted, accent)
                } else {
                    (geom::IND_FOCUSED, geom::IND_RUNNING, accent, muted)
                };
                let bar = div().absolute().bottom(px(geom::IND_BOTTOM)).left_0().right_0().flex().justify_center();
                let line = div().h(px(geom::IND_H));
                if generation == 0 {
                    // First sight: no transition.
                    bar.child(line.w(px(to)).bg(c_to)).into_any_element()
                } else {
                    bar.child(line.with_animation(
                        gpui::ElementId::NamedInteger(format!("dock-ind-{}", item.key).into(), generation),
                        tween(timing::DOCK_INDICATOR, EASE_STANDARD),
                        move |el, p| el.w(px(from + (to - from) * p)).bg(ashd_ui_mix(c_from, c_to, p)),
                    ))
                    .into_any_element()
                }
            });
            let focused = item.focused;
            let (click, middle, wheel) = (item.clone(), item.clone(), item.clone());
            let radius = t.radius_control;
            let key = item.key.clone();
            // The focused app's plate fades in / out with the indicator (160 ms standard).
            let plate = follow(
                gpui::SharedString::from(format!("dock-plate-{key}")),
                if focused { 1.0 } else { 0.0 },
                Timing::new(timing::DOCK_INDICATOR, EASE_STANDARD),
                move |p| div().absolute().top_0().left_0().size_full().rounded(radius).bg(mix(gpui::transparent_black(), s2, p)),
            )
            .fade();
            let button = div()
                .id(gpui::SharedString::from(format!("dock-app-{key}")))
                .relative()
                .flex_none()
                .size(px(app_size))
                .flex()
                .items_center()
                .justify_center()
                .rounded(t.radius_control)
                .when(alive, |d| d.cursor_pointer())
                .child(plate)
                .child(hover_wash(s2, None, radius))
                .child(icon_el)
                .children(indicator)
                .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    if alive && ev.standard_click() {
                        this.activate(&click, cx);
                    }
                }))
                .on_aux_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                    if alive && ev.is_middle_click() {
                        this.launch(&middle, cx);
                    }
                }))
                .on_scroll(cx.listener(move |this, ev: &ScrollEvent, _, cx| {
                    let steps = match ev.delta {
                        ScrollDelta::Lines(p) => -p.y.signum() as i32 * (p.y.abs() >= 0.5) as i32,
                        ScrollDelta::Pixels(p) => {
                            let dy = f32::from(p.y);
                            if dy * this.state.scroll_acc < 0.0 {
                                this.state.scroll_acc = 0.0;
                            }
                            this.state.scroll_acc += dy;
                            let n = (this.state.scroll_acc / geom::SCROLL_STEP_PX).trunc();
                            this.state.scroll_acc -= n * geom::SCROLL_STEP_PX;
                            -(n as i32)
                        }
                    };
                    if alive && steps != 0 {
                        this.cycle(&wheel, steps, cx);
                    }
                }));
            // The button sits in a slot whose width follows the presence (grows in, shrinks
            // away); the slot's margin carries the gap so a vanished app leaves no hole.
            div()
                .flex_none()
                .h(px(app_size))
                .w(px(app_size * grow))
                .ml(px(geom::APP_GAP * grow))
                .overflow_hidden()
                .opacity(fade)
                .child(button)
        });

        let has_apps = !pins.is_empty() || self.ctx.niri.read(cx).windows.values().any(|w| w.app_id.is_some());
        let apps = div().flex().flex_none().items_center().ml(px(-geom::APP_GAP)).children(buttons);
        let sep = || div().flex_none().w(px(1.)).h(px(geom::SEP_H)).bg(t.line);
        match t.dock {
            DockVariant::Bar => {
                // The centre slot starts after the status block: centre the row on
                // DOCK_CENTER_X of the output by giving it a box symmetric around that point.
                let half = px(metrics::DOCK_CENTER_X - metrics::STATUS_W);
                div().size_full().relative().child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .w(half * 2.)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(geom::GAP))
                                .child(launcher)
                                .when(has_apps, |d| d.child(sep()))
                                .child(apps),
                        ),
                )
            }
            // Ink / Moss (desk-06 l.1664): launcher pill (launcher + "02 code scroll") and
            // the apps pill, spread like CSS `space-between` together with the mode pill
            // that follows this view (the last spacer's −gap margin makes both spaces equal).
            DockVariant::Split => {
                let ws = self.workspace(cx);
                let (fg, muted) = (t.fg, t.fg_muted);
                let text = |s: String, size: f32, color: Hsla| {
                    div()
                        .flex_none()
                        .font_family(t.font_mono.clone())
                        .text_size(px(size))
                        .line_height(px((size * 1.3).round()))
                        .text_color(color)
                        .child(t.label_text(&s))
                };
                let label = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(geom::LABEL_GAP))
                    .children(ws.as_ref().map(|(n, _, _)| text(format!("{n:02}"), geom::LABEL_INDEX_SIZE, fg)))
                    .children(
                        ws.as_ref()
                            .and_then(|(_, n, _)| n.clone())
                            .filter(|n| !n.is_empty())
                            .map(|n| text(n, metrics::type_scale::LABEL.0, muted)),
                    )
                    .child(text(
                        if ws.as_ref().is_some_and(|w| w.2) { "canvas" } else { "scroll" }.into(),
                        metrics::type_scale::LABEL.0,
                        muted,
                    ));
                let pill = || {
                    frame::frame_box(&t).flex_none().h_full().px(px(frame::SPLIT_PILL_PAD_X)).flex().items_center().gap(px(frame::PILL_GAP))
                };
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .child(pill().child(launcher).child(label))
                    .child(div().flex_1())
                    .when(has_apps, |d| d.child(pill().child(apps)))
                    .child(div().flex_1().mr(-t.gap))
            }
            // Midnight / Dusk (desk-07 l.1631): launcher │ apps inside the dock pill that
            // shell-core draws around this view and the mode switch.
            DockVariant::Pill => div()
                .h_full()
                .flex()
                .items_center()
                .gap(px(frame::PILL_GAP))
                .child(launcher)
                .when(has_apps, |d| d.child(sep()).child(apps)),
        }
    }
}

/// Linear colour mix for the indicator tween.
fn ashd_ui_mix(a: Hsla, b: Hsla, p: f32) -> Hsla {
    let (a, b): (gpui::Rgba, gpui::Rgba) = (a.into(), b.into());
    gpui::Rgba { r: a.r + (b.r - a.r) * p, g: a.g + (b.g - a.g) * p, b: a.b + (b.b - a.b) * p, a: a.a + (b.a - a.a) * p }
        .into()
}

#[cfg(test)]
mod tests {
    use ashd_niri::niri_ipc::{Event, Window, WindowLayout};

    use super::*;

    fn win(id: u64, app: &str, focused: bool) -> Window {
        Window {
            id,
            title: None,
            app_id: Some(app.into()),
            pid: None,
            workspace_id: Some(1),
            is_focused: focused,
            is_floating: false,
            is_urgent: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: Some((1, 1)),
                tile_size: (1.0, 1.0),
                window_size: (1, 1),
                tile_pos_in_workspace_view: None,
                window_offset_in_tile: (0.0, 0.0),
                pos_in_canvas: None,
                is_pinned: false,
            },
            focus_timestamp: None,
            hidden_from_workspace_id: None,
        }
    }

    fn app(id: &str) -> DesktopApp {
        DesktopApp { id: id.into(), name: id.into(), exec: vec![id.into()], ..Default::default() }
    }

    #[test]
    fn pinned_then_running() {
        let mut n = NiriState::default();
        n.apply(Event::WindowsChanged {
            windows: vec![win(5, "foot", false), win(3, "firefox", true), win(9, "firefox", false)],
        });
        let apps = vec![app("kitty"), app("firefox"), app("foot")];
        let items = dock_items(&["kitty".into(), "firefox".into()], &apps, &n);
        let keys: Vec<_> = items.iter().map(|i| (i.key.as_str(), i.pinned, i.windows.len(), i.focused)).collect();
        assert_eq!(keys, vec![("kitty", true, 0, false), ("firefox", true, 2, true), ("foot", false, 1, false)]);
        // Most recent first: the focused one.
        assert_eq!(items[1].windows.first(), Some(&3));
    }
}
