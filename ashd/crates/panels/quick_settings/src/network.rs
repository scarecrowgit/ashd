//! Network tile, Ethernet section (det-12), VPN rows and the "Keep these network settings?"
//! banner of the quick-settings panel. The Wi-Fi list (det-06) is in [`crate::wifi`], the
//! Bluetooth section in [`crate::bluetooth`]; the shared bits (section accordion, small
//! controls) live here.
//!
//! Everything that decides *what* to show is a plain function (unit-tested); the
//! `impl QuickSettingsPanel` blocks only lay it out. Controls that need NetworkManager
//! (profiles, Disconnect, Settings…, the static editor, Retry DHCP, Wi-Fi) render only when
//! the service says `can_configure`; on a host without NetworkManager the section is
//! read-only information.

use std::time::{Duration, Instant};

use gpui::{
    Focusable as _,
    ClickEvent, Context, Div, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::*, px,
};
use ashd_svc_network::{
    IfKind, Interface, LinkStatus, NetworkState, Profile, ProfileKind, StaticConfig, StepStatus, Vpn, fmt, nm::parse_cidr,
};
use ashd_tokens::metrics;
use ashd_ui::{Icon, TextInput, TextInputEvent, icon, label, theme};

use crate::{QuickSettingsPanel, sparkline, tile};

/// Which connectivity section is expanded under the tile grid (one at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnSection {
    Network,
    Wifi,
    Bluetooth,
}

/// Static-profile editor fields.
pub(crate) struct Editor {
    pub name: Entity<TextInput>,
    pub address: Entity<TextInput>,
    pub gateway: Entity<TextInput>,
    pub dns: Entity<TextInput>,
    pub error: Option<String>,
}

/// Everything the connectivity sections need to remember between frames.
#[derive(Default)]
pub(crate) struct ConnState {
    pub section: Option<ConnSection>,
    /// Sparkline scale, latched per view.
    pub scale: Option<f32>,
    /// SSID whose inline password field is open.
    pub wifi_pick: Option<String>,
    pub wifi_input: Option<Entity<TextInput>>,
    pub editor: Option<Editor>,
    /// Copy button that was just used (flips its icon to a check for a moment).
    pub copied: Option<(&'static str, Instant)>,
    /// Bluetooth: device path whose small menu is open.
    pub bt_menu: Option<String>,
    /// Bluetooth: PIN / passkey entry field of a pairing prompt.
    pub bt_input: Option<Entity<TextInput>>,
}

const COPIED_MS: u64 = 1300;

// ── pure decisions ────────────────────────────────────────────────────────────

/// Icon of the network tile.
pub(crate) fn tile_icon(net: &NetworkState) -> Icon {
    match net.primary.as_ref() {
        None => Icon::GlobeOff,
        Some(p) => match (p.kind, p.status) {
            (IfKind::Wifi, LinkStatus::Unplugged) => Icon::WifiOff,
            (IfKind::Wifi, _) => Icon::wifi(net.wifi.as_ref().and_then(|w| w.active()).map(|a| f32::from(a.strength) / 100.0).or(Some(1.0))),
            (_, LinkStatus::Unplugged) => Icon::EthernetOff,
            _ => Icon::Ethernet,
        },
    }
}

/// The tile is "on" (accent) while there is a link in use.
pub(crate) fn tile_on(net: &NetworkState) -> bool {
    matches!(net.status(), LinkStatus::Connected | LinkStatus::Connecting | LinkStatus::Limited)
}

/// Header status on the right of the section: text and colour role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Ok,
    Accent,
    Warn,
    Muted,
}

pub(crate) fn section_status(net: &NetworkState) -> (String, Tone) {
    let Some(p) = net.primary.as_ref() else { return ("offline".into(), Tone::Muted) };
    let age = net.primary_interface().and_then(|i| i.link_up_secs);
    match p.status {
        LinkStatus::Connected => match age {
            Some(s) => (format!("connected · {}", fmt::uptime(s)), Tone::Ok),
            None => ("connected".into(), Tone::Ok),
        },
        LinkStatus::Connecting => ("connecting".into(), Tone::Accent),
        LinkStatus::Limited => ("limited".into(), Tone::Warn),
        LinkStatus::Unplugged => ("unplugged".into(), Tone::Muted),
        LinkStatus::Disconnected => ("disconnected".into(), Tone::Muted),
        LinkStatus::Unknown => ("…".into(), Tone::Muted),
    }
}

/// `↓ 84.2 MB/s`, `↑ 3.1 MB/s`.
pub(crate) fn rates(net: &NetworkState) -> (String, String) {
    (format!("↓ {}", fmt::rate(net.throughput.rx_bps)), format!("↑ {}", fmt::rate(net.throughput.tx_bps)))
}

