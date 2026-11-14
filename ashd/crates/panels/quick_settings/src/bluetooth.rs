//! Bluetooth tile and section of the quick-settings panel (det-06 / det-12 tiles):
//! device rows (34 px: type icon, name, `connected` / `paired`, battery %), a Scan control
//! with a spinner, a small per-device menu (forget, trust) and the inline pairing prompt
//! fed by the service's pairing agent. Hidden entirely while BlueZ is unavailable.

use gpui::{
    Focusable as _,
    ClickEvent, Context, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::*, px,
};
use ashd_svc_bluetooth::{BluetoothState, Busy, Device, DeviceKind, PairingKind, PairingReply, PairingRequest};
use ashd_tokens::metrics;
use ashd_ui::{Icon, TextInput, TextInputEvent, icon, label, theme};

use crate::{
    QuickSettingsPanel, tile,
    network::{ConnSection, Tone, caps, card_with, field, small_button, tiny_icon_button, tone_color},
};

pub(crate) fn device_icon(kind: DeviceKind) -> Icon {
    match kind {
        DeviceKind::Headphones | DeviceKind::Headset => Icon::Headphones,
        DeviceKind::Speaker => Icon::Volume,
        DeviceKind::Keyboard => Icon::Keyboard,
        DeviceKind::Computer => Icon::Monitor,
        _ => Icon::Bluetooth,
    }
}

/// The caps state at the right of a row (`None` for a stranger in range).
pub(crate) fn state_text(d: &Device) -> Option<(&'static str, Tone)> {
    match (d.busy, d.connected, d.paired) {
        (Some(Busy::Connecting), _, _) => Some(("connecting", Tone::Accent)),
        (Some(Busy::Disconnecting), _, _) => Some(("disconnecting", Tone::Muted)),
        (Some(Busy::Pairing), _, _) => Some(("pairing", Tone::Accent)),
        (None, true, _) => Some(("connected", Tone::Accent)),
        (None, false, true) => Some(("paired", Tone::Muted)),
        _ => None,
    }
}

/// What a click on a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowAction {
    Disconnect,
    Connect,
    Pair,
    Nothing,
}

pub(crate) fn row_action(d: &Device) -> RowAction {
    match (d.busy, d.connected, d.paired) {
        (Some(_), _, _) => RowAction::Nothing,
        (None, true, _) => RowAction::Disconnect,
        (None, false, true) => RowAction::Connect,
        (None, false, false) => RowAction::Pair,
    }
}

/// `482913` → `482 913`.
pub(crate) fn format_passkey(p: u32) -> String {
    let s = format!("{p:06}");
    format!("{} {}", &s[..3], &s[3..])
}

/// Prompt wording: the question and, for codes, the code to show big.
pub(crate) fn prompt_text(r: &PairingRequest) -> (String, Option<String>) {
    let who = if r.name.is_empty() { r.address.clone() } else { r.name.clone() };
    match &r.kind {
        PairingKind::Confirm { passkey } => (format!("Does this code match the one on {who}?"), Some(format_passkey(*passkey))),
        PairingKind::DisplayPasskey { passkey, .. } => (format!("Type this code on {who}, then press Enter."), Some(format_passkey(*passkey))),
        PairingKind::DisplayPin { pin } => (format!("Type this PIN on {who}."), Some(pin.clone())),
        PairingKind::RequestPin => (format!("Enter the PIN for {who}."), None),
        PairingKind::RequestPasskey => (format!("Enter the 6-digit passkey for {who}."), None),
        PairingKind::Authorize => (format!("{who} wants to pair or connect."), None),
        PairingKind::AuthorizeService { .. } => (format!("{who} asks to use a service."), None),
    }
}

fn battery_text(d: &Device) -> Option<String> {
    d.battery.map(|b| format!("{b}%"))
}

fn title(s: &BluetoothState) -> String {
    match s.devices.len() {
        0 => "bluetooth".into(),
        1 => "bluetooth · 1 device".into(),
        n => format!("bluetooth · {n} devices"),
    }
}

