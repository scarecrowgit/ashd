//! Session / power menu with 60 % dim (det-09, det-13).
//!
//! Owner: quick settings. Contract: ARCHITECTURE.md "Panels" — `SessionPanel` is registered
//! by `ashd-shell` with `reg.register::<SessionPanel>()`.
//!
//! A 260 px menu that grows up from the bar's session button (placed from the modal
//! anchor): header `user@host` / uptime, then Lock, Suspend (only when logind says it can),
//! Log out, a separator, Reboot, Shut down. Arrows + Enter, or a single-letter accelerator
//! (shown as kbd chips: `l` lock, `s` suspend, `o` log out, `r` reboot, `d` shut down).
//! Reboot and Shut down turn the menu into the shared 10 s in-place countdown
//! (`ashd_panel_quick_settings::power`); press-and-hold 600 ms skips it. Under
//! `ASHD_FAKE_POWER=1` nothing is performed, only logged.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AppContext as _, Context, Div, ElementId, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};
use ashd_ipc::{PanelOp, Request, Response};
use ashd_panel_quick_settings::{
    identity::{Identity, format_uptime, read_proc_uptime},
    power::{HOLD_MS, PowerCountdown, PowerKind, run_logout},
};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_tokens::{LayoutVariant, metrics, motion};
use ashd_ui::{Icon, anim, hline, icon, label, theme};

/// Menu width (det-13).
pub const MENU_W: f32 = 260.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Lock,
    Suspend,
    Logout,
    Reboot,
    PowerOff,
}

impl Item {
    pub fn title(self) -> &'static str {
        match self {
            Item::Lock => "Lock",
            Item::Suspend => "Suspend",
            Item::Logout => "Log out",
            Item::Reboot => "Reboot",
            Item::PowerOff => "Shut down",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            Item::Lock => Icon::Lock,
            Item::Suspend => Icon::Moon,
            Item::Logout => Icon::Logout,
            Item::Reboot => Icon::Reboot,
            Item::PowerOff => Icon::Power,
        }
    }
    /// Single-letter accelerator.
    pub fn accelerator(self) -> &'static str {
        match self {
            Item::Lock => "l",
            Item::Suspend => "s",
            Item::Logout => "o",
            Item::Reboot => "r",
            Item::PowerOff => "d",
        }
    }
    fn power_kind(self) -> Option<PowerKind> {
        match self {
            Item::Reboot => Some(PowerKind::Reboot),
            Item::PowerOff => Some(PowerKind::PowerOff),
            _ => None,
        }
    }
}

/// The menu entries in order; Suspend is hidden when logind cannot suspend.
pub fn items(can_suspend: bool) -> Vec<Item> {
    [Item::Lock, Item::Suspend, Item::Logout, Item::Reboot, Item::PowerOff]
        .into_iter()
        .filter(|i| *i != Item::Suspend || can_suspend)
        .collect()
}

/// Selection after pressing up (`-1`) / down (`+1`), wrapping.
pub fn step_selection(selected: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as i32 + delta).rem_euclid(len as i32) as usize
}

pub fn item_for_key(items: &[Item], key: &str) -> Option<usize> {
    items.iter().position(|i| i.accelerator() == key)
}

pub struct SessionPanelState {
    focus: FocusHandle,
    selected: usize,
    identity: Identity,
    uptime_fallback: Option<Duration>,
    pub countdown: Entity<PowerCountdown>,
}

pub struct SessionPanel {
    pub ctx: PanelContext,
    pub state: SessionPanelState,
}

impl SessionPanel {
    fn items(&self, cx: &gpui::App) -> Vec<Item> {
        items(self.ctx.services.logind.state.read(cx).can_suspend)
    }

