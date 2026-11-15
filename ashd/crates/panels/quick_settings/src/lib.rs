//! Quick settings popup growing up from the status bar (det-06, det-12, det-13).
//!
//! Owner: quick settings. Contract: ARCHITECTURE.md "Panels" — `QuickSettingsPanel` is
//! registered by `ashd-shell` with `reg.register::<QuickSettingsPanel>()`.
//!
//! Top to bottom: tile grid (Do not disturb; Wi-Fi / Bluetooth / Night light / Network
//! come with their services), output volume, input volume, the audio-output radio list,
//! the theme row and the footer (avatar, `user@host`, uptime, Lock / Reboot / Shut down).
//! Reboot and Shut down turn the footer into the shared in-place countdown ([`power`]).
//!
//! This crate also hosts what the session panel shares with it: [`power`] (countdown +
//! session buttons), [`identity`] (`user@host`, uptime strings) and [`themes`].

mod bluetooth;
mod display;
mod network;
mod sparkline;
mod wifi;
pub mod identity;
pub mod power;
pub mod themes;

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui::{
    App, Context, Div, ElementId, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::*, px,
};
use ashd_ipc::{PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_audio::{Device, Target};
use ashd_tokens::{LayoutVariant, metrics, motion};
use ashd_ui::{
    Icon, Look, anim, follow, hover_wash, icon, label, mix, presence::{Presence, Timing}, state_layer, theme,
};

use crate::{
    identity::{Identity, format_uptime, read_proc_uptime},
    power::{PowerCountdown, PowerKind, power_button, session_button},
};

/// A slider value the user just set stays on screen for this long, so the thumb does not
/// jump back to a stale snapshot while the server catches up.
const PENDING_MS: u64 = 400;

/// Panel state (fm rule: everything this region needs lives here).
pub struct QuickSettingsPanelState {
    focus: FocusHandle,
    lock_focus: FocusHandle,
    reboot_focus: FocusHandle,
    off_focus: FocusHandle,
    identity: Identity,
    /// `/proc/uptime`, used while the sysinfo service has not sampled yet.
    uptime_fallback: Option<Duration>,
    pending_sink: Option<(f32, Instant)>,
    pending_source: Option<(f32, Instant)>,
    /// Night light section expanded (temperature slider, mode selector).
    night_open: bool,
    /// ... how far it has unfolded (`fold`).
    night_view: Option<Presence>,
    /// Temperature slider position the user just set (0..=1).
    pending_temp: Option<(f32, Instant)>,
    /// Brightness the user just set, by display id.
    pending_brightness: HashMap<String, (f32, Instant)>,
    /// Network / Wi-Fi / Bluetooth sections ([`network::ConnState`]).
    conn: network::ConnState,
    pub countdown: Entity<PowerCountdown>,
}

pub struct QuickSettingsPanel {
    pub ctx: PanelContext,
    pub state: QuickSettingsPanelState,
}

impl Panel for QuickSettingsPanel {
    const ID: PanelId = PanelId("quick-settings");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });
    const ANCHORS: &'static [&'static str] = &[ashd_shell_core::anchors::names::NETWORK, ashd_shell_core::anchors::names::VOLUME];

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.audio.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.notifications.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.sysinfo.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.logind.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.nightlight.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.brightness.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.network.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.bluetooth.state, |_, _, cx| cx.notify()).detach();
        let countdown = cx.new(|cx| PowerCountdown::new(ctx.clone(), false, cx));
        cx.observe(&countdown, |_, _, cx| cx.notify()).detach();
        let state = QuickSettingsPanelState {
            focus: cx.focus_handle(),
            lock_focus: cx.focus_handle(),
            reboot_focus: cx.focus_handle(),
            off_focus: cx.focus_handle(),
            identity: Identity::current(),
            uptime_fallback: None,
            pending_sink: None,
            pending_source: None,
            night_open: false,
            night_view: None,
            pending_temp: None,
            pending_brightness: HashMap::new(),
            conn: network::ConnState::default(),
            countdown,
        };
        Self { ctx, state }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::QuickSettings { op } = request else { return None };
        let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
        match (op, open) {
            (PanelOp::Open, _) | (PanelOp::Toggle, false) => self.ctx.shell.open_modal(Self::ID, None, cx),
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.focus.focus(window, cx);
        // Re-read the displays (rate-limited in the service) in case a monitor's own buttons were used.
        self.ctx.services.brightness.handle.refresh();
        self.state.identity = Identity::current();
        // Uptime fallback + inhibitors are read off the main thread.
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
        self.state.pending_sink = None;
        self.state.pending_source = None;
        self.state.pending_temp = None;
        self.state.pending_brightness.clear();
        self.state.night_open = false;
        self.state.night_view = None;
        self.connectivity_on_close(cx);
    }
}

