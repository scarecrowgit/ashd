//! Critical-battery dialog (det-09).
//!
//! Owner: polkit. Contract: ARCHITECTURE.md "Panels" — `BatteryPanel` is registered by
//! `ashd-shell` with `reg.register::<BatteryPanel>()`.
//!
//! Driven by the sysinfo service's battery sample (`/sys/class/power_supply`, every
//! sysinfo tick): when the system battery is discharging at or below
//! [`CRITICAL_PERCENT`], a 360 px card with an `err` border opens (60 % scrim): "Battery
//! N% — sleeping in 60 s", "Sleep now" and, when logind can, "Hibernate". Plugging in a
//! charger closes it and cancels the countdown; Escape / dismissing closes it and skips
//! the automatic sleep for this discharge (re-armed after the next charge). When the
//! countdown ends the session suspends through logind (logged only under
//! `ASHD_FAKE_POWER=1`). Hosts without a battery never see it.

use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_sysinfo::{BatteryInfo, BatteryStatus};
use ashd_tokens::{metrics, motion};
use ashd_ui::{ButtonKind, Icon, anim, button_small, icon, theme};

/// Discharging at or below this opens the dialog.
pub const CRITICAL_PERCENT: u8 = 5;
/// Countdown before the automatic sleep.
pub const GRACE: Duration = Duration::from_secs(60);
/// After an automatic sleep, wait this long before counting down again.
pub const COOLDOWN: Duration = Duration::from_secs(120);
/// Card width (det-09).
pub const CARD_W: f32 = 360.0;

/// The countdown state machine (no GPUI; unit-tested).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    /// Counting down to an automatic sleep.
    pub deadline: Option<Instant>,
    /// Dismissed during this discharge.
    pub dismissed: bool,
    /// The last battery level seen while critical.
    pub percent: Option<u8>,
    cooldown_until: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Nothing,
    /// The countdown ran out: suspend now.
    Sleep,
}

pub fn is_critical(b: Option<&BatteryInfo>) -> bool {
    b.is_some_and(|b| b.status == BatteryStatus::Discharging && b.percent <= CRITICAL_PERCENT)
}

impl Watch {
    /// A new battery sample.
    pub fn update(&mut self, battery: Option<&BatteryInfo>, now: Instant) {
        if !is_critical(battery) {
            // Charging, full, healthy or no battery: everything is reset.
            if battery.is_none_or(|b| b.status != BatteryStatus::Discharging) {
                self.dismissed = false;
                self.cooldown_until = None;
            }
            self.deadline = None;
            self.percent = None;
            return;
        }
        self.percent = battery.map(|b| b.percent);
        let cooling = self.cooldown_until.is_some_and(|c| now < c);
        if self.deadline.is_none() && !self.dismissed && !cooling {
            self.deadline = Some(now + GRACE);
        }
    }

    /// Advance the clock.
    pub fn tick(&mut self, now: Instant) -> Tick {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                self.cooldown_until = Some(now + COOLDOWN);
                Tick::Sleep
            }
            _ => Tick::Nothing,
        }
    }

    /// The user closed the dialog: no automatic sleep during this discharge.
    pub fn dismiss(&mut self) {
        self.deadline = None;
        self.dismissed = true;
    }

    /// The dialog should be on screen.
    pub fn visible(&self) -> bool {
        self.deadline.is_some()
    }

    /// Whole seconds left (rounded up).
    pub fn remaining(&self, now: Instant) -> u64 {
        self.deadline.map(|d| d.saturating_duration_since(now).as_secs_f64().ceil() as u64).unwrap_or(0)
    }
}

/// Panel state (fm rule: everything this region needs lives here).
#[derive(Default)]
pub struct BatteryPanelState {
    pub watch: Watch,
    ticking: bool,
}

pub struct BatteryPanel {
    pub ctx: PanelContext,
    pub state: BatteryPanelState,
}

impl BatteryPanel {
    fn is_open(&self, cx: &App) -> bool {
        self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID)
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let battery = self.ctx.services.sysinfo.state.read(cx).battery.clone();
        self.state.watch.update(battery.as_ref(), Instant::now());
        self.apply(cx);
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let open = self.is_open(cx);
        if self.state.watch.visible() {
            if !open {
                self.ctx.shell.open_modal(Self::ID, None, cx);
            }
            self.start_ticking(cx);
        } else if open {
            self.ctx.shell.close_modal(cx);
        }
        cx.notify();
    }

    /// 1 s timer while a countdown runs (re-render + automatic sleep).
    fn start_ticking(&mut self, cx: &mut Context<Self>) {
        if self.state.ticking {
            return;
        }
        self.state.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let go_on = this
                    .update(cx, |this, cx| {
                        if this.state.watch.tick(Instant::now()) == Tick::Sleep {
                            this.suspend(cx);
                        }
                        cx.notify();
                        let go_on = this.state.watch.visible();
                        this.state.ticking = go_on;
                        go_on
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
    }

    fn suspend(&mut self, cx: &mut Context<Self>) {
        if ashd_svc_logind::fake_power() {
            tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: critical battery suspend (nothing is done)");
        }
        self.ctx.services.logind.handle.suspend();
        self.state.watch.deadline = None;
        if self.is_open(cx) {
            self.ctx.shell.close_modal(cx);
        }
    }

    fn hibernate(&mut self, cx: &mut Context<Self>) {
        self.ctx.services.logind.handle.hibernate();
        self.state.watch.dismiss();
        if self.is_open(cx) {
            self.ctx.shell.close_modal(cx);
        }
    }
}

impl Panel for BatteryPanel {
    const ID: PanelId = PanelId("battery");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: true });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.sysinfo.state, |this, _, cx| this.sync(cx)).detach();
        cx.observe(&ctx.services.logind.state, |_, _, cx| cx.notify()).detach();
        Self { ctx, state: BatteryPanelState::default() }
    }

    fn on_close(&mut self, _cx: &mut Context<Self>) {
        // Escape / `ashd dismiss` / another modal: skip the automatic sleep this time.
        if self.state.watch.visible() {
            self.state.watch.dismiss();
        }
    }
}

