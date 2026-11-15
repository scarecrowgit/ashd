//! The reboot / shut-down countdown, shared by the quick-settings footer and the session
//! menu (det-13): a 10 s in-place countdown that replaces the row it was started from,
//! plus press-and-hold (600 ms) on the buttons to skip it. Never a modal dialog.
//!
//! - [`Machine`] is the pure state machine (unit-tested).
//! - [`PowerCountdown`] is the GPUI entity around it: ticker, hold timer, execution
//!   (`logind.reboot()` / `power_off()`, or a log line under `ASHD_FAKE_POWER=1`), and the
//!   view of the countdown block ([`PowerCountdown::render`]).
//! - [`session_button`] / [`power_button`] draw the 40×40 buttons with their
//!   hover / pressed / focus / disabled states.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, Context, Div, ElementId, Entity, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Stateful,
    Styled as _, Transformation, Window, div, percentage, prelude::FluentBuilder as _, px, relative,
};
use ashd_niri::niri_ipc::Action;
use ashd_shell_core::PanelContext;
use ashd_svc_logind::Inhibitor;
use ashd_tokens::metrics;
use ashd_ui::{Icon, hover_ring, hover_wash, icon, label, theme};

/// Length of the countdown.
pub const COUNTDOWN_SECS: u32 = 10;
/// How long a button must be held to skip the countdown.
pub const HOLD_MS: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerKind {
    Reboot,
    PowerOff,
}

impl PowerKind {
    /// "Reboot" / "Shut down".
    pub fn verb(self) -> &'static str {
        match self {
            PowerKind::Reboot => "Reboot",
            PowerKind::PowerOff => "Shut down",
        }
    }
    /// "Rebooting…" / "Shutting down…".
    pub fn progressive(self) -> &'static str {
        match self {
            PowerKind::Reboot => "Rebooting…",
            PowerKind::PowerOff => "Shutting down…",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            PowerKind::Reboot => Icon::Reboot,
            PowerKind::PowerOff => Icon::Power,
        }
    }
}

// ── pure state machine ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Counting {
        kind: PowerKind,
        left: u32,
    },
    /// The action was fired; the row shows "Rebooting…".
    Done(PowerKind),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    pub phase: Phase,
    /// Bumped on every transition so a stale ticker notices it was superseded.
    pub generation: u64,
}

impl Machine {
    /// Start a countdown. Only from `Idle`.
    pub fn start(&mut self, kind: PowerKind) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Counting { kind, left: COUNTDOWN_SECS };
        self.generation += 1;
        true
    }

    /// One second passed. `Some(kind)` when it reached zero and the action must run now.
    pub fn tick(&mut self) -> Option<PowerKind> {
        let Phase::Counting { kind, left } = self.phase else { return None };
        if left <= 1 {
            self.phase = Phase::Done(kind);
            self.generation += 1;
            Some(kind)
        } else {
            self.phase = Phase::Counting { kind, left: left - 1 };
            None
        }
    }

    /// "{verb} now" / the hold shortcut: run the action immediately (also from `Idle`).
    pub fn fire_now(&mut self, kind: PowerKind) -> Option<PowerKind> {
        match self.phase {
            Phase::Done(_) => None,
            Phase::Idle | Phase::Counting { .. } => {
                let kind = if let Phase::Counting { kind, .. } = self.phase { kind } else { kind };
                self.phase = Phase::Done(kind);
                self.generation += 1;
                Some(kind)
            }
        }
    }

    pub fn cancel(&mut self) {
        if self.phase != Phase::Idle {
            self.phase = Phase::Idle;
            self.generation += 1;
        }
    }

    pub fn is_active(&self) -> bool {
        self.phase != Phase::Idle
    }
}

/// The note under the countdown bar: windows that will close and what blocks the action.
pub fn note_text(windows: usize, inhibitors: &[Inhibitor]) -> String {
    let mut parts = Vec::new();
    parts.push(match windows {
        0 => "no open windows".to_string(),
        1 => "1 window will close".to_string(),
        n => format!("{n} windows will close"),
    });
    let blockers: Vec<&Inhibitor> = inhibitors.iter().filter(|i| i.blocks() && i.affects("shutdown")).collect();
    if let Some(first) = blockers.first() {
        let mut s = if first.why.is_empty() { format!("{} blocks it", first.who) } else { format!("{}: {}", first.who, first.why) };
        if blockers.len() > 1 {
            s.push_str(&format!(" (+{} more)", blockers.len() - 1));
        }
        parts.push(s);
    }
    parts.push("esc cancels".into());
    parts.join(" · ")
}