/// `today 41.6 G`.
pub(crate) fn today_text(iface: &Interface) -> String {
    format!("today {}", fmt::bytes(iface.today.total()))
}

/// A row of the details grid.
pub(crate) struct Detail {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    pub copy: bool,
}

/// IPv4, gateway, DNS, IPv6, MAC, link — rows that have nothing to say are left out.
pub(crate) fn details(net: &NetworkState, i: &Interface) -> Vec<Detail> {
    let mut v = Vec::new();
    if let Some(a) = i.ipv4.first() {
        v.push(Detail { key: "ipv4", label: "IPv4", value: a.to_string(), copy: true });
    }
    if let Some(g) = i.gateway {
        v.push(Detail { key: "gateway", label: "gateway", value: g.to_string(), copy: false });
    }
    if !net.dns.is_empty() {
        let dns: Vec<String> = net.dns.iter().take(3).map(ToString::to_string).collect();
        v.push(Detail { key: "dns", label: "DNS", value: dns.join(" · "), copy: false });
    }
    if let Some(a) = i.ipv6.first() {
        v.push(Detail { key: "ipv6", label: "IPv6", value: a.to_string(), copy: true });
    }
    if let Some(m) = &i.mac {
        v.push(Detail { key: "mac", label: "MAC", value: m.clone(), copy: true });
    }
    let link = i.link_text();
    if !link.is_empty() {
        v.push(Detail { key: "link", label: "link", value: link, copy: false });
    }
    v
}

/// `VPN · wg0` and its sub line `connected · 10.8.0.2`.
pub(crate) fn vpn_texts(v: &Vpn) -> (String, String) {
    let sub = if v.connected {
        match v.addrs.first() {
            Some(a) => format!("connected · {a}"),
            None => "connected".into(),
        }
    } else {
        "off".into()
    };
    (format!("VPN · {}", v.name), sub)
}

/// Wired profiles first for the Ethernet card.
pub(crate) fn wired_profiles(net: &NetworkState, ifname: &str) -> Vec<Profile> {
    net.profiles
        .iter()
        .filter(|p| p.kind == ProfileKind::Wired && p.ifname.as_deref().is_none_or(|n| n == ifname))
        .cloned()
        .collect()
}

/// Seconds left on the safe-apply banner.
pub(crate) fn seconds_left(deadline: Instant, now: Instant) -> u64 {
    deadline.saturating_duration_since(now).as_secs_f32().ceil() as u64
}

/// Parse the static editor into a config; `Err` is the message to show.
pub(crate) fn parse_static(ifname: &str, name: &str, address: &str, gateway: &str, dns: &str) -> Result<StaticConfig, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the profile a name".into());
    }
    let (addr, prefix) = parse_cidr(address).ok_or("address must look like 10.0.0.5/24")?;
    let gw = gateway.trim();
    let gateway = if gw.is_empty() { None } else { Some(gw.parse().map_err(|_| "gateway is not an IPv4 address")?) };
    let mut servers = Vec::new();
    for part in dns.split([',', ' ']).filter(|s| !s.is_empty()) {
        servers.push(part.parse().map_err(|_| format!("{part} is not an IPv4 address"))?);
    }
    Ok(StaticConfig { name: name.into(), ifname: ifname.into(), address: addr, prefix, gateway, dns: servers })
}

// ── small widgets shared with wifi.rs / bluetooth.rs ──────────────────────────

pub(crate) fn tone_color(t: &ashd_ui::Theme, tone: Tone) -> gpui::Hsla {
    match tone {
        Tone::Ok => t.ok,
        Tone::Accent => t.accent,
        Tone::Warn => t.warn,
        Tone::Muted => t.fg_muted,
    }
}

/// Mono caps text in a given colour at 10 px (headers, states).
pub(crate) fn caps(text: impl Into<SharedString>, color: gpui::Hsla, cx: &gpui::App) -> Div {
    label(text, cx).text_size(px(10.)).text_color(color)
}

pub(crate) fn dot(color: gpui::Hsla) -> Div {
    div().size(px(6.)).flex_none().rounded_full().bg(color)
}

/// The bordered section card: header (title left, status right) over a padded body.
pub(crate) fn card(title: impl Into<SharedString>, right: impl IntoElement, body: Div, cx: &gpui::App) -> Div {
    card_with(title, right, body.p(px(10.)).flex().flex_col().gap(px(10.)), cx)
}

