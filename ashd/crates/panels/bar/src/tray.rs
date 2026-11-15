//! System tray in the status block (after the keyboard-layout chip, before the bell):
//! hidden while empty; Active and NeedsAttention items as 16px icons in 24px hit boxes
//! (NeedsAttention with a warn dot), Passive ones — and whatever does not fit — behind a
//! small chevron that opens the list of hidden items. The status block hugs its content up
//! to `STATUS_W`: the bar computes the room the other items leave (`geom::tray_avail`), the
//! plan fits the icons into it, and the area is as wide as what it draws (it clips, with
//! `min-width: 0`, only if that estimate was short), so the bell is never pushed out.
//!
//! Mouse: left = `Activate` (or the menu when `ItemIsMenu`), middle = `SecondaryActivate`,
//! right = the dbusmenu (else `ContextMenu`), wheel = `Scroll`. The menu itself is the
//! modal [`crate::tray_menu::TrayMenuPanel`], the hidden-items list
//! [`crate::tray_overflow::TrayOverflowPanel`], the tooltip (a bar surface is only 46 px
//! tall, so it cannot draw above itself) the passive [`TrayTooltipPanel`].
//!
//! [`TrayUi`] is the one entity the three panels and every bar (one per display) share:
//! pending menu, hover/tooltip, scroll accumulator, the hidden-item snapshot.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, DisplayId, Div, ElementId, Entity, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, ScrollDelta, ScrollEvent, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, prelude::*, px,
};
use ashd_shell_core::{ModalAnchor, Panel, PanelContext, PanelId, Placement, ShellHandle, frame};
use ashd_svc_tray::{Orientation, Status, TrayItem, TrayState};
use ashd_tokens::{LayoutVariant, metrics};
use ashd_ui::{
    Icon, Theme,
    anim::{EASE_OUT_EXPO, EASE_OUT_QUAD, motion::timing},
    app_icon, icon,
    presence::{PresenceList, Timing},
    theme, tooltip,
};

use crate::{BarPanel, geom, hover_layer, ids, open_layer};

// ── what fits ─────────────────────────────────────────────────────────────────

/// Which items are drawn in the bar (`shown`, registration order) and which are behind
/// the chevron (`hidden`).
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub shown: Vec<usize>,
    pub hidden: Vec<usize>,
}

fn fit(width: f32) -> usize {
    ((width + geom::TRAY_GAP) / (geom::TRAY_BOX + geom::TRAY_GAP)).floor().max(0.0) as usize
}

/// Passive items are always hidden; Active and NeedsAttention ones are shown while they
/// fit (NeedsAttention first), the rest joins the hidden ones. A chevron (which takes
/// room itself) appears exactly when something is hidden.
pub fn plan(statuses: &[Status], width: f32) -> Plan {
    let n = statuses.len();
    if !statuses.contains(&Status::Passive) && n <= fit(width) {
        return Plan { shown: (0..n).collect(), hidden: Vec::new() };
    }
    let capacity = fit(width - geom::TRAY_CHEVRON_W - geom::TRAY_GAP);
    let mut order: Vec<usize> = (0..n).filter(|&i| statuses[i] != Status::Passive).collect();
    order.sort_by_key(|&i| (statuses[i] != Status::NeedsAttention, i));
    let mut shown: Vec<usize> = order.into_iter().take(capacity).collect();
    shown.sort_unstable();
    let hidden = (0..n).filter(|i| !shown.contains(i)).collect();
    Plan { shown, hidden }
}

/// How the now-playing control ([`crate::nowplaying`]) fits next to the tray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaFit {
    /// No active player: no control.
    None,
    /// Only the music glyph (no room for a title).
    Glyph,
    /// Glyph + title, the title at most this wide (ellipsised).
    Title(f32),
}

/// [`plan`] for the whole area the tray shares with the now-playing control. The control's
/// glyph always shows while a player is active; the tray gets the rest by [`plan`]; the
/// title takes what the tray leaves, and goes first when space runs out (title → glyph
/// only → then tray icons move behind the chevron).
#[derive(Debug, PartialEq)]
pub struct RowPlan {
    pub tray: Plan,
    pub media: MediaFit,
}