// ── the entity ────────────────────────────────────────────────────────────────

/// Press-and-hold tracking (pure): a press arms a 600 ms timer; if the same press is still
/// held when it expires, the action fires and the click that follows the release is
/// swallowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    /// Button currently held down, and the generation of that press.
    pub held: Option<(PowerKind, u64)>,
    gen_: u64,
    suppress_click: bool,
}

impl Hold {
    /// Mouse down; returns the press generation the timer must present to [`Hold::expire`].
    pub fn press(&mut self, kind: PowerKind) -> u64 {
        self.gen_ += 1;
        self.suppress_click = false;
        self.held = Some((kind, self.gen_));
        self.gen_
    }

    /// Mouse up. `true` when a press was being tracked.
    pub fn release(&mut self) -> bool {
        self.held.take().is_some()
    }

    /// The timer of press `gen_` elapsed: `true` if that press is still held (fire now).
    pub fn expire(&mut self, kind: PowerKind, gen_: u64) -> bool {
        if self.held == Some((kind, gen_)) {
            self.held = None;
            self.suppress_click = true;
            true
        } else {
            false
        }
    }

    /// A click arrived: `false` when it merely ends a hold that already fired.
    pub fn click_allowed(&mut self) -> bool {
        !std::mem::take(&mut self.suppress_click)
    }

    pub fn reset(&mut self) {
        *self = Self { gen_: self.gen_, ..Self::default() };
    }
}

pub struct PowerCountdown {
    ctx: PanelContext,
    machine: Machine,
    hold: Hold,
    /// Narrow layout (session menu, 260 px): buttons on their own row.
    compact: bool,
}