/// [`card`] whose body brings its own padding (rows that run edge to edge).
pub(crate) fn card_with(title: impl Into<SharedString>, right: impl IntoElement, body: Div, cx: &gpui::App) -> Div {
    let t = theme(cx).clone();
    div()
        .flex()
        .flex_col()
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_control)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.line)
                .child(caps(title, t.fg, cx))
                .child(div().flex_1())
                .child(right),
        )
        .child(body)
}

/// 26 px action button: `primary` = accent fill, `ghost` = no border.
pub(crate) fn small_button(id: impl Into<ElementId>, text: &str, primary: bool, ghost: bool, cx: &gpui::App) -> Stateful<Div> {
    let t = theme(cx).clone();
    let s2 = t.surface_2;
    let (bg, fg, border) = if primary {
        (Some(t.accent), t.on_accent, t.accent)
    } else if ghost {
        (None, t.fg, gpui::transparent_black())
    } else {
        (None, t.fg, t.line)
    };
    div()
        .id(id)
        .h(px(26.))
        .px(px(12.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .border_1()
        .border_color(border)
        .rounded(t.radius_control)
        .when_some(bg, |d, c| d.bg(c))
        .when(bg.is_none(), |d| d.hover(move |s| s.bg(s2)))
        .cursor_pointer()
        .font_family(t.font_sans.clone())
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .whitespace_nowrap()
        .child(SharedString::from(text.to_string()))
}

/// 22 px icon button (copy, forget, menu).
pub(crate) fn tiny_icon_button(id: impl Into<ElementId>, ic: Icon, color: gpui::Hsla, cx: &gpui::App) -> Stateful<Div> {
    let t = theme(cx).clone();
    let s2 = t.surface_2;
    div()
        .id(id)
        .size(px(22.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(t.radius_control)
        .cursor_pointer()
        .hover(move |s| s.bg(s2))
        .child(icon(ic, 12.).text_color(color))
}

/// A bordered 32 px text field row (accent border while it is the active one).
pub(crate) fn field(input: &Entity<TextInput>, cx: &gpui::App) -> Div {
    let t = theme(cx).clone();
    div()
        .h(px(32.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_1()
        .border_color(t.accent)
        .bg(t.bg)
        .rounded(t.radius_control)
        .font_family(t.font_mono.clone())
        .text_size(px(12.))
        .text_color(t.fg)
        .child(input.clone())
}

fn sans(cx: &gpui::App) -> Div {
    let t = theme(cx);
    div().font_family(t.font_sans.clone()).text_size(px(metrics::type_scale::UI.0)).text_color(t.fg)
}

fn mono_text(text: impl Into<SharedString>, size: f32, color: gpui::Hsla, cx: &gpui::App) -> Div {
    div().font_family(theme(cx).font_mono.clone()).text_size(px(size)).text_color(color).child(text.into())
}

// ── panel integration ─────────────────────────────────────────────────────────

impl QuickSettingsPanel {
    /// Expand / collapse a connectivity section (accordion). Sampling and the sparkline
    /// scale follow the Ethernet section; leaving Bluetooth stops a running scan.
    pub(crate) fn set_section(&mut self, section: Option<ConnSection>, cx: &mut Context<Self>) {
        let cs = &mut self.state.conn;
        let previous = cs.section;
        cs.section = section;
        cs.scale = None;
        cs.wifi_pick = None;
        cs.editor = None;
        cs.bt_menu = None;
        let net = &self.ctx.services.network.handle;
        net.set_sampling(section == Some(ConnSection::Network));
        if previous == Some(ConnSection::Bluetooth) && section != Some(ConnSection::Bluetooth) {
            self.ctx.services.bluetooth.handle.stop_discovery();
        }
        cx.notify();
    }

    fn toggle_section(&mut self, s: ConnSection, cx: &mut Context<Self>) {
        let next = if self.state.conn.section == Some(s) { None } else { Some(s) };
        self.set_section(next, cx);
    }

    /// Panel closed: nothing stays expanded and the fast sampler stops.
    pub(crate) fn connectivity_on_close(&mut self, cx: &mut Context<Self>) {
        if self.state.conn.section.is_some() {
            self.set_section(None, cx);
        }
        self.state.conn = ConnState::default();
    }

    fn copy(&mut self, key: &'static str, text: String, cx: &mut Context<Self>) {
        self.ctx.shell.spawn(["wl-copy".to_string(), text], cx);
        self.state.conn.copied = Some((key, Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(COPIED_MS + 30)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }

    fn chevron(&self, open: bool, on: bool, cx: &gpui::App) -> impl IntoElement {
        let t = theme(cx);
        icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12.).text_color(if on { t.on_accent } else { t.fg_muted })
    }

    /// The network tile (and the Wi-Fi tile when there is a Wi-Fi device besides it).
    pub(crate) fn network_tiles(&mut self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let net = self.ctx.services.network.state.read(cx).clone();
        if !net.available {
            return Vec::new();
        }
        let section = self.state.conn.section;
        let wifi_primary = net.primary.as_ref().is_some_and(|p| p.kind == IfKind::Wifi) && net.wifi.is_some();
        let target = if wifi_primary { ConnSection::Wifi } else { ConnSection::Network };
        let on = tile_on(&net);
        let mut out = vec![
            tile("tile-network", tile_icon(&net), net.tile_title(), &net.tile_subtitle(), on, cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_section(target, cx)))
                .child(self.chevron(section == Some(target), on, cx)),
        ];
        if let (Some(w), false) = (net.wifi.as_ref(), wifi_primary) {
            let wifi_on = w.enabled && w.active().is_some();
            let sub = match (w.enabled, w.active()) {
                (true, Some(a)) => format!("{} · {}", a.ssid, a.band()),
                (true, None) => format!("{} networks", w.aps.len()),
                (false, _) if net.status() == LinkStatus::Connected => "off · wired".to_string(),
                (false, _) => "off".to_string(),
            };
            let ic = if w.enabled { Icon::wifi(w.active().map(|a| f32::from(a.strength) / 100.0).or(Some(1.0))) } else { Icon::WifiOff };
            out.push(
                tile("tile-wifi", ic, "Wi-Fi", &sub, wifi_on, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_section(ConnSection::Wifi, cx)))
                    .child(self.chevron(section == Some(ConnSection::Wifi), wifi_on, cx)),
            );
        }
        out
    }

    /// "Keep these network settings?" — shown above everything while a manual profile is
    /// being tried.
    pub(crate) fn apply_banner(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let net = self.ctx.services.network.state.read(cx).clone();
        let p = net.pending_apply?;
        let t = theme(cx).clone();
        let now = Instant::now();
        let left = seconds_left(p.deadline, now);
        let total = ashd_svc_network::SAFE_APPLY_SECS as f32;
        let frac = (p.deadline.saturating_duration_since(now).as_secs_f32() / total).clamp(0.0, 1.0);
        // Keep the countdown bar and the seconds ticking.
        window.request_animation_frame();
        let revert_to = p.revert_to.clone().unwrap_or_else(|| "the previous profile".into());
        Some(
            div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(t.line)
                .rounded(t.radius_control)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap(px(10.))
                        .px(px(10.))
                        .py(px(8.))
                        .child(icon(Icon::Ethernet, 15.).text_color(t.fg).flex_none())
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(250.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(sans(cx).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Keep these network settings?"))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap(px(4.))
                                        .font_family(t.font_sans.clone())
                                        .text_size(px(12.))
                                        .text_color(t.fg_muted)
                                        .child(format!("Reverting to {revert_to} in"))
                                        .child(mono_text(format!("{left} s"), 12., t.accent, cx))
                                        .child("if you lose access."),
                                ),
                        )
                        .child(
                            small_button("net-revert", "Revert", false, true, cx)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, _| this.ctx.services.network.handle.revert_apply())),
                        )
                        .child(
                            small_button("net-keep", "Keep", true, false, cx)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, _| this.ctx.services.network.handle.keep_apply())),
                        ),
                )
                .child(
                    div()
                        .h(px(2.))
                        .bg(t.line)
                        .child(div().h(px(2.)).w(gpui::relative(frac)).bg(t.accent)),
                ),
        )
    }

    /// The expanded section below the tile grid (network / Wi-Fi / Bluetooth) with the VPN
    /// rows under the network ones.
    pub(crate) fn connectivity_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        match self.state.conn.section? {
            ConnSection::Network => self.network_section(window, cx),
            ConnSection::Wifi => self.wifi_section(window, cx),
            ConnSection::Bluetooth => self.bluetooth_section(window, cx),
        }
    }

    fn network_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let net = self.ctx.services.network.state.read(cx).clone();
        if !net.available {
            return None;
        }
        let t = theme(cx).clone();
        let mut out = div().flex().flex_col().gap(px(6.));
        if let (Some(p), Some(iface)) = (net.primary.as_ref(), net.primary_interface()) {
            let title = format!("{} · {}", if p.kind == IfKind::Wifi { "wi-fi" } else { "ethernet" }, p.ifname);
            let (text, tone) = section_status(&net);
            let color = tone_color(&t, tone);
            let right = if p.status == LinkStatus::Connecting {
                div().flex().items_center().gap(px(6.)).child(ashd_ui::spinner("net-hdr-spin", 12., t.accent)).child(caps(text, color, cx))
            } else {
                div().flex().items_center().gap(px(6.)).child(dot(color)).child(caps(text, color, cx))
            };
            let body = match p.status {
                LinkStatus::Connected => self.connected_body(&net, iface, window, cx),
                LinkStatus::Connecting | LinkStatus::Limited => self.steps_body(&net, cx),
                LinkStatus::Unplugged | LinkStatus::Unknown => self.unplugged_body(&net, iface, cx),
                LinkStatus::Disconnected => self.disconnected_body(&net, iface, cx),
            };
            out = out.child(card(title, right, body, cx));
        } else {
            out = out.child(card(
                "network",
                div().flex().items_center().gap(px(6.)).child(dot(t.fg_muted)).child(caps("offline", t.fg_muted, cx)),
                div().child(label("no network interface found", cx)),
                cx,
            ));
        }
        for v in &net.vpns {
            out = out.child(self.vpn_row(v, cx));
        }
        Some(out)
    }

    fn vpn_row(&self, v: &Vpn, cx: &gpui::App) -> Div {
        let t = theme(cx).clone();
        let (title, sub) = vpn_texts(v);
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .py(px(8.))
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_control)
            .child(icon(Icon::Shield, 15.).text_color(if v.connected { t.accent } else { t.fg_muted }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(sans(cx).truncate().child(title))
                    .child(caps(sub, t.fg_muted, cx)),
            )
            .when(v.default_route, |d| d.child(caps("all traffic", t.fg_muted, cx)))
    }

    fn connected_body(&mut self, net: &NetworkState, iface: &Interface, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let (down, up) = rates(net);
        let mut body = div().child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .child(div().flex().gap(px(14.)).child(mono_text(down, 12., t.accent, cx)).child(mono_text(up, 12., t.fg_muted, cx)))
                .child(label(today_text(iface), cx)),
        );

        // Sparkline: scale latched per view; frames only while it is on screen.
        let th = &net.throughput;
        let scale = sparkline::update_latch(&mut self.state.conn.scale, th.history.len(), th.window_max_bps);
        let frac = th.sampled_at.map(|at| at.elapsed().as_secs_f32() / th.interval.as_secs_f32().max(0.001)).unwrap_or(0.0);
        if net.sampling {
            window.request_animation_frame();
        }
        body = body.child(
            div()
                .relative()
                .w_full()
                .h(px(sparkline::HEIGHT))
                .overflow_hidden()
                .border_b_1()
                .border_color(t.line)
                .child(sparkline::chart(th.history.clone(), frac, scale, t.accent, t.fg_muted)),
        );

        // Details grid.
        let copied = self.state.conn.copied.filter(|(_, at)| at.elapsed() < Duration::from_millis(COPIED_MS)).map(|(k, _)| k);
        let mut grid = div().flex().flex_col();
        for (dix, d) in details(net, iface).into_iter().enumerate() {
            let key = d.key;
            let value = d.value.clone();
            let copy: gpui::AnyElement = if d.copy {
                let done = copied == Some(key);
                tiny_icon_button(ElementId::from(("net-copy", dix as u32)), if done { Icon::Check } else { Icon::Copy }, if done { t.ok } else { t.fg_muted }, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.copy(key, value.clone(), cx)))
                    .into_any_element()
            } else {
                div().into_any_element()
            };
            grid = grid.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(24.))
                    .font_family(t.font_mono.clone())
                    .text_size(px(11.5))
                    .child(div().w(px(74.)).flex_none().text_color(t.fg_muted).child(d.label))
                    .child(div().flex_1().min_w_0().truncate().text_color(t.fg).child(d.value))
                    .child(copy),
            );
        }
        body = body.child(grid);

        if net.can_configure {
            body = self.profiles_block(body, net, &iface.name, window, cx);
            body = body.child(
                div()
                    .flex()
                    .gap(px(6.))
                    .flex_wrap()
                    .child(small_button("net-settings", "Settings…", false, false, cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.ctx.shell.spawn(["nm-connection-editor"], cx);
                        this.ctx.shell.close_modal(cx);
                    })))
                    .child({
                        let name = iface.name.clone();
                        small_button("net-disconnect", "Disconnect", false, true, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, _| this.ctx.services.network.handle.disconnect_device(&name)))
                    }),
            );
        }
        body
    }

    /// Profile radio list + "New profile…" + the static editor (NetworkManager only).
    fn profiles_block(&mut self, body: Div, net: &NetworkState, ifname: &str, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let profiles = wired_profiles(net, ifname);
        let mut list = div().flex().flex_col().gap(px(2.)).child(label("profile", cx));
        for (ix, p) in profiles.iter().enumerate() {
            let uuid = p.uuid.clone();
            let forget = p.uuid.clone();
            let s2 = t.surface_2;
            list = list.child(
                div()
                    .id(ElementId::from(("net-profile", ix as u32)))
                    .group("profile-row")
                    .h(px(metrics::ROW_H))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .border_1()
                    .border_color(if p.active { t.accent } else { gpui::transparent_black() })
                    .rounded(t.radius_control)
                    .cursor_pointer()
                    .when(!p.active, |d| d.hover(move |s| s.bg(s2)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, _| this.ctx.services.network.handle.activate_profile(&uuid)))
                    .child(
                        div()
                            .size(px(12.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_1()
                            .border_color(if p.active { t.accent } else { t.fg_muted })
                            .when(p.active, |d| d.child(div().size(px(6.)).rounded_full().bg(t.accent))),
                    )
                    .child(sans(cx).flex_1().min_w_0().truncate().child(p.id.clone()))
                    .child(caps(p.summary(), t.fg_muted, cx))
                    .when(!p.active, |d| {
                        d.child(
                            tiny_icon_button(ElementId::from(("net-forget", ix as u32)), Icon::Trash, t.fg_muted, cx)
                                .invisible()
                                .group_hover("profile-row", |s| s.visible())
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.ctx.services.network.handle.forget_profile(&forget);
                                })),
                        )
                    }),
            );
        }
        if self.state.conn.editor.is_none() {
            list = list.child(
                div()
                    .id("net-new-profile")
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.line)
                    .rounded(t.radius_control)
                    .cursor_pointer()
                    .text_color(t.fg_muted)
                    .font_family(t.font_sans.clone())
                    .text_size(px(12.))
                    .child(icon(Icon::Plus, 12.).text_color(t.fg_muted))
                    .child("New profile…")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_editor(window, cx))),
            );
        }
        let mut body = body.child(list);
        if self.state.conn.editor.is_some() {
            body = body.child(self.editor_view(ifname, window, cx));
        }
        body
    }

    fn open_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mk = |ph: &'static str, cx: &mut Context<Self>| {
            let input = cx.new(|cx| TextInput::new(ph, cx));
            cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &TextInputEvent, window, cx| match ev {
                TextInputEvent::Tab | TextInputEvent::Down => window.focus_next(cx),
                TextInputEvent::Up => window.focus_prev(cx),
                TextInputEvent::Submit => this.submit_editor(cx),
                TextInputEvent::Cancel => {
                    this.state.conn.editor = None;
                    cx.notify();
                }
                _ => {}
            })
            .detach();
            input
        };
        let editor = Editor { name: mk("name", cx), address: mk("10.0.0.5/24", cx), gateway: mk("gateway · 10.0.0.1", cx), dns: mk("dns · 1.1.1.1, 9.9.9.9", cx), error: None };
        editor.name.focus_handle(cx).focus(window, cx);
        self.state.conn.editor = Some(editor);
        cx.notify();
    }

    fn submit_editor(&mut self, cx: &mut Context<Self>) {
        let net = self.ctx.services.network.state.read(cx).clone();
        let Some(ifname) = net.primary.as_ref().map(|p| p.ifname.clone()) else { return };
        let Some(ed) = self.state.conn.editor.as_mut() else { return };
        let (name, addr, gw, dns) = (ed.name.read(cx).text().to_string(), ed.address.read(cx).text().to_string(), ed.gateway.read(cx).text().to_string(), ed.dns.read(cx).text().to_string());
        match parse_static(&ifname, &name, &addr, &gw, &dns) {
            Ok(cfg) => {
                self.ctx.services.network.handle.apply_static(cfg);
                self.state.conn.editor = None;
            }
            Err(e) => ed.error = Some(e),
        }
        cx.notify();
    }

    fn editor_view(&mut self, ifname: &str, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let ed = self.state.conn.editor.as_ref().expect("editor open");
        let rows = [("name", ed.name.clone()), ("address", ed.address.clone()), ("gateway", ed.gateway.clone()), ("dns", ed.dns.clone())];
        let error = ed.error.clone();
        let mut col = div().flex().flex_col().gap(px(6.)).child(label(format!("new static profile · {ifname}"), cx));
        for (name, input) in rows {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().w(px(56.)).flex_none().child(label(name, cx)))
                    .child(div().flex_1().min_w_0().child(field(&input, cx))),
            );
        }
        col.when_some(error, |d, e| d.child(div().text_size(px(11.5)).text_color(t.err).font_family(t.font_sans.clone()).child(e)))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(small_button("net-editor-apply", "Apply", true, false, cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit_editor(cx))))
                    .child(small_button("net-editor-cancel", "Cancel", false, true, cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.conn.editor = None;
                        cx.notify();
                    }))),
            )
    }

    /// Connecting / limited: the step list (+ hint + actions that need NetworkManager).
    fn steps_body(&mut self, net: &NetworkState, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let p = net.primary.as_ref().expect("primary");
        let mut body = div();
        for (ix, s) in p.steps.iter().enumerate() {
            let (glyph, label_color): (gpui::AnyElement, gpui::Hsla) = match s.status {
                StepStatus::Done => (icon(Icon::Check, 13.).text_color(t.ok).into_any_element(), t.fg),
                StepStatus::Active => (ashd_ui::spinner(ElementId::from(("net-step-spin", ix as u32)), 12., t.accent).into_any_element(), t.fg),
                StepStatus::Pending => (
                    div().size(px(12.)).flex_none().rounded_full().border_1().border_color(t.line).into_any_element(),
                    t.fg_muted,
                ),
                StepStatus::Warn => (icon(Icon::AlertTriangle, 13.).text_color(t.warn).into_any_element(), t.warn),
                StepStatus::Fail => (icon(Icon::Close, 13.).text_color(t.err).into_any_element(), t.err),
            };
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(30.))
                    .child(div().w(px(13.)).flex_none().flex().justify_center().child(glyph))
                    .child(sans(cx).flex_1().min_w_0().truncate().text_color(label_color).child(s.label.clone()))
                    .child(mono_text(s.detail.clone(), 11., t.fg_muted, cx)),
            );
        }
        if let Some(h) = &p.hint {
            body = body.child(div().font_family(t.font_sans.clone()).text_size(px(12.)).line_height(px(18.)).text_color(t.fg_muted).child(h.clone()));
        }
        if net.can_configure {
            let name = p.ifname.clone();
            let mut row = div().flex().gap(px(6.)).flex_wrap();
            if p.status == LinkStatus::Limited {
                let n2 = name.clone();
                row = row
                    .child(
                        small_button("net-retry", "Retry DHCP", true, false, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, _| this.ctx.services.network.handle.reapply(&n2))),
                    )
                    .child(small_button("net-diagnose", "Diagnose", false, true, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.ctx.shell.spawn(["kitty", "--hold", "nmcli", "device", "show"], cx);
                        this.ctx.shell.close_modal(cx);
                    })));
            } else {
                row = row.child(
                    small_button("net-cancel", "Cancel", false, true, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, _| this.ctx.services.network.handle.disconnect_device(&name))),
                );
            }
            body = body.child(row);
        }
        body
    }

    fn unplugged_body(&mut self, net: &NetworkState, iface: &Interface, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let wifi = iface.kind == IfKind::Wifi;
        let (headline, sub) = if wifi {
            (format!("{} is not connected", iface.name), "Pick a network from the Wi-Fi list.".to_string())
        } else {
            let tail = match net.profiles.iter().find(|p| p.autoconnect && p.kind == ProfileKind::Wired) {
                Some(p) if net.can_configure => format!("Plug a cable in and ashd connects with the {} profile automatically.", p.id),
                _ => "Plug a cable in — the network comes up on its own.".to_string(),
            };
            (format!("No cable in {}", iface.name), tail)
        };
        let mut col = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .px(px(8.))
            .py(px(18.))
            .child(icon(if wifi { Icon::WifiOff } else { Icon::EthernetOff }, 26.).text_color(t.fg_muted))
            .child(sans(cx).text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(headline))
            .child(
                div()
                    .max_w(px(280.))
                    .text_center()
                    .font_family(t.font_sans.clone())
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(t.fg_muted)
                    .child(sub),
            );
        if net.can_configure && net.wifi.is_some() && !wifi {
            col = col.child(small_button("net-use-wifi", "Use Wi-Fi instead", false, false, cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_section(Some(ConnSection::Wifi), cx))));
        }
        div().child(col)
    }

    fn disconnected_body(&mut self, net: &NetworkState, iface: &Interface, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let name = iface.name.clone();
        let mut col = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .py(px(14.))
            .child(icon(Icon::Ethernet, 22.).text_color(t.fg_muted))
            .child(sans(cx).font_weight(FontWeight::SEMIBOLD).child("Disconnected"))
            .child(div().font_family(t.font_sans.clone()).text_size(px(12.)).text_color(t.fg_muted).child("The cable is plugged in, but the link is not in use."));
        if net.can_configure {
            col = col.child(
                small_button("net-connect", "Connect", true, false, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, _| this.ctx.services.network.handle.connect_device(&name))),
            );
        }
        div().child(col)
    }
}