    fn activate(&mut self, item: Item, cx: &mut Context<Self>) {
        match item {
            Item::Lock => {
                if ashd_svc_logind::fake_power() {
                    tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: lock requested (nothing is done)");
                } else if let Some(shell) = self.ctx.shell.entity() {
                    ashd_shell_core::dispatch(&shell.downgrade(), &Request::Lock, cx);
                }
                self.ctx.shell.close_modal(cx);
            }
            Item::Suspend => {
                if ashd_svc_logind::fake_power() {
                    tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: suspend requested (nothing is done)");
                }
                self.ctx.services.logind.handle.suspend();
                self.ctx.shell.close_modal(cx);
            }
            Item::Logout => {
                run_logout(&self.ctx, cx);
                self.ctx.shell.close_modal(cx);
            }
            Item::Reboot | Item::PowerOff => {
                if let Some(kind) = item.power_kind() {
                    self.state.countdown.update(cx, |c, cx| c.begin(kind, cx));
                }
            }
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.state.countdown.read(cx).active() {
            return;
        }
        let items = self.items(cx);
        let m = &ev.keystroke.modifiers;
        match ev.keystroke.key.as_str() {
            "up" => self.state.selected = step_selection(self.state.selected, items.len(), -1),
            "down" => self.state.selected = step_selection(self.state.selected, items.len(), 1),
            "tab" => self.state.selected = step_selection(self.state.selected, items.len(), if m.shift { -1 } else { 1 }),
            "home" => self.state.selected = 0,
            "end" => self.state.selected = items.len().saturating_sub(1),
            "enter" | "space" => {
                if let Some(item) = items.get(self.state.selected).copied() {
                    self.activate(item, cx);
                }
            }
            key if !m.control && !m.alt && !m.platform => {
                if let Some(ix) = item_for_key(&items, key) {
                    self.state.selected = ix;
                    self.activate(items[ix], cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn row(&self, ix: usize, item: Item, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx).clone();
        let selected = self.state.selected == ix;
        let danger = item == Item::PowerOff;
        let ink = if danger { t.err } else { t.fg };
        let icon_ink = if danger { t.err } else if selected { t.fg } else { t.fg_muted };
        let kind = item.power_kind();
        let countdown = self.state.countdown.clone();
        let held = kind.and_then(|k| countdown.read(cx).holding().filter(|(hk, _)| *hk == k));
        let mut row = div()
            .id(ElementId::from(("session-item", ix as u32)))
            .relative()
            .h(px(36.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_l_2()
            .border_color(if selected { t.accent } else { gpui::transparent_black() })
            .when_selected(selected, t.surface_2)
            .text_color(ink)
            .cursor_pointer()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.state.selected != ix {
                    this.state.selected = ix;
                    cx.notify();
                }
            }))
            .child(icon(item.icon(), 15.).text_color(icon_ink))
            .child(
                div()
                    .flex_1()
                    .font_family(t.font_sans.clone())
                    .text_size(px(metrics::type_scale::BODY.0))
                    .child(item.title()),
            )
            .child(kbd(item.accelerator(), cx));
        if let Some((_, gen_)) = held {
            row = row.child(
                div().absolute().left_0().bottom_0().h(px(2.)).bg(if danger { t.err } else { t.accent }).with_animation(
                    SharedString::from(format!("session-hold-{gen_}")),
                    Animation::new(Duration::from_millis(HOLD_MS)),
                    |el, d| el.w(relative(d)),
                ),
            );
        }
        match kind {
            Some(kind) => {
                let (down, up, up_out, click) = (countdown.clone(), countdown.clone(), countdown.clone(), countdown);
                row.on_mouse_down(MouseButton::Left, move |_, _, cx| down.update(cx, |c, cx| c.press(kind, cx)))
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| up.update(cx, |c, cx| c.release(cx)))
                    .on_mouse_up_out(MouseButton::Left, move |_, _, cx| up_out.update(cx, |c, cx| c.release(cx)))
                    .on_click(move |_, _, cx| click.update(cx, |c, cx| c.click(kind, cx)))
            }
            None => row.on_click(cx.listener(move |this, _, _, cx| this.activate(item, cx))),
        }
    }
}

/// Small helper trait so the selected background reads naturally in the builder chain.
trait WhenSelected {
    fn when_selected(self, selected: bool, bg: gpui::Hsla) -> Self;
}

impl WhenSelected for Stateful<Div> {
    fn when_selected(self, selected: bool, bg: gpui::Hsla) -> Self {
        if selected { self.bg(bg) } else { self }
    }
}

/// kbd chip (det-13): 20 high, mono 10.5, 1 px line border with a 2 px bottom border.
fn kbd(text: &'static str, cx: &gpui::App) -> Div {
    let t = theme(cx);
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .border_1()
        .border_b_2()
        .border_color(t.line)
        .rounded(t.radius_control)
        .font_family(t.font_mono.clone())
        .text_size(px(10.5))
        .text_color(t.fg)
        .child(text)
}

impl Panel for SessionPanel {
    const ID: PanelId = PanelId("session");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: true });
    const ANCHORS: &'static [&'static str] = &[ashd_shell_core::anchors::names::SESSION];

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.logind.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.sysinfo.state, |_, _, cx| cx.notify()).detach();
        let countdown = cx.new(|cx| PowerCountdown::new(ctx.clone(), true, cx));
        cx.observe(&countdown, |_, _, cx| cx.notify()).detach();
        let state = SessionPanelState {
            focus: cx.focus_handle(),
            selected: 0,
            identity: Identity::current(),
            uptime_fallback: None,
            countdown,
        };
        Self { ctx, state }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Session { op } = request else { return None };
        let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
        match (op, open) {
            (PanelOp::Open, _) | (PanelOp::Toggle, false) => self.ctx.shell.open_modal(Self::ID, None, cx),
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.focus.focus(window, cx);
        self.state.selected = 0;
        self.state.identity = Identity::current();
        let task = cx.background_executor().spawn(async { read_proc_uptime() });
        cx.spawn(async move |this, cx| {
            let up = task.await;
            this.update(cx, |this, cx| {
                this.state.uptime_fallback = up;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.state.countdown.update(cx, |c, cx| c.reset(cx));
        self.state.selected = 0;
    }
}

impl Render for SessionPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let sysinfo_uptime = self.ctx.services.sysinfo.state.read(cx).uptime;
        let up = if sysinfo_uptime > Duration::ZERO { Some(sysinfo_uptime) } else { self.state.uptime_fallback };

        // Grows up from the bar: right edge on the button's right edge, bottom above the bar.
        let anchor = self.ctx.shell.modal_anchor(Self::ID, cx);
        let output_w: f32 = self.ctx.shell.modal_display(cx).map(|d| d.size().width.into()).unwrap_or(1920.0);
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let bottom = f32::from(t.bar_h) + margin + metrics::POPUP_GAP;
        let right_edge = anchor.map(|a| f32::from(a.origin.x + a.size.width)).unwrap_or(output_w - 16.0 - margin);
        let left = (right_edge - MENU_W).clamp(margin, (output_w - MENU_W - margin).max(margin));

        let countdown = self.state.countdown.clone();
        let body = if countdown.read(cx).active() {
            div().p(px(12.)).child(countdown)
        } else {
            let items = self.items(cx);
            let mut col = div().p(px(4.)).flex().flex_col().child(
                div()
                    .px(px(12.))
                    .pt(px(8.))
                    .pb(px(6.))
                    .flex()
                    .justify_between()
                    .child(label(self.state.identity.label(), cx))
                    .child(label(up.map(format_uptime).unwrap_or_default(), cx)),
            );
            for (ix, item) in items.iter().copied().enumerate() {
                if item == Item::Reboot {
                    col = col.child(div().py(px(4.)).child(hline(cx)));
                }
                col = col.child(self.row(ix, item, cx));
            }
            col
        };

        let panel = ashd_ui::overlay_panel(cx)
            .occlude()
            .track_focus(&self.state.focus)
            .key_context("SessionMenu")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.on_key(ev, cx)))
            .absolute()
            .left(px(left))
            .bottom(px(bottom))
            .w(px(MENU_W))
            .text_color(t.fg)
            .child(body)
            .with_animation(
                "session-in",
                anim::tween(motion::timing::SUBMENU_IN, anim::EASE_STANDARD),
                |el, d| el.opacity(d),
            );
        div().size_full().relative().child(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_is_hidden_when_logind_cannot_suspend() {
        let with = items(true);
        let without = items(false);
        assert_eq!(with, [Item::Lock, Item::Suspend, Item::Logout, Item::Reboot, Item::PowerOff]);
        assert_eq!(without, [Item::Lock, Item::Logout, Item::Reboot, Item::PowerOff]);
    }

    #[test]
    fn selection_wraps() {
        assert_eq!(step_selection(0, 4, -1), 3);
        assert_eq!(step_selection(3, 4, 1), 0);
        assert_eq!(step_selection(1, 4, 1), 2);
        assert_eq!(step_selection(0, 0, 1), 0);
    }

    #[test]
    fn accelerators_are_unique_and_resolve_against_the_visible_items() {
        let all = items(true);
        let mut keys: Vec<_> = all.iter().map(|i| i.accelerator()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), all.len());
        // a hidden entry's accelerator does nothing
        assert_eq!(item_for_key(&items(false), "s"), None);
        assert_eq!(item_for_key(&items(false), "d"), Some(3));
        assert_eq!(item_for_key(&items(true), "o"), Some(2));
    }

    #[test]
    fn only_reboot_and_shutdown_go_through_the_countdown() {
        assert_eq!(Item::Reboot.power_kind(), Some(PowerKind::Reboot));
        assert_eq!(Item::PowerOff.power_kind(), Some(PowerKind::PowerOff));
        assert_eq!(Item::Lock.power_kind(), None);
        assert_eq!(Item::Logout.power_kind(), None);
    }
}