// ── small view helpers ────────────────────────────────────────────────────────

fn volume_icon(volume: f32, muted: bool) -> Icon {
    if muted {
        Icon::VolumeMute
    } else if volume <= 0.001 {
        Icon::Volume0
    } else if volume < 0.5 {
        Icon::Volume1
    } else {
        Icon::Volume
    }
}

/// A section that unfolds when it opens and folds when it closes: 140 ms out-expo in,
/// 100 ms out-quad out, like the rest of the popup's motion.
pub(crate) mod fold {
    use std::time::Instant;

    use ashd_ui::{
        anim::{EASE_OUT_EXPO, EASE_OUT_QUAD},
        presence::{Presence, Timing},
    };

    use super::motion;

    /// The section was opened / closed now.
    pub(crate) fn set(view: &mut Option<Presence>, open: bool, now: Instant) {
        let p = view.get_or_insert_with(|| {
            let (enter, exit) = (
                Timing::new(motion::timing::PAGE, EASE_OUT_EXPO),
                Timing::new(motion::timing::ITEM_OUT, EASE_OUT_QUAD),
            );
            // Never animated before: it was either absent, or (closing) fully there.
            if open { Presence::absent(enter, exit, now) } else { Presence::present(enter, exit, now) }
        });
        p.set_present(open, now);
    }

    /// Still on screen: open, or on its way out.
    pub(crate) fn alive(open: bool, view: &Option<Presence>, now: Instant) -> bool {
        open || view.is_some_and(|v| !v.is_gone(now))
    }

    /// (height progress, fade) of a section; fully unfolded when it never animated.
    pub(crate) fn progress(view: &Option<Presence>, now: Instant) -> (f32, f32) {
        view.map_or((1.0, 1.0), |v| (v.movement(now), v.value(now)))
    }

    pub(crate) fn animating(view: &Option<Presence>, now: Instant) -> bool {
        view.is_some_and(|v| v.is_animating(now))
    }
}

/// Percent for the mono readout.
fn percent(v: f32) -> String {
    format!("{}", (v * 100.0).round() as i32)
}

/// The value to show: the one the user just dragged to, while it is fresh.
fn effective(device: f32, pending: Option<(f32, Instant)>) -> f32 {
    match pending {
        Some((v, at)) if at.elapsed() < Duration::from_millis(PENDING_MS) => v,
        _ => device,
    }
}

