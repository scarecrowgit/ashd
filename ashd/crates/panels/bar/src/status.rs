//! Status block (desk-05 l.1590–1658, det-12 network states): clock + date │ cpu, mem │
//! network + volume, battery │ layout chip, bell.

use std::time::Duration;

use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::*, px,
};
use ashd_ipc::{PanelOp, Request};
use ashd_niri::niri_ipc::{Action, LayoutSwitchTarget};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, Slot};
use ashd_svc_sysinfo::{BatteryStatus, LinkKind, NetState};
use ashd_svc_network::barview::{Glyph, Tone, bar_net};
use ashd_ui::{Icon, icon, theme};

use crate::{BoundsStore, caps, geom, ids, is_open, open_look, sep};

/// Two-letter chip text for a niri/xkb layout name: `English (US)` → `EN`.
pub fn layout_code(name: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[
        ("english", "EN"),
        ("russian", "RU"),
        ("ukrainian", "UA"),
        ("german", "DE"),
        ("french", "FR"),
        ("spanish", "ES"),
        ("italian", "IT"),
        ("portuguese", "PT"),
        ("polish", "PL"),
        ("czech", "CZ"),
        ("swedish", "SE"),
        ("norwegian", "NO"),
        ("danish", "DK"),
        ("finnish", "FI"),
        ("dutch", "NL"),
        ("greek", "GR"),
        ("turkish", "TR"),
        ("hebrew", "HE"),
        ("arabic", "AR"),
        ("japanese", "JA"),
        ("korean", "KO"),
        ("chinese", "ZH"),
        ("belarusian", "BY"),
        ("georgian", "KA"),
        ("armenian", "HY"),
    ];
    let lower = name.trim().to_ascii_lowercase();
    if let Some((_, code)) = KNOWN.iter().find(|(lang, _)| lower.starts_with(lang)) {
        return code.to_string();
    }
    lower.chars().filter(|c| c.is_alphabetic()).take(2).collect::<String>().to_uppercase()
}

#[derive(Default)]
pub struct BarState {
    /// Highest notification `seq` seen while the notification centre was open.
    pub seen_seq: u64,
    /// Wheel travel on the now-playing control not yet turned into a track change.
    pub media_scroll: f32,
}

pub struct BarPanel {
    pub ctx: PanelContext,
    pub state: BarState,
    pub(crate) bounds: BoundsStore,
    pub(crate) tray: crate::tray::TrayBar,
}

impl BarPanel {
    /// Re-render on the minute boundary (the clock), nothing in between.
    fn schedule_clock(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let now = chrono::Local::now();
                let into_minute = now.timestamp_subsec_millis() as u64 + (now.timestamp() % 60) as u64 * 1000;
                let wait = Duration::from_millis(60_000 - into_minute.min(59_999) + 20);
                cx.background_executor().timer(wait).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn mark_seen(&mut self, cx: &gpui::App) {
        if is_open(ids::NOTIFICATION_CENTER, &self.ctx, cx) {
            self.state.seen_seq = self.ctx.services.notifications.state.read(cx).last_seq;
        }
    }
}

impl Panel for BarPanel {
    const ID: PanelId = PanelId("bar");
    const PLACEMENT: Placement = Placement::Frame(Slot::BarStart);

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.sysinfo.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.mpris.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.audio.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.network.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.notifications.state, |this, _, cx| {
            this.mark_seen(cx);
            cx.notify();
        })
        .detach();
        // Keyboard layouts live in niri state; re-render only when they change.
        cx.observe(&ctx.niri, {
            let mut last = None;
            move |_, niri, cx| {
                let kb = niri.read(cx).keyboard_layouts.clone();
                if kb != last {
                    last = kb;
                    cx.notify();
                }
            }
        })
        .detach();
        // The open modal drives the controls' "open" look.
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |this, _, cx| {
                this.mark_seen(cx);
                cx.notify();
            })
            .detach();
        }
        Self::schedule_clock(cx);
        let tray = crate::tray::TrayBar::new(&ctx, cx);
        let bounds = BoundsStore::new(&ctx);
        Self { ctx, state: BarState::default(), bounds, tray }
    }
}