#[cfg(test)]
mod tests {
    use ashd_svc_network::mock::scenario;

    use super::*;

    #[test]
    fn tiles_follow_the_primary_connection() {
        let c = scenario("connected").unwrap();
        assert_eq!((tile_icon(&c), tile_on(&c)), (Icon::Ethernet, true));
        let u = scenario("unplugged").unwrap();
        assert_eq!((tile_icon(&u), tile_on(&u)), (Icon::EthernetOff, false));
        let o = scenario("offline").unwrap();
        assert_eq!((tile_icon(&o), tile_on(&o)), (Icon::GlobeOff, false));
        let l = scenario("limited").unwrap();
        assert!(tile_on(&l));
        let w = scenario("wifi").unwrap();
        assert_eq!(tile_icon(&w), Icon::Wifi);
        assert_eq!(w.tile_subtitle(), "home · 5 GHz");
    }

    #[test]
    fn section_status_texts() {
        assert_eq!(section_status(&scenario("connected").unwrap()), ("connected · 4d 02h".to_string(), Tone::Ok));
        assert_eq!(section_status(&scenario("connecting").unwrap()), ("connecting".to_string(), Tone::Accent));
        assert_eq!(section_status(&scenario("limited").unwrap()), ("limited".to_string(), Tone::Warn));
        assert_eq!(section_status(&scenario("unplugged").unwrap()), ("unplugged".to_string(), Tone::Muted));
        assert_eq!(section_status(&scenario("offline").unwrap()).0, "offline");
    }