/// Width of the whole tray area for a row of `avail` px (the now-playing title at its widest:
/// a safe over-estimate).
pub fn area_width(statuses: &[Status], avail: f32, media: bool) -> f32 {
    let row = plan_row(statuses, avail, media);
    let tray = tray_used(&row.tray);
    let np = match row.media {
        MediaFit::None => 0.0,
        MediaFit::Glyph => geom::NOW_PLAYING_GLYPH_W,
        MediaFit::Title(w) => geom::NOW_PLAYING_GLYPH_W + geom::NOW_PLAYING_TITLE_GAP + w,
    };
    np + tray + if np > 0.0 && tray > 0.0 { geom::TRAY_GAP } else { 0.0 }
}

/// Width the tray's drawn icons (+ chevron) take for `plan`.
fn tray_used(p: &Plan) -> f32 {
    let icons = p.shown.len() as f32;
    let mut w = icons * geom::TRAY_BOX + (icons - 1.0).max(0.0) * geom::TRAY_GAP;
    if !p.hidden.is_empty() {
        w += geom::TRAY_CHEVRON_W + if icons > 0.0 { geom::TRAY_GAP } else { 0.0 };
    }
    w
}

pub fn plan_row(statuses: &[Status], width: f32, media: bool) -> RowPlan {
    if !media {
        return RowPlan { tray: plan(statuses, width), media: MediaFit::None };
    }
    let glyph = geom::NOW_PLAYING_GLYPH_W + geom::TRAY_GAP;
    let tray = plan(statuses, width - glyph);
    let used = tray_used(&tray);
    let leftover = width - glyph - used - geom::NOW_PLAYING_TITLE_GAP;
    let media = if leftover >= geom::NOW_PLAYING_TITLE_MIN {
        MediaFit::Title(leftover.min(geom::NOW_PLAYING_TITLE_MAX))
    } else {
        MediaFit::Glyph
    };
    RowPlan { tray, media }
}

// ── shared UI state ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

struct Pending {
    id: String,
    anchor: Option<ModalAnchor>,
    expires: Instant,
}

/// The tooltip being shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Tip {
    pub id: String,
    /// The icon, output-local.
    pub anchor: ModalAnchor,
    pub display: DisplayId,
}

pub struct TrayUi {
    shell: ShellHandle,
    handle: ashd_svc_tray::Handle,
    /// A menu was asked for by a click; the first menu of that item opens the modal at
    /// `anchor` (the icon, or the chevron for a hidden item).
    pending: Option<Pending>,
    /// Ids behind the chevron when it was last clicked, and where the chevron is.
    pub hidden: Vec<String>,
    pub chevron: Option<ModalAnchor>,
    pub tip: Option<Tip>,
    tip_generation: u64,
    scroll: (String, f32),
}

struct TrayUiGlobal(Entity<TrayUi>);

impl Global for TrayUiGlobal {}

/// How long a click may wait for the menu it asked for.
const MENU_PATIENCE: Duration = Duration::from_secs(15);
/// …for `Activate` (the menu only comes if the app has no `Activate`).
const ACTIVATE_PATIENCE: Duration = Duration::from_secs(3);

impl TrayUi {
    /// The process-wide instance (created by whichever tray panel is built first).
    pub fn get(ctx: &PanelContext, cx: &mut App) -> Entity<TrayUi> {
        if let Some(g) = cx.try_global::<TrayUiGlobal>() {
            return g.0.clone();
        }
        let ui = cx.new(|cx: &mut Context<TrayUi>| {
            cx.observe(&ctx.services.tray.state, |this, state, cx| this.on_tray(&state, cx)).detach();
            TrayUi {
                shell: ctx.shell.clone(),
                handle: ctx.services.tray.handle.clone(),
                pending: None,
                hidden: Vec::new(),
                chevron: None,
                tip: None,
                tip_generation: 0,
                scroll: (String::new(), 0.0),
            }
        });
        cx.set_global(TrayUiGlobal(ui.clone()));
        ui
    }