impl Render for BarPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let now = chrono::Local::now();
        let sys = self.ctx.services.sysinfo.state.read(cx).clone();
        let audio = self.ctx.services.audio.state.read(cx);
        let sink = audio.available.then(|| audio.default_sink().cloned()).flatten();
        let notifications = self.ctx.services.notifications.state.read(cx);
        let unread = notifications.notifications.iter().any(|n| n.seq > self.state.seen_seq);
        let niri = self.ctx.niri.read(cx);
        let layout = niri.current_layout().map(layout_code);
        let layouts = niri.keyboard_layouts.as_ref().map(|k| k.names.len()).unwrap_or(0);
        let (fg, muted, s2) = (t.fg, t.fg_muted, t.surface_2);

        // ── clock ──
        let clock_open = is_open(ids::CALENDAR, &self.ctx, cx);
        let clock = open_look(
            div()
                .id("bar-clock")
                .relative()
                .flex_none()
                .h(px(geom::CONTROL_H))
                .px(px(4.))
                .flex()
                .flex_col()
                .justify_center()
                .cursor_pointer()
                .child(
                    div()
                        .font_family(t.font_mono.clone())
                        .text_size(px(geom::CLOCK_SIZE))
                        .line_height(px(geom::CLOCK_SIZE * geom::CLOCK_LINE))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(fg)
                        .child(now.format("%H:%M").to_string()),
                )
                .child(
                    caps(now.format("%a %-d %b").to_string(), geom::DATE_SIZE, muted, &t)
                        .line_height(px(geom::DATE_SIZE * geom::CLOCK_LINE + 1.)),
                )
                .child(self.bounds.track("clock"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("clock", &this.ctx, cx);
                    this.ctx.shell.request(Request::Calendar { op: PanelOp::Toggle }, anchor, cx);
                })),
            clock_open,
            &t,
        );

        // ── cpu / mem ──
        let meter_group = |caption: &str, value: f32, text: String| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(geom::METER_GAP))
                .child(caps(caption.to_string(), geom::CAPTION_SIZE, muted, &t))
                .child(ashd_ui::meter(value, geom::METER_W, cx))
                .child(caps(text, geom::VALUE_SIZE, fg, &t))
        };
        let cpu = meter_group("cpu", sys.cpu, format!("{:.0}%", sys.cpu * 100.0));
        let mem = meter_group("mem", sys.mem_fraction(), sys.mem_used_label());

        // ── network + volume (one control: opens quick settings) ──
        let net = &sys.network;
        let (net_icon, net_color, net_text, net_text_color, warn_dot) = match (net.state, net.kind) {
            (NetState::Connected, LinkKind::Wired) => (Icon::Ethernet, fg, net.speed_label(), fg, false),
            (NetState::Connected, LinkKind::Wifi) => (Icon::Wifi, fg, None, fg, false),
            (NetState::NoInternet, kind) => {
                let ic = if kind == LinkKind::Wifi { Icon::Wifi } else { Icon::Ethernet };
                (ic, fg, Some("no net".to_string()), t.warn, true)
            }
            (NetState::NoCarrier, LinkKind::Wifi) => (Icon::WifiOff, muted, Some("off".to_string()), muted, false),
            (NetState::NoCarrier, LinkKind::Wired) => (Icon::EthernetOff, muted, Some("off".to_string()), muted, false),
            (NetState::Unknown, _) => (Icon::GlobeOff, muted, None, muted, false),
        };
        // With the network service up it decides (det-12 states, pulse, VPN shield); the
        // sysinfo summary above is the fallback while it has not sampled.
        let netsvc = self.ctx.services.network.state.read(cx).clone();
        let (mut pulse, mut vpn) = (false, None);
        let (net_icon, net_color, net_text, net_text_color, warn_dot) = if netsvc.available {
            let b = bar_net(&netsvc);
            let tone = |tn: Tone| match tn {
                Tone::Normal => fg,
                Tone::Muted => muted,
                Tone::Accent => t.accent,
                Tone::Warn => t.warn,
            };
            let ic = match b.glyph {
                Glyph::Ethernet => Icon::Ethernet,
                Glyph::EthernetOff => Icon::EthernetOff,
                Glyph::Wifi(s) => Icon::wifi(Some(s)),
                Glyph::WifiOff => Icon::WifiOff,
                Glyph::GlobeOff => Icon::GlobeOff,
            };
            (pulse, vpn) = (b.pulse, b.vpn);
            (ic, tone(b.tone), b.text, tone(b.text_tone), b.warn_dot)
        } else {
            (net_icon, net_color, net_text, net_text_color, warn_dot)
        };
        let net_glyph = icon(net_icon, geom::STATUS_ICON).text_color(net_color);
        let net_glyph: gpui::AnyElement = if pulse {
            use gpui::AnimationExt as _;
            net_glyph
                .with_animation("net-pulse", gpui::Animation::new(Duration::from_millis(1200)).repeat(), |g, p| {
                    g.opacity(1.0 - 0.65 * (p * std::f32::consts::PI).sin())
                })
                .into_any_element()
        } else {
            net_glyph.into_any_element()
        };
        let net_el = div()
            .flex()
            .items_center()
            .gap(px(geom::NET_GAP))
            .child(
                div().relative().flex().child(net_glyph).when(warn_dot, |d| {
                    d.child(
                        div()
                            .absolute()
                            .right(px(-3.))
                            .top(px(-2.))
                            .size(px(geom::NET_DOT))
                            .rounded(px(geom::NET_DOT / 2.))
                            .bg(t.warn),
                    )
                }),
            )
            .children(net_text.map(|s| caps(s, geom::VALUE_SIZE, net_text_color, &t)))
            .children(vpn.map(|name| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(icon(Icon::Shield, 12.).text_color(t.accent))
                    // A long VPN name is cut, not allowed to eat the status pill.
                    .child(
                        caps(name, geom::CAPTION_SIZE, t.accent, &t)
                            .max_w(px(geom::VPN_LABEL_MAX))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis(),
                    )
            }));
        let vol_el = sink.map(|d| {
            let pct = (d.volume * 100.0).round() as u32;
            let (ic, color) = if d.muted || pct == 0 {
                (Icon::VolumeMute, muted)
            } else if pct < 34 {
                (Icon::Volume0, fg)
            } else if pct < 67 {
                (Icon::Volume1, fg)
            } else {
                (Icon::Volume, fg)
            };
            div()
                .flex()
                .items_center()
                .gap(px(geom::ICON_GAP))
                .child(icon(ic, geom::STATUS_ICON).text_color(color))
                .child(caps(if d.muted { "mute".to_string() } else { pct.to_string() }, geom::VALUE_SIZE, color, &t))
        });
        let qs_open = is_open(ids::QUICK_SETTINGS, &self.ctx, cx);
        let netvol = open_look(
            div()
                .id("bar-netvol")
                .relative()
                .flex_none()
                .h(px(geom::CONTROL_H))
                .px(px(geom::CONTROL_PAD_X))
                .flex()
                .items_center()
                .gap(px(geom::GAP))
                .cursor_pointer()
                .when(!qs_open, |d| d.hover(move |s| s.bg(s2)))
                .child(net_el)
                .children(vol_el)
                .child(self.bounds.track("netvol"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("netvol", &this.ctx, cx);
                    this.ctx.shell.request(Request::QuickSettings { op: PanelOp::Toggle }, anchor, cx);
                })),
            qs_open,
            &t,
        );

        // ── battery (only if there is one) ──
        let battery = sys.battery.as_ref().map(|b| {
            let charging = b.status == BatteryStatus::Charging;
            let color = if b.percent <= 10 && !charging { t.err } else { fg };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(geom::ICON_GAP))
                .child(icon(Icon::battery(b.percent as f32 / 100.0, charging), geom::STATUS_ICON).text_color(color))
                .child(caps(format!("{}%", b.percent), geom::VALUE_SIZE, color, &t))
        });

        // ── layout chip (click: next layout) ──
        let chip = layout.map(|code| {
            let accent = t.accent;
            div()
                .id("bar-layout")
                .flex_none()
                .h(px(geom::CHIP_H))
                .px(px(geom::CHIP_PAD_X))
                .flex()
                .items_center()
                .border_1()
                .border_color(accent)
                .rounded(t.radius_control)
                .font_family(t.font_mono.clone())
                .text_size(px(geom::VALUE_SIZE))
                .font_weight(FontWeight::MEDIUM)
                .text_color(accent)
                .when(layouts > 1, |d| d.cursor_pointer().hover(move |s| s.bg(s2)))
                .child(code)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ctx.shell.niri_action(Action::SwitchLayout { layout: LayoutSwitchTarget::Next }, cx);
                }))
        });

        // ── bell ──
        let bell_open = is_open(ids::NOTIFICATION_CENTER, &self.ctx, cx);
        let bell = open_look(
            div()
                .id("bar-bell")
                .relative()
                .flex_none()
                .size(px(geom::BELL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(t.radius_control)
                .cursor_pointer()
                .when(!bell_open, |d| d.hover(move |s| s.bg(s2)))
                .child(icon(Icon::Bell, geom::BELL_ICON).text_color(fg))
                .when(unread, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(geom::BELL_DOT_INSET))
                            .right(px(geom::BELL_DOT_INSET))
                            .size(px(geom::BELL_DOT))
                            .rounded(px(geom::BELL_DOT / 2.))
                            .bg(t.accent),
                    )
                })
                .child(self.bounds.track("bell"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("bell", &this.ctx, cx);
                    this.ctx.shell.request(Request::Notifications { op: PanelOp::Toggle }, anchor, cx);
                })),
            bell_open,
            &t,
        );

        // ── tray (nothing at all while empty) ──
        let tray = self.tray_section(&t, cx);
        let row_gap = if tray.is_some() { geom::TRAY_ROW_GAP } else { geom::GAP };

        let has_chip = chip.is_some();
        div()
            .id("bar-status")
            .size_full()
            .overflow_hidden()
            .px(px(geom::PAD_X - 4.))
            .flex()
            .items_center()
            .gap(px(row_gap))
            .child(clock)
            .child(sep(&t))
            .child(cpu)
            .child(mem)
            .child(sep(&t))
            .child(netvol)
            .children(battery)
            .when(has_chip, |d| d.child(sep(&t)))
            .children(chip)
            .children(tray)
            .child(bell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_codes() {
        assert_eq!(layout_code("English (US)"), "EN");
        assert_eq!(layout_code("Russian"), "RU");
        assert_eq!(layout_code("German (Neo 2)"), "DE");
        assert_eq!(layout_code("Esperanto"), "ES");
        assert_eq!(layout_code(""), "");
    }
}