impl PowerCountdown {
    pub fn new(ctx: PanelContext, compact: bool, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.niri, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.logind.state, |_, _, cx| cx.notify()).detach();
        Self { ctx, machine: Machine::default(), hold: Hold::default(), compact }
    }

    /// A countdown (or its "Rebooting…" tail) is on screen: the host replaces its row.
    pub fn active(&self) -> bool {
        self.machine.is_active()
    }

    pub fn phase(&self) -> Phase {
        self.machine.phase
    }

    /// The button being held, if any (drives the 2 px fill).
    pub fn holding(&self) -> Option<(PowerKind, u64)> {
        self.hold.held
    }

    /// Start the countdown for `kind` (a click, or Enter in the session menu).
    pub fn begin(&mut self, kind: PowerKind, cx: &mut Context<Self>) {
        if !self.machine.start(kind) {
            return;
        }
        self.ctx.services.logind.handle.refresh_inhibitors();
        self.spawn_ticker(cx);
        cx.notify();
    }

    /// A click on the button; a click that ended a completed hold is swallowed.
    pub fn click(&mut self, kind: PowerKind, cx: &mut Context<Self>) {
        if self.hold.click_allowed() {
            self.begin(kind, cx);
        }
    }

    /// Mouse down on a power button: start the 600 ms hold timer.
    pub fn press(&mut self, kind: PowerKind, cx: &mut Context<Self>) {
        if self.machine.is_active() {
            return;
        }
        let gen_ = self.hold.press(kind);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(HOLD_MS)).await;
            this.update(cx, |this, cx| {
                if this.hold.expire(kind, gen_) {
                    this.fire(kind, cx);
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Mouse up (inside or outside): the hold is over.
    pub fn release(&mut self, cx: &mut Context<Self>) {
        if self.hold.release() {
            cx.notify();
        }
    }

    /// Abort the countdown (Cancel, Escape, the panel closing).
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.hold.reset();
        self.machine.cancel();
        cx.notify();
    }

    /// Back to the starting row without side effects (panel closed).
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    fn fire(&mut self, kind: PowerKind, cx: &mut Context<Self>) {
        if let Some(kind) = self.machine.fire_now(kind) {
            run_power(&self.ctx, kind);
        }
        cx.notify();
    }

    fn spawn_ticker(&mut self, cx: &mut Context<Self>) {
        let gen_ = self.machine.generation;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.machine.generation != gen_ {
                            return false;
                        }
                        if let Some(kind) = this.machine.tick() {
                            run_power(&this.ctx, kind);
                            cx.notify();
                            return false;
                        }
                        // The generation only changes on transitions; ticks keep it.
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }
}

/// Execute a power action: logind (which logs instead under `ASHD_FAKE_POWER=1`).
pub fn run_power(ctx: &PanelContext, kind: PowerKind) {
    if ashd_svc_logind::fake_power() {
        tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: {} requested (nothing is done)", kind.verb());
    }
    match kind {
        PowerKind::Reboot => ctx.services.logind.handle.reboot(),
        PowerKind::PowerOff => ctx.services.logind.handle.power_off(),
    }
}

/// Log out of the session: niri's `Quit` (no confirmation) ends the compositor and with
/// it the session; under `ASHD_FAKE_POWER=1` it only logs.
pub fn run_logout(ctx: &PanelContext, cx: &gpui::App) {
    if ashd_svc_logind::fake_power() {
        tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: log out requested (nothing is done)");
        return;
    }
    ctx.shell.niri_action(Action::Quit { skip_confirmation: true }, cx);
}

impl Render for PowerCountdown {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (kind, left, done) = match self.machine.phase {
            Phase::Idle => return div().into_any_element(),
            Phase::Counting { kind, left } => (kind, left, false),
            Phase::Done(kind) => (kind, 0, true),
        };
        let tint = if kind == PowerKind::PowerOff { t.err } else { t.accent };

        if done {
            // "Rebooting…" with a spinner (mockup `isDone`).
            return div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    icon(Icon::Spinner, 12.)
                        .text_color(t.accent)
                        .with_animation(
                            "power-spin",
                            Animation::new(Duration::from_millis(800)).repeat(),
                            |el, d| el.with_transformation(Transformation::rotate(percentage(d))),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .font_family(t.font_sans.clone())
                        .text_size(px(metrics::type_scale::BODY.0))
                        .text_color(t.fg)
                        .child(kind.progressive()),
                )
                .into_any_element();
        }

        let windows = self.ctx.niri.read(cx).windows.len();
        let inhibitors = self.ctx.services.logind.state.read(cx).inhibitors.clone();
        let note = note_text(windows, &inhibitors);
        let entity = cx.entity();

        let button = |id: &'static str, text: SharedString, primary: bool| -> Stateful<Div> {
            let mut b = div()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .border_1()
                .rounded(t.radius_control)
                .font_family(t.font_sans.clone())
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer();
            // The hover fill is a layer under the label, eased in over 80 ms.
            let r = (t.radius_control - px(1.)).max(px(0.));
            if primary {
                b = b
                    .border_color(tint)
                    .bg(tint)
                    .text_color(t.on_accent)
                    .child(hover_wash(ashd_ui::mix(tint, gpui::black(), 0.12), None, r));
            } else {
                b = b.border_color(t.line).text_color(t.fg).child(hover_wash(t.surface_2, None, r));
            }
            b.child(text)
        };
        let cancel = {
            let e = entity.clone();
            button("power-cancel", "Cancel".into(), false).on_click(move |_, _, cx| e.update(cx, |this, cx| this.cancel(cx)))
        };
        let now = {
            let e = entity.clone();
            button("power-now", format!("{} now", kind.verb()).into(), true)
                .on_click(move |_, _, cx| e.update(cx, |this, cx| this.fire(kind, cx)))
        };

        let headline = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex().text_color(tint).child(icon(kind.icon(), 16.).text_color(tint)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_baseline()
                    .gap(px(4.))
                    .font_family(t.font_sans.clone())
                    .text_size(px(metrics::type_scale::BODY.0))
                    .text_color(t.fg)
                    .child(format!("{} in", kind.verb()))
                    .child(div().font_family(t.font_mono.clone()).text_color(tint).child(format!("{left} s"))),
            );

        // The bar drains over one second per tick (`transition: width 1s linear`).
        let from = (left + 1).min(COUNTDOWN_SECS) as f32 / COUNTDOWN_SECS as f32;
        let to = left as f32 / COUNTDOWN_SECS as f32;
        let bar = div().h(px(2.)).w_full().bg(t.line).child(
            div().h(px(2.)).bg(tint).with_animation(
                SharedString::from(format!("power-bar-{left}")),
                Animation::new(Duration::from_secs(1)),
                move |el, d| el.w(relative(from + (to - from) * d)),
            ),
        );

        let mut col = div().flex().flex_col().gap(px(10.));
        if self.compact {
            col = col
                .child(headline)
                .child(div().flex().justify_end().gap(px(8.)).child(cancel).child(now));
        } else {
            col = col.child(headline.child(cancel).child(now));
        }
        col.child(bar).child(label(note, cx)).into_any_element()
    }
}