    fn on_tray(&mut self, state: &Entity<TrayState>, cx: &mut Context<Self>) {
        let (menu_item, tip_gone) = {
            let s = state.read(cx);
            (s.menu.as_ref().map(|m| m.item.clone()), self.tip.as_ref().is_some_and(|t| s.item(&t.id).is_none()))
        };
        let menu_modal_open = self.shell.modal(cx).is_some_and(|m| m.panel == ids::TRAY_MENU);
        if let Some(item) = menu_item {
            if let Some(p) = self.pending.take_if(|p| p.id == item)
                && p.expires > Instant::now()
                && !menu_modal_open
            {
                self.tip = None;
                self.shell.open_modal(ids::TRAY_MENU, p.anchor, cx);
            }
        } else if menu_modal_open {
            // the item left, or the app dropped its menu
            self.shell.close_modal(cx);
        }
        if tip_gone {
            self.tip = None;
            self.shell.refresh_overlays(cx);
        }
        if self.pending.as_ref().is_some_and(|p| p.expires < Instant::now()) {
            self.pending = None;
        }
    }

    /// Ask the service for `item`'s menu; the modal opens when it arrives.
    pub fn request_menu(&mut self, item: &TrayItem, anchor: Option<ModalAnchor>, patience: Duration, _cx: &mut Context<Self>) {
        self.pending = Some(Pending { id: item.id.clone(), anchor, expires: Instant::now() + patience });
        if patience == MENU_PATIENCE {
            self.handle.open_menu(&item.id);
        }
    }

    /// A mouse button on an icon whose bounds (output-local) are `anchor`.
    pub fn click(&mut self, item: &TrayItem, button: Button, anchor: Option<ModalAnchor>, cx: &mut Context<Self>) {
        self.hide_tip(cx);
        let (x, y) = anchor.map_or((0, 0), |a| {
            let c = a.center();
            (f32::from(c.x) as i32, f32::from(c.y) as i32)
        });
        match button {
            Button::Left if item.item_is_menu && item.has_menu() => self.request_menu(item, anchor, MENU_PATIENCE, cx),
            Button::Left => {
                if item.has_menu() {
                    // an app without `Activate` answers with an error: the service then
                    // opens the menu on its own, and it should open here
                    self.request_menu(item, anchor, ACTIVATE_PATIENCE, cx);
                }
                self.handle.activate(&item.id, x, y);
            }
            Button::Middle => self.handle.secondary_activate(&item.id, x, y),
            Button::Right if item.has_menu() => self.request_menu(item, anchor, MENU_PATIENCE, cx),
            Button::Right => self.handle.context_menu(&item.id, x, y),
        }
    }

    /// Wheel over an icon: notches of 120 for a wheel, one per 40 px for a touchpad.
    pub fn scroll(&mut self, id: &str, ev: &ScrollEvent) {
        let (notches, pixels) = match ev.delta {
            ScrollDelta::Lines(p) => (Some(p), None),
            ScrollDelta::Pixels(p) => (None, Some(p)),
        };
        if self.scroll.0 != id {
            self.scroll = (id.to_string(), 0.0);
        }
        let (horizontal, vertical) = match (notches, pixels) {
            (Some(l), _) => (l.x * geom::TRAY_SCROLL_NOTCH as f32, l.y * geom::TRAY_SCROLL_NOTCH as f32),
            (_, Some(p)) => (
                f32::from(p.x) / geom::TRAY_SCROLL_PIXELS * geom::TRAY_SCROLL_NOTCH as f32,
                f32::from(p.y) / geom::TRAY_SCROLL_PIXELS * geom::TRAY_SCROLL_NOTCH as f32,
            ),
            _ => (0.0, 0.0),
        };
        // dominant axis only
        let (orientation, amount) = if vertical.abs() >= horizontal.abs() { (Orientation::Vertical, vertical) } else { (Orientation::Horizontal, horizontal) };
        self.scroll.1 += amount;
        let step = geom::TRAY_SCROLL_NOTCH as f32;
        let whole = (self.scroll.1 / step).trunc();
        if whole != 0.0 {
            self.scroll.1 -= whole * step;
            self.handle.scroll(id, (whole * step) as i32, orientation);
        }
    }