    #[test]
    fn rates_and_today() {
        let c = scenario("connected").unwrap();
        assert_eq!(rates(&c), ("↓ 84.2 MB/s".to_string(), "↑ 3.1 MB/s".to_string()));
        assert_eq!(today_text(c.primary_interface().unwrap()), "today 41.6 G");
    }

    #[test]
    fn details_grid_matches_the_mockup() {
        let c = scenario("connected").unwrap();
        let rows: Vec<(String, String, bool)> = details(&c, c.primary_interface().unwrap()).into_iter().map(|d| (d.label.to_string(), d.value, d.copy)).collect();
        assert_eq!(
            rows,
            vec![
                ("IPv4".to_string(), "192.168.1.24/24".to_string(), true),
                ("gateway".to_string(), "192.168.1.1".to_string(), false),
                ("DNS".to_string(), "1.1.1.1 · 9.9.9.9".to_string(), false),
                ("IPv6".to_string(), "2a02:8070::4c1d:a1/64".to_string(), true),
                ("MAC".to_string(), "3c:7c:3f:5e:12:a1".to_string(), true),
                ("link".to_string(), "1000 Mb/s · full duplex · MTU 1500".to_string(), false),
            ]
        );
        // Nothing to say → no rows.
        let u = scenario("unplugged").unwrap();
        let rows = details(&u, u.primary_interface().unwrap());
        assert!(rows.iter().all(|d| d.label == "MAC" || d.label == "link"));
    }