impl QuickSettingsPanel {
    /// The Bluetooth tile; `None` while BlueZ is unavailable.
    pub(crate) fn bluetooth_tile(&mut self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let bt = self.ctx.services.bluetooth.state.read(cx).clone();
        if !bt.available {
            return None;
        }
        let t = theme(cx).clone();
        let on = bt.powered();
        let open = self.state.conn.section == Some(ConnSection::Bluetooth);
        let fg = if on { t.on_accent } else { t.fg_muted };
        Some(
            tile("tile-bluetooth", if on { Icon::Bluetooth } else { Icon::BluetoothOff }, "Bluetooth", &bt.tile_subtitle(), on, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    let next = if this.state.conn.section == Some(ConnSection::Bluetooth) { None } else { Some(ConnSection::Bluetooth) };
                    this.set_section(next, cx);
                }))
                .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12.).text_color(fg)),
        )
    }

    fn pick_device(&mut self, d: &Device, cx: &mut Context<Self>) {
        let h = self.ctx.services.bluetooth.handle.clone();
        match row_action(d) {
            RowAction::Disconnect => h.disconnect(&d.path),
            RowAction::Connect => h.connect(&d.path),
            RowAction::Pair => h.pair(&d.path),
            RowAction::Nothing => {}
        }
        self.state.conn.bt_menu = None;
        cx.notify();
    }

    pub(crate) fn bluetooth_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let bt = self.ctx.services.bluetooth.state.read(cx).clone();
        if !bt.available {
            return None;
        }
        let t = theme(cx).clone();
        let s2 = t.surface_2;
        let handle = self.ctx.services.bluetooth.handle.clone();

        // Header: scan control + power toggle.
        let scan: gpui::AnyElement = if !bt.powered() {
            div().into_any_element()
        } else if bt.scanning() {
            div()
                .id("bt-scan")
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .child(ashd_ui::spinner("bt-scan-spin", 12., t.accent))
                .child(caps("scanning", t.accent, cx))
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| this.ctx.services.bluetooth.handle.stop_discovery()))
                .into_any_element()
        } else {
            small_button("bt-scan", "Scan", false, true, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| this.ctx.services.bluetooth.handle.start_discovery()))
                .into_any_element()
        };
        let power = ashd_ui::toggle("bt-power", bt.powered(), { let h = handle.clone(); move |on, _, _| h.set_powered(*on) }, cx);
        let header_right = div().flex().items_center().gap(px(10.)).child(scan).child(power);

        let mut body = div().flex().flex_col();
        if !bt.powered() {
            body = body.child(div().px(px(10.)).py(px(12.)).child(label("bluetooth is off", cx)));
        } else {
            if bt.devices.is_empty() && bt.pairing.is_none() {
                body = body.child(div().px(px(10.)).py(px(12.)).child(label("no devices · scan to find some", cx)));
            }
            // A pairing prompt for a device that is not listed (yet) goes first.
            let listed = bt.pairing.as_ref().is_some_and(|p| bt.device(&p.path).is_some());
            if let (Some(p), false) = (bt.pairing.as_ref(), listed) {
                body = body.child(self.pairing_prompt(p, window, cx));
            }
            for (ix, d) in bt.devices.iter().enumerate() {
                let menu_open = self.state.conn.bt_menu.as_deref() == Some(d.path.as_str());
                let dev = d.clone();
                let state = state_text(d);
                let mut row = div()
                    .id(ElementId::from(("bt-dev", ix as u32)))
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(row_action(d) != RowAction::Nothing, |r| r.cursor_pointer())
                    .when(menu_open, |r| r.bg(t.surface_2))
                    .when(!menu_open, |r| r.hover(move |s| s.bg(s2)))
                    .child(icon(device_icon(d.kind), 14.).text_color(if d.connected { t.accent } else { t.fg }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(t.font_sans.clone())
                            .text_size(px(metrics::type_scale::UI.0))
                            .text_color(t.fg)
                            .child(d.label().to_string()),
                    );
                if let Some(b) = battery_text(d) {
                    row = row.child(caps(b, t.fg_muted, cx));
                }
                row = match (d.busy, state) {
                    (Some(_), Some((txt, tone))) => row.child(ashd_ui::spinner(ElementId::from(("bt-busy", ix as u32)), 12., t.accent)).child(caps(txt, tone_color(&t, tone), cx)),
                    (None, Some((txt, tone))) => row.child(caps(txt, tone_color(&t, tone), cx)),
                    _ => row,
                };
                if d.paired {
                    let path = d.path.clone();
                    row = row.child(
                        tiny_icon_button(ElementId::from(("bt-menu", ix as u32)), Icon::Dots, t.fg_muted, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.state.conn.bt_menu = if this.state.conn.bt_menu.as_deref() == Some(path.as_str()) { None } else { Some(path.clone()) };
                            cx.notify();
                        })),
                    );
                }
                body = body.child(row.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_device(&dev, cx))));

                if menu_open {
                    let (p1, p2, trusted) = (d.path.clone(), d.path.clone(), d.trusted);
                    body = body.child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .px(px(10.))
                            .pb(px(8.))
                            .bg(t.surface_2)
                            .child(small_button(ElementId::from(("bt-trust", ix as u32)), if trusted { "Untrust" } else { "Trust" }, false, false, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ctx.services.bluetooth.handle.set_trusted(&p1, !trusted);
                                this.state.conn.bt_menu = None;
                                cx.notify();
                            })))
                            .child(small_button(ElementId::from(("bt-forget", ix as u32)), "Forget", false, true, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ctx.services.bluetooth.handle.remove(&p2);
                                this.state.conn.bt_menu = None;
                                cx.notify();
                            }))),
                    );
                }
                if let Some(p) = bt.pairing.as_ref().filter(|p| p.path == d.path) {
                    body = body.child(self.pairing_prompt(p, window, cx));
                }
            }
        }
        if let Some(e) = &bt.error {
            body = body.child(div().px(px(10.)).py(px(8.)).text_size(px(11.5)).font_family(t.font_sans.clone()).text_color(t.err).child(e.clone()));
        }
        Some(div().flex().flex_col().gap(px(6.)).child(card_with(title(&bt), header_right, body, cx)))
    }

    /// The inline prompt under the device being paired.
    fn pairing_prompt(&mut self, req: &PairingRequest, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let (text, code) = prompt_text(req);
        let seq = req.seq;
        let needs_input = matches!(req.kind, PairingKind::RequestPin | PairingKind::RequestPasskey);
        let passkey_entry = matches!(req.kind, PairingKind::RequestPasskey);
        if needs_input && self.state.conn.bt_input.is_none() {
            let input = cx.new(|cx| TextInput::new(if passkey_entry { "000000" } else { "PIN" }, cx));
            cx.subscribe_in(&input, window, move |this: &mut Self, input, ev: &TextInputEvent, _, cx| match ev {
                TextInputEvent::Submit => this.answer_entry(seq, passkey_entry, input.read(cx).text().to_string(), cx),
                TextInputEvent::Cancel => {
                    this.ctx.services.bluetooth.handle.reply_pairing(seq, PairingReply::Reject);
                    this.state.conn.bt_input = None;
                    cx.notify();
                }
                _ => {}
            })
            .detach();
            input.focus_handle(cx).focus(window, cx);
            self.state.conn.bt_input = Some(input);
        }
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(10.))
            .pt(px(6.))
            .pb(px(10.))
            .bg(t.surface_2)
            .child(div().font_family(t.font_sans.clone()).text_size(px(12.)).text_color(t.fg).child(text));
        if let Some(code) = code {
            let digits = match &req.kind {
                PairingKind::DisplayPasskey { entered, .. } => *entered as usize,
                _ => 0,
            };
            // Typed digits are dimmed (the space counts as no digit).
            let shown: Vec<gpui::AnyElement> = code
                .chars()
                .scan(0usize, |n, c| {
                    let dim = c != ' ' && *n < digits;
                    if c != ' ' {
                        *n += 1;
                    }
                    Some(div().text_color(if dim { t.fg_muted } else { t.accent }).child(c.to_string()).into_any_element())
                })
                .collect();
            col = col.child(div().flex().font_family(t.font_mono.clone()).text_size(px(20.)).font_weight(gpui::FontWeight::MEDIUM).children(shown));
        }
        if needs_input && let Some(input) = self.state.conn.bt_input.clone() {
            col = col.child(field(&input, cx));
        }
        let buttons = match &req.kind {
            PairingKind::Confirm { .. } => Some(("Reject", "Pair")),
            PairingKind::Authorize | PairingKind::AuthorizeService { .. } => Some(("Deny", "Allow")),
            PairingKind::RequestPin | PairingKind::RequestPasskey => Some(("Cancel", "OK")),
            _ => None,
        };
        if let Some((no, yes)) = buttons {
            col = col.child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(small_button("bt-prompt-yes", yes, true, false, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if needs_input {
                            let text = this.state.conn.bt_input.as_ref().map(|i| i.read(cx).text().to_string()).unwrap_or_default();
                            this.answer_entry(seq, passkey_entry, text, cx);
                        } else {
                            this.ctx.services.bluetooth.handle.reply_pairing(seq, PairingReply::Accept);
                        }
                    })))
                    .child(small_button("bt-prompt-no", no, false, true, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.ctx.services.bluetooth.handle.reply_pairing(seq, PairingReply::Reject);
                        this.state.conn.bt_input = None;
                        cx.notify();
                    }))),
            );
        }
        col
    }

    fn answer_entry(&mut self, seq: u64, passkey: bool, text: String, cx: &mut Context<Self>) {
        let h = &self.ctx.services.bluetooth.handle;
        let reply = if passkey {
            match text.trim().parse::<u32>() {
                Ok(p) if p <= 999_999 => PairingReply::Passkey(p),
                _ => return,
            }
        } else {
            PairingReply::Pin(text.trim().to_string())
        };
        h.reply_pairing(seq, reply);
        self.state.conn.bt_input = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use ashd_svc_bluetooth::mock::scenario;

    use super::*;

    #[test]
    fn rows_and_actions() {
        let s = scenario("devices").unwrap();
        let by = |n: &str| s.devices.iter().find(|d| d.label() == n).unwrap().clone();
        let hp = by("WH-1000XM5");
        assert_eq!(state_text(&hp), Some(("connected", Tone::Accent)));
        assert_eq!(row_action(&hp), RowAction::Disconnect);
        assert_eq!(battery_text(&hp).as_deref(), Some("82%"));
        let phone = by("Pixel 8");
        assert_eq!(state_text(&phone), Some(("paired", Tone::Muted)));
        assert_eq!(row_action(&phone), RowAction::Connect);
        let mouse = by("MX Anywhere 3");
        assert_eq!(state_text(&mouse), None);
        assert_eq!(row_action(&mouse), RowAction::Pair);
        let mut busy = mouse;
        busy.busy = Some(Busy::Pairing);
        assert_eq!((row_action(&busy), state_text(&busy)), (RowAction::Nothing, Some(("pairing", Tone::Accent))));
    }

    #[test]
    fn icons_by_kind() {
        assert_eq!(device_icon(DeviceKind::Headphones), Icon::Headphones);
        assert_eq!(device_icon(DeviceKind::Keyboard), Icon::Keyboard);
        assert_eq!(device_icon(DeviceKind::Mouse), Icon::Bluetooth);
    }

    #[test]
    fn passkey_formatting_and_prompt_wording() {
        assert_eq!(format_passkey(482_913), "482 913");
        assert_eq!(format_passkey(42), "000 042");
        let s = scenario("pairing").unwrap();
        let (q, code) = prompt_text(s.pairing.as_ref().unwrap());
        assert!(q.contains("match") && q.contains("MX Anywhere 3"));
        assert_eq!(code.as_deref(), Some("482 913"));
        let d = scenario("pairing-display").unwrap();
        assert_eq!(prompt_text(d.pairing.as_ref().unwrap()).1.as_deref(), Some("118 204"));
    }

    #[test]
    fn titles() {
        assert_eq!(title(&scenario("devices").unwrap()), "bluetooth · 4 devices");
        assert_eq!(title(&scenario("on").unwrap()), "bluetooth");
    }
}