    /// The pointer entered / left the icon (`anchor` output-local).
    pub fn hover(&mut self, id: &str, anchor: Option<ModalAnchor>, display: Option<DisplayId>, entered: bool, cx: &mut Context<Self>) {
        self.tip_generation += 1;
        let generation = self.tip_generation;
        if !entered {
            self.hide_tip(cx);
            return;
        }
        let (Some(anchor), Some(display)) = (anchor, display) else { return };
        let tip = Tip { id: id.to_string(), anchor, display };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(geom::TRAY_TOOLTIP_DELAY_MS)).await;
            let shown = this
                .update(cx, |this, cx| {
                    // still over the same icon, and no menu in the way
                    let show = this.tip_generation == generation && this.shell.modal(cx).is_none();
                    if show {
                        this.tip = Some(tip);
                        cx.notify();
                        this.shell.refresh_overlays(cx);
                    }
                    show
                })
                .unwrap_or(false);
            // GPUI keeps the last pointer position when the pointer leaves the surface, so a
            // fast exit can leave the icon "hovered": the tooltip must not outlive that.
            if shown {
                cx.background_executor().timer(Duration::from_millis(geom::TRAY_TOOLTIP_TTL_MS)).await;
                this.update(cx, |this, cx| {
                    if this.tip_generation == generation {
                        this.hide_tip(cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn hide_tip(&mut self, cx: &mut Context<Self>) {
        self.tip_generation += 1;
        if self.tip.take().is_some() {
            cx.notify();
            self.shell.refresh_overlays(cx);
        }
    }
}

// ── bar side ──────────────────────────────────────────────────────────────────

/// Last painted bounds of the icons (bar-surface px), by item id.
#[derive(Clone, Default)]
pub(crate) struct ItemBounds(Rc<RefCell<HashMap<String, Bounds<Pixels>>>>);

impl ItemBounds {
    fn track(&self, key: &str) -> impl IntoElement {
        let (store, key) = (self.0.clone(), key.to_string());
        canvas(
            move |bounds, _, _| {
                store.borrow_mut().insert(key, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn get(&self, key: &str) -> Option<Bounds<Pixels>> {
        self.0.borrow().get(key).copied()
    }
}

/// The bar's part of the tray (a field of [`BarPanel`]).
pub struct TrayBar {
    ui: Entity<TrayUi>,
    icons: ItemBounds,
    /// Ids behind the chevron in the plan last rendered.
    hidden: Rc<RefCell<Vec<String>>>,
    /// The icons as drawn: a new one grows in, a gone one shrinks away.
    presence: PresenceList<String, TrayItem>,
    /// The now-playing control as drawn (its last look is kept while it fades out).
    np: PresenceList<u8, (String, bool, MediaFit)>,
    /// The overflow chevron (what is behind it is not drawn, so it is the tray's whole
    /// presence when the row is crowded).
    chev: PresenceList<u8, ()>,
    /// Something is entering or leaving: the bar asks for frames.
    pub(crate) animating: bool,
}

impl TrayBar {
    pub fn new(ctx: &PanelContext, cx: &mut Context<BarPanel>) -> Self {
        let ui = TrayUi::get(ctx, cx);
        cx.observe(&ctx.services.tray.state, |_, _, cx| cx.notify()).detach();
        // the open menu / flyout drives the icons' "open" look
        cx.observe(&ui, |_, _, cx| cx.notify()).detach();
        let (enter, exit) = (Timing::new(timing::ITEM_IN, EASE_OUT_EXPO), Timing::new(timing::ITEM_OUT, EASE_OUT_QUAD));
        // (a list's first `sync` only fills it: take that now, so the first item that ever
        // arrives animates in)
        let now = Instant::now();
        let mut presence = PresenceList::new(enter, exit);
        presence.sync(now, []);
        let mut np = PresenceList::new(enter, exit);
        np.sync(now, []);
        let mut chev = PresenceList::new(enter, exit);
        chev.sync(now, []);
        Self { ui, icons: ItemBounds::default(), hidden: Rc::new(RefCell::new(Vec::new())), presence, np, chev, animating: false }
    }
}

impl BarPanel {
    fn tray_display(&self) -> Option<DisplayId> {
        self.ctx.display.as_ref().map(|d| d.id)
    }

    /// Output-local bounds of a bar-surface rect on this panel's display.
    fn to_output(&self, rect: Bounds<Pixels>, cx: &App) -> Option<ModalAnchor> {
        let info = self.ctx.shell.display(self.tray_display()?, cx)?;
        Some(info.bar_to_output(rect, theme(cx)))
    }

    fn icon_anchor(&self, id: &str, cx: &App) -> Option<ModalAnchor> {
        self.to_output(self.tray.icons.get(id)?, cx)
    }

    fn chevron_anchor(&self, cx: &App) -> Option<ModalAnchor> {
        self.to_output(self.bounds.0.borrow().get("tray-chevron").copied()?, cx)
    }

    fn tray_item(&self, id: &str, cx: &App) -> Option<TrayItem> {
        self.ctx.services.tray.state.read(cx).item(id).cloned()
    }

    fn tray_click(&mut self, id: &str, button: Button, cx: &mut Context<Self>) {
        let Some(item) = self.tray_item(id, cx) else { return };
        // an item behind the chevron has no icon of its own: its menu grows from the chevron
        let anchor = self.icon_anchor(id, cx).or_else(|| self.chevron_anchor(cx));
        self.tray.ui.update(cx, |ui, cx| ui.click(&item, button, anchor, cx));
    }

    fn tray_button(&self, item: &TrayItem, open: bool, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let warn = t.warn;
        let id = item.id.clone();
        div()
            .id(ElementId::from(SharedString::from(format!("tray-{}", item.id))))
            .relative()
            .flex_none()
            .size(px(geom::TRAY_BOX))
            .flex()
            .items_center()
            .justify_center()
            .rounded(t.radius_control)
            .cursor_pointer()
            .child(open_layer(open, t))
            .when(!open, |d| d.child(hover_layer(t)))
            .child(app_icon(item.display_icon().map(|p| p.as_path()), geom::TRAY_ICON, cx))
            .when(item.status == Status::NeedsAttention, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(geom::TRAY_DOT_INSET))
                        .right(px(geom::TRAY_DOT_INSET))
                        .size(px(geom::TRAY_DOT))
                        .rounded(px(geom::TRAY_DOT / 2.))
                        .bg(warn),
                )
            })
            .child(self.tray.icons.track(&item.id))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, ev: &ClickEvent, _, cx| {
                    let button = if ev.is_right_click() { Button::Right } else { Button::Left };
                    this.tray_click(&id, button, cx);
                }
            }))
            .on_aux_click(cx.listener({
                let id = id.clone();
                move |this, ev: &ClickEvent, _, cx| {
                    if ev.is_middle_click() {
                        this.tray_click(&id, Button::Middle, cx);
                    } else if ev.is_right_click() {
                        this.tray_click(&id, Button::Right, cx);
                    }
                }
            }))
            .on_scroll(cx.listener({
                let id = id.clone();
                move |this, ev: &ScrollEvent, _, cx| {
                    this.tray.ui.update(cx, |ui, _| ui.scroll(&id, ev));
                }
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let anchor = this.icon_anchor(&id, cx);
                let display = this.tray_display();
                this.tray.ui.update(cx, |ui, cx| ui.hover(&id, anchor, display, *hovered, cx));
            }))
    }

    /// Room the tray area would take at `avail` (see [`area_width`]).
    pub(crate) fn tray_area_width(&self, avail: f32, cx: &App) -> f32 {
        let statuses: Vec<Status> = self.ctx.services.tray.state.read(cx).items.iter().map(|i| i.status).collect();
        area_width(&statuses, avail, self.has_now_playing(cx))
    }

    /// Is there anything for the tray area to show (tray items or an active player)?
    pub(crate) fn tray_present(&self, cx: &gpui::App) -> bool {
        // (what is still fading out keeps the area, so it does not vanish under its exit)
        !self.ctx.services.tray.state.read(cx).items.is_empty()
            || self.has_now_playing(cx)
            || !self.tray.presence.is_empty()
            || !self.tray.np.is_empty()
            || !self.tray.chev.is_empty()
    }

    /// The tray area, or `None` while there is nothing to show. `avail` is the room the
    /// status row leaves it (`geom::tray_avail`); the area is as wide as what it draws.
    pub(crate) fn tray_section(&mut self, t: &Theme, avail: f32, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let items = self.ctx.services.tray.state.read(cx).items.clone();
        // The now-playing control shares this area (and its width budget).
        let media = self.has_now_playing(cx);
        if items.is_empty() && !media && self.tray.presence.is_empty() && self.tray.np.is_empty() && self.tray.chev.is_empty() {
            self.bounds.forget("media", cx);
            return None;
        }
        let open_menu = self.ctx.shell.modal(cx).filter(|m| m.panel == ids::TRAY_MENU).and_then(|_| {
            self.ctx.services.tray.state.read(cx).menu.as_ref().map(|m| m.item.clone())
        });
        let flyout_open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == ids::TRAY_OVERFLOW);
        let statuses: Vec<Status> = items.iter().map(|i| i.status).collect();
        let row = plan_row(&statuses, avail, media);
        let plan = row.tray;
        *self.tray.hidden.borrow_mut() = plan.hidden.iter().map(|&i| items[i].id.clone()).collect();

        // What is drawn is kept for the length of its exit: an icon that went (or moved behind
        // the chevron) shrinks away, a new one grows in, the player control fades.
        let now = Instant::now();
        self.tray.presence.sync(now, plan.shown.iter().map(|&i| (items[i].id.clone(), items[i].clone())));
        let np_label = crate::nowplaying::now_playing_label(self.ctx.services.mpris.state.read(cx)).filter(|_| row.media != MediaFit::None);
        self.tray.np.sync(now, np_label.map(|(title, playing)| (0u8, (title, playing, row.media))));
        self.tray.chev.sync(now, (!plan.hidden.is_empty()).then_some((0u8, ())));
        // (once everything has gone one more frame lets the bar drop the empty area)
        self.tray.animating =
            self.tray.presence.is_animating(now)
                || self.tray.np.is_animating(now)
                || self.tray.chev.is_animating(now)
                || (items.is_empty() && !media);
        if !media {
            self.bounds.forget("media", cx);
        }
        let np_entries: Vec<_> = self.tray.np.entries(now).to_vec();
        let now_playing: Vec<AnyElement> = np_entries
            .into_iter()
            .filter_map(|e| {
                let v = e.presence.value(now);
                let el = if e.is_present() {
                    self.now_playing(row.media, t, cx).map(|el| el.into_any_element())
                } else {
                    let (title, playing, fit) = e.value;
                    Some(BarPanel::now_playing_ghost(title, playing, fit, t).into_any_element())
                };
                el.map(|el| div().flex_none().opacity(v).child(el).into_any_element())
            })
            .collect();
        let entries: Vec<_> = self.tray.presence.entries(now).to_vec();
        let slots: Vec<AnyElement> = entries
            .into_iter()
            .map(|e| {
                let (fade, grow) = (e.presence.value(now), e.presence.movement(now));
                let open = open_menu.as_deref() == Some(e.key.as_str());
                let button = self.tray_button(&e.value, open, t, cx);
                div()
                    .flex_none()
                    .h(px(geom::TRAY_BOX))
                    .w(px(geom::TRAY_BOX * grow))
                    .ml(px(geom::TRAY_GAP * grow))
                    .overflow_hidden()
                    .opacity(fade)
                    .child(button)
                    .into_any_element()
            })
            .collect();
        let buttons = (!slots.is_empty()).then(|| div().flex().flex_none().items_center().ml(px(-geom::TRAY_GAP)).children(slots));

        let chev_entries: Vec<_> = self.tray.chev.entries(now).to_vec();
        let chevrons: Vec<AnyElement> = chev_entries.into_iter().map(|e| {
            let (fade, grow) = (e.presence.value(now), e.presence.movement(now));
            let needs_attention = plan.hidden.iter().any(|&i| items[i].status == Status::NeedsAttention);
            let warn = t.warn;
            let hidden = self.tray.hidden.clone();
            let chevron = div()
                .id("tray-chevron")
                .relative()
                .flex_none()
                .w(px(geom::TRAY_CHEVRON_W))
                .h(px(geom::TRAY_BOX))
                .flex()
                .items_center()
                .justify_center()
                .rounded(t.radius_control)
                .cursor_pointer()
                .child(open_layer(flyout_open, t))
                .when(!flyout_open, |d| d.child(hover_layer(t)))
                .child(icon(Icon::ChevronUp, geom::TRAY_CHEVRON_ICON).text_color(t.fg_muted))
                .when(needs_attention, |d| {
                    d.child(div().absolute().top(px(geom::TRAY_DOT_INSET)).right(px(0.)).size(px(geom::TRAY_DOT)).rounded(px(geom::TRAY_DOT / 2.)).bg(warn))
                })
                .child(self.bounds.track("tray-chevron"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let anchor = this.chevron_anchor(cx);
                    let ids_hidden = hidden.borrow().clone();
                    this.tray.ui.update(cx, |ui, cx| {
                        ui.hide_tip(cx);
                        ui.hidden = ids_hidden;
                        ui.chevron = anchor;
                    });
                    this.ctx.shell.toggle_modal(ids::TRAY_OVERFLOW, anchor, cx);
                }));
            // grows in and shrinks away with the icons
            div().flex_none().h(px(geom::TRAY_BOX)).w(px(geom::TRAY_CHEVRON_W * grow)).overflow_hidden().opacity(fade).child(chevron).into_any_element()
        }).collect();

        let area = div()
            .id("bar-tray")
            .relative()
            // as wide as its content; it gives way (clips) only if the estimate was short
            .flex_shrink(1.0)
            .min_w_0()
            .overflow_hidden()
            .h(px(geom::TRAY_BOX))
            .flex()
            .items_center()
            .gap(px(geom::TRAY_GAP))
            .children(now_playing)
            .children(buttons)
            .children(chevrons);
        Some(area)
    }
}

// ── tooltip (passive overlay) ─────────────────────────────────────────────────

pub struct TrayTooltipPanel {
    ctx: PanelContext,
    ui: Entity<TrayUi>,
}

impl Panel for TrayTooltipPanel {
    const ID: PanelId = PanelId("tray-tooltip");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let ui = TrayUi::get(&ctx, cx);
        cx.observe(&ui, |_, _, cx| cx.notify()).detach();
        Self { ctx, ui }
    }