impl Render for BatteryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let root = div().size_full().flex().items_center().justify_center();
        if !self.state.watch.visible() {
            return root;
        }
        let logind = self.ctx.services.logind.state.read(cx).clone();
        let percent = self.state.watch.percent.unwrap_or(0);
        let remaining = self.state.watch.remaining(Instant::now());
        let title = if logind.can_suspend || !logind.available {
            format!("Battery {percent}% — sleeping in {remaining} s")
        } else {
            format!("Battery {percent}% — plug in a charger")
        };
        let mut actions = div().flex().gap(px(8.)).child(
            button_small("battery-sleep", "Sleep now", ButtonKind::Secondary, cx)
                .on_click(cx.listener(|this, _, _, cx| this.suspend(cx))),
        );
        if logind.can_hibernate {
            actions = actions.child(
                button_small("battery-hibernate", "Hibernate", ButtonKind::Ghost, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.hibernate(cx))),
            );
        }
        let card = ashd_ui::overlay_panel(cx)
            .id("battery-card")
            .occlude()
            .w(px(CARD_W))
            .border_color(t.err)
            .text_color(t.fg)
            .font_family(t.font_sans.clone())
            .child(
                div()
                    .p(px(20.))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(12.))
                    .child(icon(Icon::battery(f32::from(percent) / 100.0, false), metrics::icon_size::EMPTY).text_color(t.err))
                    .child(
                        div()
                            .text_size(px(metrics::type_scale::TITLE.0))
                            .line_height(px(metrics::type_scale::TITLE.1))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(metrics::type_scale::UI.0))
                            .text_color(t.fg_muted)
                            .child("Your work will be saved. Plug in a charger to cancel."),
                    )
                    .child(actions),
            )
            .with_animation("battery-in", anim::tween(motion::DUR_FAST, anim::EASE_STANDARD), |el, d| el.opacity(d));
        root.child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bat(percent: u8, status: BatteryStatus) -> Option<BatteryInfo> {
        Some(BatteryInfo { percent, status })
    }

    #[test]
    fn counts_down_only_when_discharging_and_critical() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        w.update(None, t0);
        assert!(!w.visible());
        w.update(bat(30, BatteryStatus::Discharging).as_ref(), t0);
        assert!(!w.visible());
        w.update(bat(4, BatteryStatus::Charging).as_ref(), t0);
        assert!(!w.visible());
        w.update(bat(5, BatteryStatus::Discharging).as_ref(), t0);
        assert!(w.visible());
        assert_eq!(w.remaining(t0), 60);
        assert_eq!(w.percent, Some(5));
        // a later sample keeps the deadline
        w.update(bat(4, BatteryStatus::Discharging).as_ref(), t0 + Duration::from_secs(10));
        assert_eq!(w.remaining(t0 + Duration::from_secs(10)), 50);
        assert_eq!(w.tick(t0 + Duration::from_secs(59)), Tick::Nothing);
        assert_eq!(w.tick(t0 + Duration::from_secs(60)), Tick::Sleep);
        assert!(!w.visible());
        // cooldown after the sleep, then it counts down again
        w.update(bat(3, BatteryStatus::Discharging).as_ref(), t0 + Duration::from_secs(61));
        assert!(!w.visible());
        w.update(bat(3, BatteryStatus::Discharging).as_ref(), t0 + Duration::from_secs(61) + COOLDOWN);
        assert!(w.visible());
    }

    #[test]
    fn plugging_in_cancels_and_dismiss_lasts_until_charging() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        w.update(bat(3, BatteryStatus::Discharging).as_ref(), t0);
        assert!(w.visible());
        w.update(bat(3, BatteryStatus::Charging).as_ref(), t0);
        assert!(!w.visible());
        assert_eq!(w.tick(t0 + GRACE), Tick::Nothing);

        w.update(bat(3, BatteryStatus::Discharging).as_ref(), t0);
        w.dismiss();
        assert!(!w.visible());
        w.update(bat(2, BatteryStatus::Discharging).as_ref(), t0);
        assert!(!w.visible(), "dismissed for this discharge");
        w.update(bat(2, BatteryStatus::Charging).as_ref(), t0);
        w.update(bat(2, BatteryStatus::Discharging).as_ref(), t0);
        assert!(w.visible(), "re-armed after charging");
    }
}