    #[test]
    fn vpn_row_texts() {
        let v = scenario("vpn").unwrap();
        assert_eq!(vpn_texts(&v.vpns[1]), ("VPN · wg0".to_string(), "connected · 10.8.0.2".to_string()));
        assert_eq!(vpn_texts(&v.vpns[0]).0, "VPN · tailscale0");
    }

    #[test]
    fn profiles_listed_for_the_interface() {
        let n = scenario("nm").unwrap();
        let p = wired_profiles(&n, "enp5s0");
        assert_eq!(p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["Home", "Lab"]);
        assert!(wired_profiles(&n, "eth9").is_empty());
    }

    #[test]
    fn countdown_seconds_round_up() {
        let now = Instant::now();
        assert_eq!(seconds_left(now + Duration::from_millis(12_300), now), 13);
        assert_eq!(seconds_left(now, now + Duration::from_secs(1)), 0);
    }

    #[test]
    fn static_editor_validation() {
        let ok = parse_static("enp5s0", "Lab", "10.0.0.5/24", "10.0.0.1", "1.1.1.1, 9.9.9.9").unwrap();
        assert_eq!((ok.prefix, ok.dns.len(), ok.gateway.is_some()), (24, 2, true));
        assert!(parse_static("e", "", "10.0.0.5/24", "", "").is_err());
        assert_eq!(parse_static("e", "x", "10.0.0.5", "", "").unwrap_err(), "address must look like 10.0.0.5/24");
        assert!(parse_static("e", "x", "10.0.0.5/24", "nope", "").is_err());
        assert!(parse_static("e", "x", "10.0.0.5/24", "", "1.1.1").unwrap_err().contains("1.1.1"));
        assert!(parse_static("e", "x", "10.0.0.5/24", "", "").unwrap().gateway.is_none());
    }
}