// ── buttons ───────────────────────────────────────────────────────────────────

/// A 40×40 session button (17 px icon, 1 px line border) with the documented states:
/// default transparent, hover `surface-2` (border takes `tint`, the err colour of Shut down), pressed `line` + 1 px down, focus 1 px `fg`
/// ring, disabled 40 % opacity. Add `.on_click(..)` / key handling yourself.
pub fn session_button(
    id: impl Into<ElementId>,
    ic: Icon,
    tint: Option<gpui::Hsla>,
    disabled: bool,
    focus: Option<&FocusHandle>,
    cx: &gpui::App,
) -> Stateful<Div> {
    let t = theme(cx);
    let (line, s2, fg) = (t.line, t.surface_2, t.fg);
    let b = div()
        .id(id)
        .relative()
        .size(px(metrics::SESSION_BUTTON))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .border_1()
        .border_color(line)
        .rounded(t.radius_control)
        .text_color(fg);
    if disabled {
        b.child(icon(ic, metrics::SESSION_ICON).text_color(fg)).opacity(0.4)
    } else {
        let b = match focus {
            Some(h) => b.track_focus(h).tab_index(0),
            None => b,
        };
        // Hover: `surface-2` (+ the tint border of Shut down), pressed: `line`; both ease in.
        let inner = (t.radius_control - px(1.)).max(px(0.));
        b.cursor_pointer()
            .child(hover_wash(s2, Some(line), inner))
            .when_some(tint, |d, c| d.child(hover_ring(c, px(1.), t.radius_control)))
            .child(icon(ic, metrics::SESSION_ICON).text_color(fg))
            .active(move |s| s.top(px(1.)))
            .focus_visible(move |s| s.border_color(fg))
    }
}

