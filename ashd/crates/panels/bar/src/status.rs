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

use crate::{BoundsStore, caps, fixture, geom, hover_layer, ids, is_open, open_layer, sep, slot};

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

/// CPU readout: whole percent, `7%`, `100%` (never wider than [`geom::CPU_CHARS`]).
pub fn cpu_label(fraction: f32) -> String {
    let f = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
    format!("{:.0}%", (f * 100.0).round())
}

/// Volume readout next to the glyph: `0`..`150`, or `mute`.
pub fn volume_label(percent: u32, muted: bool) -> String {
    if muted { "mute".to_string() } else { percent.min(150).to_string() }
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let now = chrono::Local::now();
        let mut sys = self.ctx.services.sysinfo.state.read(cx).clone();
        let audio = self.ctx.services.audio.state.read(cx);
        let mut sink = audio.available.then(|| audio.default_sink().cloned()).flatten().map(|d| ((d.volume * 100.0).round() as u32, d.muted));
        let notifications = self.ctx.services.notifications.state.read(cx);
        let mut unread = notifications.notifications.iter().any(|n| n.seq > self.state.seen_seq);
        // nested session only: force every readout to its widest value
        if let Some(fx) = fixture::from_env() {
            let forced = fx.apply(&mut sys);
            sink = Some(forced.volume);
            unread |= forced.unread;
        }
        let niri = self.ctx.niri.read(cx);
        let layout = niri.current_layout().map(layout_code);
        let layouts = niri.keyboard_layouts.as_ref().map(|k| k.names.len()).unwrap_or(0);
        let (fg, muted) = (t.fg, t.fg_muted);

        // ── clock ──
        let clock_open = is_open(ids::CALENDAR, &self.ctx, cx);
        let clock = div()
                .id("bar-clock")
                .relative()
                .flex_none()
                .h(px(geom::CONTROL_H))
                .px(px(4.))
                .flex()
                .flex_col()
                .justify_center()
                .cursor_pointer()
                .child(open_layer(clock_open, &t))
                .child({
                    // the minute changes: the digits dip and come back (120 ms)
                    let (mono, text) = (t.font_mono.clone(), now.format("%H:%M").to_string());
                    ashd_ui::swap("clock-text", text.clone(), move |p| {
                        div()
                            .font_family(mono)
                            .text_size(px(geom::CLOCK_SIZE))
                            .line_height(px(geom::CLOCK_SIZE * geom::CLOCK_LINE))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(fg)
                            .opacity(p)
                            .child(text)
                    })
                })
                .child(
                    caps(now.format("%a %-d %b").to_string(), geom::DATE_SIZE, muted, &t)
                        .line_height(px(geom::DATE_SIZE * geom::CLOCK_LINE + 1.)),
                )
                .child(self.bounds.track("clock"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("clock", &this.ctx, cx);
                    this.ctx.shell.request(Request::Calendar { op: PanelOp::Toggle }, anchor, cx);
                }));

        // ── cpu / mem ──
        let meter_group = |caption: &str, value: f32, text: String, chars: usize| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(geom::METER_GAP))
                .child(caps(caption.to_string(), geom::CAPTION_SIZE, muted, &t))
                .child(ashd_ui::meter(value, geom::METER_W, cx))
                .child(slot(text, chars, geom::VALUE_SIZE, fg, &t))
        };
        let cpu = meter_group("cpu", sys.cpu, cpu_label(sys.cpu), geom::CPU_CHARS);
        let mem = meter_group("mem", sys.mem_fraction(), sys.mem_used_label(), geom::MEM_CHARS);

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
        // ── how the row is laid out ──
        // The block hugs its content (at most `STATUS_W`): the gap is the widest of the
        // ladder at which the fixed parts fit, a VPN shield that cannot fit even at the
        // tightest gap is left out, and the tray gets what the other items leave.
        let net_chars = net_text.as_ref().map(|s| s.chars().count());
        let has_battery = sys.battery.is_some();
        let tray_present = self.tray_present(cx);
        if !tray_present {
            self.tray.animating = false;
        }
        let base = geom::Parts {
            battery: has_battery,
            net_text: net_chars,
            vpn_label: None,
            tray: tray_present.then_some(geom::TRAY_CHEVRON_W),
        };
        let vpn = vpn.filter(|_| geom::vpn_fits(base, geom::BLOCK_MAX_W));
        let has_vpn = vpn.is_some();
        let parts = geom::Parts { vpn_label: has_vpn.then_some(0.0), ..base };
        let row_gap = geom::row_gap(parts, geom::BLOCK_MAX_W);
        // ── tray (nothing at all while empty) ──
        let tray_room = geom::tray_avail(parts, row_gap);
        // The VPN label takes what the tray leaves; with less than ~3 characters of room the
        // shield stands alone instead of next to a lone ellipsis.
        let label_room = (tray_room - if tray_present { self.tray_area_width(tray_room, cx) } else { 0.0 }).max(0.0);
        let vpn_label_w = geom::vpn_label_width(label_room);
        let tray = if tray_present { self.tray_section(&t, tray_room, cx) } else { None };
        if self.tray.animating {
            window.request_animation_frame();
        }
        let net_el = div()
            .flex()
            .flex_shrink(1.0)
            .min_w_0()
            .overflow_hidden()
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
            .children(net_text.map(|s| {
                let (size, color, tt) = (geom::VALUE_SIZE, net_text_color, t.clone());
                ashd_ui::swap("net-text", s.clone(), move |p| caps(s, size, color, &tt).opacity(p))
            }))
            .children(vpn.map(|name| {
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(geom::VPN_GAP))
                    .child(icon(Icon::Shield, geom::VPN_ICON).text_color(t.accent).flex_none())
                    // A long VPN name is cut, and gives way first when the row is tight:
                    // it takes what is left after everything else, down to the shield.
                    .children(vpn_label_w.map(|w| {
                        caps(name, geom::CAPTION_SIZE, t.accent, &t)
                            .flex_shrink(1.0)
                            .min_w_0()
                            .max_w(px(w))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                    }))
            }));
        let vol_el = sink.map(|(pct, is_muted)| {
            let (ic, color) = if is_muted || pct == 0 {
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
                .flex_none()
                .items_center()
                .gap(px(geom::ICON_GAP))
                .child(icon(ic, geom::STATUS_ICON).text_color(color))
                .child(slot(volume_label(pct, is_muted), geom::VOLUME_CHARS, geom::VALUE_SIZE, color, &t))
        });
        let qs_open = is_open(ids::QUICK_SETTINGS, &self.ctx, cx);
        let netvol = div()
                .id("bar-netvol")
                .relative()
                // the one control that gives when the row is full (VPN label, then the
                // text): everything else keeps its width, so the bell is never pushed out
                .flex_shrink(1.0)
                .min_w_0()
                .overflow_hidden()
                .h(px(geom::CONTROL_H))
                .px(px(geom::CONTROL_PAD_X))
                .flex()
                .items_center()
                .gap(px(geom::GAP))
                .cursor_pointer()
                .child(open_layer(qs_open, &t))
                .when(!qs_open, |d| d.child(hover_layer(&t)))
                .child(net_el)
                .children(vol_el)
                .child(self.bounds.track("netvol"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("netvol", &this.ctx, cx);
                    this.ctx.shell.request(Request::QuickSettings { op: PanelOp::Toggle }, anchor, cx);
                }));

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
                .child(slot(format!("{}%", b.percent), geom::BATTERY_CHARS, geom::VALUE_SIZE, color, &t))
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
                .when(layouts > 1, |d| d.cursor_pointer().child(hover_layer(&t)))
                .child(code)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ctx.shell.niri_action(Action::SwitchLayout { layout: LayoutSwitchTarget::Next }, cx);
                }))
        });

        // ── bell ──
        let bell_open = is_open(ids::NOTIFICATION_CENTER, &self.ctx, cx);
        let bell = div()
                .id("bar-bell")
                .relative()
                .flex_none()
                .size(px(geom::BELL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(t.radius_control)
                .cursor_pointer()
                .child(open_layer(bell_open, &t))
                .when(!bell_open, |d| d.child(hover_layer(&t)))
                .child(icon(Icon::Bell, geom::BELL_ICON).text_color(fg))
                // the unread dot grows in and shrinks away (120 ms)
                .child(ashd_ui::follow(
                    "bell-dot",
                    if unread { 1.0 } else { 0.0 },
                    ashd_ui::presence::Timing::new(ashd_ui::anim::motion::timing::SWAP, ashd_ui::anim::EASE_STANDARD),
                    {
                        let accent = t.accent;
                        move |p| {
                            let d = geom::BELL_DOT * p;
                            div()
                                .absolute()
                                .top(px(geom::BELL_DOT_INSET + (geom::BELL_DOT - d) / 2.))
                                .right(px(geom::BELL_DOT_INSET + (geom::BELL_DOT - d) / 2.))
                                .size(px(d))
                                .rounded(px(d / 2.))
                                .bg(accent)
                        }
                    },
                ))
                .child(self.bounds.track("bell"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("bell", &this.ctx, cx);
                    this.ctx.shell.request(Request::Notifications { op: PanelOp::Toggle }, anchor, cx);
                }));

        let has_chip = chip.is_some();
        div()
            .id("bar-status")
            .h_full()
            .max_w(px(geom::BLOCK_MAX_W))
            .overflow_hidden()
            .pl(px(geom::PAD_LEFT))
            .pr(px(row_gap))
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
            // the bell follows the chip (or the tray) at the row's gap; the block ends
            // one gap after it
            .children(tray)
            .child(bell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readout_labels_stay_inside_their_slots() {
        assert_eq!(cpu_label(0.0), "0%");
        assert_eq!(cpu_label(0.074), "7%");
        assert_eq!(cpu_label(1.0), "100%");
        assert_eq!(cpu_label(1.7), "100%", "never wider than the slot");
        assert_eq!(cpu_label(f32::NAN), "0%");
        assert_eq!(volume_label(62, false), "62");
        assert_eq!(volume_label(150, false), "150");
        assert_eq!(volume_label(400, false), "150");
        assert_eq!(volume_label(62, true), "mute");
        for text in [cpu_label(1.0), volume_label(150, false), volume_label(0, true)] {
            assert!(text.chars().count() <= geom::VOLUME_CHARS.max(geom::CPU_CHARS));
        }
    }

    #[test]
    fn layout_codes() {
        assert_eq!(layout_code("English (US)"), "EN");
        assert_eq!(layout_code("Russian"), "RU");
        assert_eq!(layout_code("German (Neo 2)"), "DE");
        assert_eq!(layout_code("Esperanto"), "ES");
        assert_eq!(layout_code(""), "");
    }
}