    fn is_live(&self, cx: &App) -> bool {
        let mine = self.ctx.display.as_ref().map(|d| d.id);
        self.ui.read(cx).tip.as_ref().is_some_and(|t| Some(t.display) == mine)
    }
}

impl Render for TrayTooltipPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().relative();
        let mine = self.ctx.display.as_ref().map(|d| d.id);
        let Some(tip) = self.ui.read(cx).tip.clone().filter(|t| Some(t.display) == mine) else { return root };
        let Some(item) = self.ctx.services.tray.state.read(cx).item(&tip.id).cloned() else { return root };
        let label = item.label().to_string();
        if label.is_empty() {
            return root;
        }
        let description = Some(item.tooltip.description.clone()).filter(|d| !d.is_empty()).map(SharedString::from);
        let t = theme(cx).clone();
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let bottom = f32::from(t.bar_h) + margin + geom::TRAY_TOOLTIP_GAP;
        // a wide row with the tooltip centred in it: centres on the icon without knowing
        // the tooltip's width
        const ROW_W: f32 = 480.0;
        let centre = f32::from(tip.anchor.center().x);
        let output_w = self.ctx.shell.display(tip.display, cx).map_or(1920.0, |d| f32::from(d.size().width));
        let left = (centre - ROW_W / 2.0).clamp(0.0, (output_w - ROW_W).max(0.0));
        let shift = centre - (left + ROW_W / 2.0);
        root.child(
            div()
                .absolute()
                .left(px(left))
                .bottom(px(bottom))
                .w(px(ROW_W))
                .flex()
                .justify_center()
                .child(div().ml(px(shift.max(0.) * 2.)).mr(px((-shift).max(0.) * 2.)).child(tooltip(label, description, cx))),
        )
    }
}