/// [`session_button`] for Reboot / Shut down: click starts the countdown, press-and-hold
/// for 600 ms skips it (a 2 px bar fills along the bottom while held).
pub fn power_button(
    id: impl Into<ElementId>,
    kind: PowerKind,
    disabled: bool,
    focus: Option<&FocusHandle>,
    countdown: &Entity<PowerCountdown>,
    cx: &gpui::App,
) -> Stateful<Div> {
    let t = theme(cx).clone();
    let tint = (kind == PowerKind::PowerOff).then_some(t.err);
    let bar_colour = tint.unwrap_or(t.accent);
    let held = countdown.read(cx).holding();
    let mut b = session_button(id, kind.icon(), tint, disabled, focus, cx);
    if let Some((k, gen_)) = held.filter(|(k, _)| *k == kind) {
        let _ = k;
        b = b.child(
            div().absolute().left_0().bottom_0().h(px(2.)).bg(bar_colour).with_animation(
                SharedString::from(format!("hold-{gen_}")),
                Animation::new(Duration::from_millis(HOLD_MS)),
                |el, d| el.w(relative(d)),
            ),
        );
    }
    if disabled {
        return b;
    }
    let (down, up, up_out, click, key) =
        (countdown.clone(), countdown.clone(), countdown.clone(), countdown.clone(), countdown.clone());
    b.on_mouse_down(MouseButton::Left, move |_, _, cx| down.update(cx, |c, cx| c.press(kind, cx)))
        .on_mouse_up(MouseButton::Left, move |_, _, cx| up.update(cx, |c, cx| c.release(cx)))
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| up_out.update(cx, |c, cx| c.release(cx)))
        .on_click(move |_, _, cx| click.update(cx, |c, cx| c.click(kind, cx)))
        .on_key_down(move |ev, _, cx| {
            if matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                key.update(cx, |c, cx| c.begin(kind, cx));
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_runs_ten_seconds_then_fires() {
        let mut m = Machine::default();
        assert!(m.start(PowerKind::Reboot));
        assert!(!m.start(PowerKind::PowerOff), "one at a time");
        for left in (1..COUNTDOWN_SECS).rev() {
            assert_eq!(m.tick(), None);
            assert_eq!(m.phase, Phase::Counting { kind: PowerKind::Reboot, left });
        }
        assert_eq!(m.tick(), Some(PowerKind::Reboot));
        assert_eq!(m.phase, Phase::Done(PowerKind::Reboot));
        assert_eq!(m.tick(), None, "fires once");
    }

    #[test]
    fn cancel_returns_to_idle_and_supersedes_the_ticker() {
        let mut m = Machine::default();
        m.start(PowerKind::PowerOff);
        let gen_ = m.generation;
        m.tick();
        assert_eq!(m.generation, gen_, "ticks keep the generation");
        m.cancel();
        assert_eq!(m.phase, Phase::Idle);
        assert_ne!(m.generation, gen_, "a stale ticker must notice the cancel");
        assert_eq!(m.tick(), None);
        // and a new countdown starts clean
        assert!(m.start(PowerKind::Reboot));
        assert_eq!(m.phase, Phase::Counting { kind: PowerKind::Reboot, left: COUNTDOWN_SECS });
    }

    #[test]
    fn now_and_hold_fire_exactly_once() {
        let mut m = Machine::default();
        m.start(PowerKind::PowerOff);
        m.tick();
        assert_eq!(m.fire_now(PowerKind::PowerOff), Some(PowerKind::PowerOff));
        assert_eq!(m.fire_now(PowerKind::PowerOff), None);
        // hold from idle skips the countdown entirely
        let mut m = Machine::default();
        assert_eq!(m.fire_now(PowerKind::Reboot), Some(PowerKind::Reboot));
        assert_eq!(m.phase, Phase::Done(PowerKind::Reboot));
    }

    #[test]
    fn hold_fires_only_if_still_held_and_swallows_the_click() {
        let mut h = Hold::default();
        // quick click: press, release before the timer, click goes through
        let g1 = h.press(PowerKind::Reboot);
        assert!(h.release());
        assert!(!h.expire(PowerKind::Reboot, g1), "released early: the timer does nothing");
        assert!(h.click_allowed());
        // a long press: timer fires, the click after release is swallowed (once)
        let g2 = h.press(PowerKind::PowerOff);
        assert!(h.expire(PowerKind::PowerOff, g2));
        assert_eq!(h.held, None);
        h.release();
        assert!(!h.click_allowed());
        assert!(h.click_allowed(), "only that one click is swallowed");
        // a stale timer of an earlier press must not fire a later press
        let g3 = h.press(PowerKind::Reboot);
        h.release();
        let g4 = h.press(PowerKind::Reboot);
        assert!(!h.expire(PowerKind::Reboot, g3) && g3 != g4);
        assert!(h.expire(PowerKind::Reboot, g4));
        // reset forgets a pending hold
        let g5 = h.press(PowerKind::Reboot);
        h.reset();
        assert!(!h.expire(PowerKind::Reboot, g5));
    }

    #[test]
    fn note_lists_windows_and_blockers() {
        let blocker = Inhibitor {
            what: "shutdown".into(),
            who: "Octium".into(),
            why: "Saving browser state".into(),
            mode: "block".into(),
            ..Default::default()
        };
        let delay = Inhibitor { mode: "delay".into(), ..blocker.clone() };
        assert_eq!(note_text(4, &[]), "4 windows will close · esc cancels");
        assert_eq!(note_text(1, std::slice::from_ref(&delay)), "1 window will close · esc cancels");
        assert_eq!(
            note_text(0, &[blocker.clone(), blocker]),
            "no open windows · Octium: Saving browser state (+1 more) · esc cancels"
        );
    }
}