/// A tile of the 2-column grid (54 high; on = accent fill with on-accent text). Flipping
/// `on` cross-fades fill, border and text (120 ms).
fn tile(id: impl Into<ElementId>, ic: Icon, title: &str, sub: &str, on: bool, cx: &App) -> Stateful<Div> {
    let t = theme(cx).clone();
    let (s2, accent, on_accent, line, fg_off) = (t.surface_2, t.accent, t.on_accent, t.line, t.fg);
    let clear = gpui::transparent_black();
    let (font_sans, font_mono) = (t.font_sans.clone(), t.font_mono.clone());
    let (title, sub) = (SharedString::from(title.to_string()), SharedString::from(sub.to_string()));
    let content = follow(
        "tile-content",
        if on { 1.0 } else { 0.0 },
        Timing::new(motion::timing::SWAP, anim::EASE_STANDARD),
        move |p| {
            let fg = mix(fg_off, on_accent, p);
            div()
                .size_full()
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .text_color(fg)
                .child(icon(ic, 16.).text_color(fg))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .font_family(font_sans)
                                .text_size(px(metrics::type_scale::UI.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(div().font_family(font_mono).text_size(px(10.5)).opacity(0.8).child(sub)),
                )
        },
    )
    .fade();
    div()
        .id(id)
        .h(px(54.))
        .border_1()
        .border_color(clear)
        .rounded(t.radius_control)
        .cursor_pointer()
        .child(state_layer("tile-fx", on, Look::new(clear, line), Look::new(accent, accent), t.radius_control))
        .child(hover_wash(s2, None, t.radius_control))
        .child(content)
}

/// One radio row of the audio-output list (30 high, accent border when selected).
fn radio_row(ix: usize, device: &Device, cx: &App) -> Stateful<Div> {
    let t = theme(cx).clone();
    let selected = device.is_default;
    let s2 = t.surface_2;
    div()
        .id(ElementId::from(("sink", ix as u32)))
        .h(px(metrics::ROW_H))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .border_1()
        .border_color(gpui::transparent_black())
        .rounded(t.radius_control)
        .cursor_pointer()
        .child(state_layer(
            "sink-fx",
            selected,
            Look::new(gpui::transparent_black(), gpui::transparent_black()),
            Look::new(gpui::transparent_black(), t.accent),
            t.radius_control,
        ))
        .when(!selected, |d| d.child(hover_wash(s2, None, t.radius_control)))
        .child(
            div()
                .size(px(12.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(if selected { t.accent } else { t.fg_muted })
                .when(selected, |d| d.child(div().size(px(6.)).rounded_full().bg(t.accent))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(t.font_sans.clone())
                .text_size(px(metrics::type_scale::UI.0))
                .text_color(t.fg)
                .child(SharedString::from(device.description.clone())),
        )
        .when_some(device.port.clone(), |d, port| d.child(label(port, cx).text_size(px(10.))))
}

impl QuickSettingsPanel {
    fn pending_clear(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(PENDING_MS + 20)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    fn set_sink_volume(&mut self, v: f32, cx: &mut Context<Self>) {
        self.state.pending_sink = Some((v, Instant::now()));
        self.ctx.services.audio.handle.set_volume(Target::DefaultSink, v);
        self.pending_clear(cx);
        cx.notify();
    }

    fn set_source_volume(&mut self, v: f32, cx: &mut Context<Self>) {
        self.state.pending_source = Some((v, Instant::now()));
        self.ctx.services.audio.handle.set_volume(Target::DefaultSource, v);
        self.pending_clear(cx);
        cx.notify();
    }

    /// Lock is one click and never asks: the same request as `ashd lock`.
    fn lock(&mut self, cx: &mut Context<Self>) {
        if ashd_svc_logind::fake_power() {
            tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: lock requested (nothing is done)");
        } else if let Some(shell) = self.ctx.shell.entity() {
            ashd_shell_core::dispatch(&shell.downgrade(), &Request::Lock, cx);
        }
        self.ctx.shell.close_modal(cx);
    }

    fn slider_row(
        &self,
        id: &'static str,
        ic: Icon,
        value: f32,
        muted: bool,
        on_icon: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        on_change: impl Fn(&mut Self, f32, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let slider_listener = cx.listener(move |this, v: &f32, _, cx| on_change(this, *v, cx));
        let slider = ashd_ui::slider(id, value.min(1.0), slider_listener, window, cx);
        let readout = if muted { "mute".to_string() } else { percent(value) };
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .id(ElementId::from((id, 1u32)))
                    .flex_none()
                    .cursor_pointer()
                    .child(icon(ic, 15.).text_color(if muted { t.fg_muted } else { t.fg }))
                    .on_click(cx.listener(move |this, _, _, cx| on_icon(this, cx))),
            )
            .child(div().flex_1().when(muted, |d| d.opacity(0.5)).child(slider))
            .child(
                div()
                    .w(px(30.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .font_family(t.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(if muted { t.fg_muted } else { t.fg })
                    .child(readout),
            )
    }
}

impl Render for QuickSettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let audio = self.ctx.services.audio.state.read(cx).clone();
        let dnd = self.ctx.services.notifications.state.read(cx).dnd;
        let logind = self.ctx.services.logind.state.read(cx).clone();
        let sysinfo_uptime = self.ctx.services.sysinfo.state.read(cx).uptime;
        let current_theme = ashd_ui::current_theme_name(cx);

        // The sections fold / unfold on the clock: frames only while one moves.
        let now = Instant::now();
        if fold::animating(&self.state.night_view, now) || fold::animating(&self.state.conn.view, now) {
            window.request_animation_frame();
        }

        // ── placement: bottom = bar_h + POPUP_GAP, left from the anchor ──
        let anchor = self.ctx.shell.modal_anchor(Self::ID, cx);
        let output_w: f32 = self.ctx.shell.modal_display(cx).map(|d| d.size().width.into()).unwrap_or(1920.0);
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let bottom = f32::from(t.bar_h) + margin + metrics::POPUP_GAP;
        let left = anchor.map(|a| f32::from(a.origin.x)).unwrap_or(margin);
        let left = left.min(output_w - metrics::QUICK_SETTINGS_W - margin).max(margin);

        // ── tiles ──
        let mut tiles = div().grid().grid_cols(2).gap(px(6.)).child(
            tile("tile-dnd", Icon::Pause, "Do not disturb", if dnd { "on" } else { "off" }, dnd, cx).on_click({
                let notifications = self.ctx.services.notifications.handle.clone();
                move |_, _, _| notifications.set_dnd(!dnd)
            }),
        );
        if let Some(night) = self.night_light_tile(cx) {
            tiles = tiles.child(night);
        }
        // Network (+ Wi-Fi) and Bluetooth tiles come from their services (network.rs, bluetooth.rs).
        for t in self.network_tiles(cx) {
            tiles = tiles.child(t);
        }
        if let Some(bt) = self.bluetooth_tile(cx) {
            tiles = tiles.child(bt);
        }

        // ── audio ──
        let mut card = div().p(px(14.)).flex().flex_col().gap(px(12.)).child(tiles);
        if let Some(banner) = self.apply_banner(window, cx) {
            card = card.child(banner);
        }
        if let Some(section) = self.connectivity_section(window, cx) {
            card = card.child(section);
        }
        if let Some(section) = self.night_light_section(window, cx) {
            card = card.child(section);
        }
        // Brightness rows go right after the output volume (det-06); without a sink they
        // follow the audio block.
        let mut brightness = Some(self.brightness_rows(window, cx));
        if audio.available {
            if let Some(sink) = audio.default_sink() {
                let v = effective(sink.volume, self.state.pending_sink);
                let row = self.slider_row(
                    "qs-volume",
                    volume_icon(v, sink.muted),
                    v,
                    sink.muted,
                    |this, _| this.ctx.services.audio.handle.toggle_mute(Target::DefaultSink),
                    |this, v, cx| this.set_sink_volume(v, cx),
                    window,
                    cx,
                );
                card = card.child(row);
                if let Some(rows) = brightness.take() {
                    card = card.children(rows);
                }
            }
            if let Some(src) = audio.default_source() {
                let v = effective(src.volume, self.state.pending_source);
                let row = self.slider_row(
                    "qs-input",
                    if src.muted { Icon::MicOff } else { Icon::Mic },
                    v,
                    src.muted,
                    |this, _| this.ctx.services.audio.handle.toggle_mute(Target::DefaultSource),
                    |this, v, cx| this.set_source_volume(v, cx),
                    window,
                    cx,
                );
                card = card.child(row);
            }
            if !audio.sinks.is_empty() {
                let rows = audio.sinks.iter().enumerate().map(|(ix, d)| {
                    let name = d.name.clone();
                    radio_row(ix, d, cx).on_click(cx.listener(move |this, _, _, _| {
                        this.ctx.services.audio.handle.set_default(Target::Sink(name.clone()));
                    }))
                });
                card = card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(label("audio output", cx))
                        .children(rows),
                );
            }
        } else {
            card = card.child(
                div()
                    .h(px(metrics::ROW_H))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(Icon::VolumeMute, 15.).text_color(t.fg_muted))
                    .child(label("audio unavailable · no pulse server", cx)),
            );
        }

        if let Some(rows) = brightness.take() {
            card = card.children(rows);
        }

        // ── theme row ──
        let shell = self.ctx.shell.clone();
        card = card.child(themes::theme_row(
            current_theme,
            themes::select_theme,
            move |cx| themes::open_switcher(&shell, cx),
            cx,
        ));

        // ── footer: identity + session buttons, or the countdown ──
        let countdown = self.state.countdown.clone();
        let footer = if countdown.read(cx).active() {
            div().child(countdown.clone())
        } else {
            let up = if sysinfo_uptime > Duration::ZERO { Some(sysinfo_uptime) } else { self.state.uptime_fallback };
            let id = &self.state.identity;
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .size(px(28.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(t.accent)
                        .rounded(t.radius_control)
                        .text_color(t.accent)
                        .font_family(t.font_mono.clone())
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(id.initial()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .truncate()
                                .font_family(t.font_sans.clone())
                                .text_size(px(metrics::type_scale::UI.0))
                                .text_color(t.fg)
                                .child(id.label()),
                        )
                        .child(label(up.map(format_uptime).unwrap_or_default(), cx).text_size(px(10.))),
                )
                .child(
                    session_button("qs-lock", Icon::Lock, None, false, Some(&self.state.lock_focus), cx)
                        .on_click(cx.listener(|this, _, _, cx| this.lock(cx)))
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                            if matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                                this.lock(cx);
                            }
                        })),
                )
                .child(power_button(
                    "qs-reboot",
                    PowerKind::Reboot,
                    logind.available && !logind.can_reboot,
                    Some(&self.state.reboot_focus),
                    &countdown,
                    cx,
                ))
                .child(power_button(
                    "qs-off",
                    PowerKind::PowerOff,
                    logind.available && !logind.can_power_off,
                    Some(&self.state.off_focus),
                    &countdown,
                    cx,
                ))
        };
        card = card.child(
            div().pt(px(12.)).border_t_1().border_color(t.line).child(footer),
        );

        let panel = ashd_ui::overlay_panel(cx)
            .occlude()
            .track_focus(&self.state.focus)
            .key_context("QuickSettings")
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "tab" {
                    if ev.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                }
            }))
            .absolute()
            .left(px(left))
            .bottom(px(bottom))
            .w(px(metrics::QUICK_SETTINGS_W))
            .text_color(t.fg)
            .child(card);
        div().size_full().relative().child(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_icons_follow_level_and_mute() {
        assert_eq!(volume_icon(0.9, true), Icon::VolumeMute);
        assert_eq!(volume_icon(0.0, false), Icon::Volume0);
        assert_eq!(volume_icon(0.3, false), Icon::Volume1);
        assert_eq!(volume_icon(0.62, false), Icon::Volume);
    }

    #[test]
    fn pending_value_wins_only_while_fresh() {
        assert_eq!(effective(0.2, None), 0.2);
        assert_eq!(effective(0.2, Some((0.7, Instant::now()))), 0.7);
        let old = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
        assert_eq!(effective(0.2, Some((0.7, old))), 0.2);
    }

    #[test]
    fn percent_rounds() {
        assert_eq!(percent(0.62), "62");
        assert_eq!(percent(1.5), "150");
        assert_eq!(percent(0.0), "0");
    }
}