/// Elements shared by the menu and the flyout: a 14px leading slot.
pub(crate) fn lead_slot(content: Option<AnyElement>) -> Div {
    div().flex().flex_none().items_center().justify_center().size(px(geom::MENU_LEAD)).children(content)
}

/// Bottom offset of a popup growing out of the bar (as the calendar does).
pub(crate) fn popup_bottom(t: &Theme) -> f32 {
    let margin = frame::inset(t);
    f32::from(t.bar_h) + f32::from(margin) + metrics::POPUP_GAP
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Status = Status::Passive;
    const A: Status = Status::Active;
    const N: Status = Status::NeedsAttention;
    const PITCH: f32 = geom::TRAY_BOX + geom::TRAY_GAP;

    #[test]
    fn everything_active_fits_without_a_chevron() {
        let p = plan(&[A, A, A], 3.0 * PITCH);
        assert_eq!(p, Plan { shown: vec![0, 1, 2], hidden: vec![] });
        // a few pixels short: the chevron (which takes room itself) appears
        let p = plan(&[A, A, A], 3.0 * PITCH - geom::TRAY_GAP - 1.0);
        assert!(!p.hidden.is_empty());
    }

    #[test]
    fn passive_items_are_always_hidden() {
        assert_eq!(plan(&[P, A, P], 10_000.0), Plan { shown: vec![1], hidden: vec![0, 2] });
        assert_eq!(plan(&[P, P], 10_000.0), Plan { shown: vec![], hidden: vec![0, 1] });
        assert_eq!(plan(&[], 100.0), Plan { shown: vec![], hidden: vec![] });
    }

    #[test]
    fn attention_wins_when_space_runs_out() {
        // room for exactly one icon next to the chevron
        let w = geom::TRAY_CHEVRON_W + geom::TRAY_GAP + geom::TRAY_BOX;
        let p = plan(&[A, A, N, A], w);
        assert_eq!(p, Plan { shown: vec![2], hidden: vec![0, 1, 3] });
        // …and the others keep registration order
        let p = plan(&[A, A, N, A], w + PITCH);
        assert_eq!(p.shown, vec![0, 2]);
    }

    #[test]
    fn no_room_means_chevron_only() {
        assert_eq!(plan(&[A, N], 0.0), Plan { shown: vec![], hidden: vec![0, 1] });
        assert_eq!(plan(&[A], -50.0), Plan { shown: vec![], hidden: vec![0] });
    }

    #[test]
    fn shown_and_hidden_partition_the_items() {
        for w in [0.0, 17.0, 40.0, 90.0, 400.0] {
            let statuses = [A, P, N, A, P, A, A];
            let p = plan(&statuses, w);
            let mut all: Vec<usize> = p.shown.iter().chain(&p.hidden).copied().collect();
            all.sort_unstable();
            assert_eq!(all, (0..statuses.len()).collect::<Vec<_>>(), "width {w}");
            // what is drawn (icons + gaps + chevron) never exceeds the width, chevron aside
            let drawn = p.shown.len() as f32 * PITCH - if p.shown.is_empty() { 0.0 } else { geom::TRAY_GAP };
            let chevron = if p.hidden.is_empty() { 0.0 } else { geom::TRAY_CHEVRON_W + if p.shown.is_empty() { 0.0 } else { geom::TRAY_GAP } };
            assert!(p.shown.is_empty() || drawn + chevron <= w + 0.001, "width {w}: {drawn} + {chevron}");
        }
    }

    #[test]
    fn the_now_playing_title_shrinks_first_then_the_glyph_stays() {
        let glyph = geom::NOW_PLAYING_GLYPH_W + geom::TRAY_GAP;
        let title_room = geom::NOW_PLAYING_TITLE_GAP + geom::NOW_PLAYING_TITLE_MAX;
        // Plenty of room: full title, every icon.
        let r = plan_row(&[A, A], glyph + 2.0 * PITCH + title_room + 50.0, true);
        assert_eq!(r.media, MediaFit::Title(geom::NOW_PLAYING_TITLE_MAX));
        assert!(r.tray.hidden.is_empty());
        // Less room: the title is cut, the icons stay.
        let r = plan_row(&[A, A], glyph + 2.0 * PITCH + geom::NOW_PLAYING_TITLE_GAP + geom::NOW_PLAYING_TITLE_MIN + 5.0, true);
        assert!(matches!(r.media, MediaFit::Title(w) if w < geom::NOW_PLAYING_TITLE_MAX));
        assert!(r.tray.hidden.is_empty());
        // Even less: glyph only, still every icon.
        let r = plan_row(&[A, A], glyph + 2.0 * PITCH, true);
        assert_eq!(r.media, MediaFit::Glyph);
        assert!(r.tray.hidden.is_empty());
        // Then the icons go behind the chevron; the glyph stays.
        let r = plan_row(&[A, A], glyph + PITCH, true);
        assert_eq!(r.media, MediaFit::Glyph);
        assert!(!r.tray.hidden.is_empty());
        // No player: the old plan, untouched.
        assert_eq!(plan_row(&[A, A], 2.0 * PITCH, false), RowPlan { tray: plan(&[A, A], 2.0 * PITCH), media: MediaFit::None });
        // Only a player, no tray: the title takes the area.
        assert_eq!(plan_row(&[], 400.0, true).media, MediaFit::Title(geom::NOW_PLAYING_TITLE_MAX));
    }
}
