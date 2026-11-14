//! The Wi-Fi list of the quick-settings panel (det-06): signal bars, name, `connected`,
//! a lock for secured networks, and an inline masked password field under the row being
//! connected. Needs NetworkManager (the service only reports Wi-Fi then).

use gpui::{
    Focusable as _,
    ClickEvent, Context, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, Window, div, prelude::*, px,
};
use ashd_svc_network::{AccessPoint, WifiState};
use ashd_ui::{Icon, TextInput, TextInputEvent, icon, label, theme};

use crate::{
    QuickSettingsPanel,
    network::{caps, card_with, field, tiny_icon_button},
};

/// Bar heights of the signal meter (px).
const BAR_H: [f32; 4] = [4., 7., 10., 13.];

/// Section title: `wi-fi · 5 networks`.
pub(crate) fn wifi_title(w: &WifiState) -> String {
    if !w.enabled {
        return "wi-fi · off".into();
    }
    match w.aps.len() {
        1 => "wi-fi · 1 network".into(),
        n => format!("wi-fi · {n} networks"),
    }
}

/// What a click on a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowAction {
    Disconnect,
    Connect,
    AskPassword,
}

pub(crate) fn row_action(ap: &AccessPoint) -> RowAction {
    if ap.active {
        RowAction::Disconnect
    } else if ap.known || !ap.secured() {
        RowAction::Connect
    } else {
        RowAction::AskPassword
    }
}

impl QuickSettingsPanel {
    fn pick_network(&mut self, ap: &AccessPoint, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.ctx.services.network.handle.clone();
        match row_action(ap) {
            RowAction::Disconnect => handle.wifi_disconnect(),
            RowAction::Connect => {
                self.state.conn.wifi_pick = None;
                handle.wifi_connect(&ap.ssid, None);
            }
            RowAction::AskPassword => {
                let ssid = ap.ssid.clone();
                let input = cx.new(|cx| {
                    let mut i = TextInput::new("password", cx);
                    i.set_masked(true, cx);
                    i
                });
                let ssid2 = ssid.clone();
                cx.subscribe_in(&input, window, move |this: &mut Self, input, ev: &TextInputEvent, _, cx| match ev {
                    TextInputEvent::Submit => {
                        let secret = input.update(cx, |i, cx| i.take_secret(cx));
                        if !secret.is_empty() {
                            this.ctx.services.network.handle.wifi_connect(&ssid2, Some(secret));
                        }
                        cx.notify();
                    }
                    TextInputEvent::Cancel => {
                        this.state.conn.wifi_pick = None;
                        this.state.conn.wifi_input = None;
                        cx.notify();
                    }
                    _ => {}
                })
                .detach();
                input.focus_handle(cx).focus(window, cx);
                self.state.conn.wifi_pick = Some(ssid);
                self.state.conn.wifi_input = Some(input);
            }
        }
        cx.notify();
    }

    pub(crate) fn wifi_section(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let net = self.ctx.services.network.state.read(cx).clone();
        let w = net.wifi?;
        // Connected to the network the password field was open for: it has done its job.
        if let Some(pick) = self.state.conn.wifi_pick.clone()
            && w.active().is_some_and(|a| a.ssid == pick)
        {
            self.state.conn.wifi_pick = None;
            self.state.conn.wifi_input = None;
        }
        let t = theme(cx).clone();
        let s2 = t.surface_2;

        let toggle = ashd_ui::toggle("wifi-power", w.enabled, {
            let handle = self.ctx.services.network.handle.clone();
            move |on, _, _| handle.wifi_set_enabled(*on)
        }, cx);
        let right: gpui::AnyElement = if w.scanning {
            ashd_ui::spinner("wifi-scan-spin", 12., t.accent).into_any_element()
        } else if w.enabled {
            tiny_icon_button("wifi-scan", Icon::Refresh, t.fg_muted, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| this.ctx.services.network.handle.wifi_scan()))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let header_right = div().flex().items_center().gap(px(10.)).child(right).child(toggle);

        let mut body = div();
        if !w.enabled {
            body = body.child(div().px(px(10.)).py(px(12.)).child(label("wi-fi is off", cx)));
        } else if w.aps.is_empty() {
            body = body.child(div().px(px(10.)).py(px(12.)).child(label(if w.scanning { "scanning…" } else { "no networks in range" }, cx)));
        }
        if w.enabled {
            for (ix, ap) in w.aps.iter().enumerate() {
                let picked = self.state.conn.wifi_pick.as_deref() == Some(ap.ssid.as_str());
                let connecting = w.connecting.as_deref() == Some(ap.ssid.as_str());
                let bars = (0..4).fold(div().flex().items_end().gap(px(2.)).h(px(14.)).flex_none(), |d, i| {
                    d.child(div().w(px(3.)).h(px(BAR_H[i])).bg(if (i as u8) < ap.bars() { t.fg } else { t.line }))
                });
                let ap_c = ap.clone();
                body = body.child(
                    div()
                        .id(ElementId::from(("wifi-ap", ix as u32)))
                        .h(px(34.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .when(picked, |d| d.bg(t.surface_2))
                        .when(!picked, |d| d.hover(move |s| s.bg(s2)))
                        .child(bars)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(t.font_sans.clone())
                                .text_size(px(ashd_tokens::metrics::type_scale::UI.0))
                                .text_color(t.fg)
                                .child(ap.ssid.clone()),
                        )
                        .when(ap.active, |d| d.child(caps("connected", t.accent, cx)))
                        .when(connecting && !ap.active, |d| d.child(caps("connecting", t.accent, cx)))
                        .when(ap.secured(), |d| d.child(icon(Icon::Lock, 11.).text_color(t.fg_muted)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick_network(&ap_c, window, cx))),
                );
                if picked && let Some(input) = self.state.conn.wifi_input.clone() {
                    let status: gpui::AnyElement = if connecting {
                        div().text_color(t.fg_muted).child(format!("Connecting to {}…", ap.ssid)).into_any_element()
                    } else if let Some(err) = &w.failed {
                        div().text_color(t.err).child(err.clone()).into_any_element()
                    } else {
                        div().text_color(t.fg_muted).child("Enter the password and press Enter").into_any_element()
                    };
                    let mut f = field(&input, cx);
                    if connecting {
                        f = f.child(ashd_ui::spinner("wifi-pw-spin", 12., t.accent));
                    }
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .px(px(10.))
                            .pt(px(8.))
                            .pb(px(10.))
                            .bg(t.surface_2)
                            .child(f)
                            .child(div().font_family(t.font_sans.clone()).text_size(px(11.5)).child(status)),
                    );
                }
            }
        }
        let c = card_with(wifi_title(&w), header_right, body.flex().flex_col(), cx);
        Some(div().flex().flex_col().gap(px(6.)).child(c))
    }
}

#[cfg(test)]
mod tests {
    use ashd_svc_network::{Security, mock::scenario};

    use super::*;

    #[test]
    fn titles() {
        let s = scenario("wifi").unwrap();
        assert_eq!(wifi_title(s.wifi.as_ref().unwrap()), "wi-fi · 4 networks");
        let off = scenario("nm").unwrap();
        assert_eq!(wifi_title(off.wifi.as_ref().unwrap()), "wi-fi · off");
    }

    #[test]
    fn row_actions() {
        let w = scenario("wifi").unwrap().wifi.unwrap();
        assert_eq!(row_action(&w.aps[0]), RowAction::Disconnect);
        // studio-5G: secured and unknown → password field.
        assert_eq!(row_action(&w.aps[1]), RowAction::AskPassword);
        // cafe-guest: open → connect straight away.
        assert_eq!(w.aps[2].security, Security::Open);
        assert_eq!(row_action(&w.aps[2]), RowAction::Connect);
        let mut known = w.aps[1].clone();
        known.known = true;
        assert_eq!(row_action(&known), RowAction::Connect);
    }

    #[test]
    fn signal_bars_follow_strength() {
        let w = scenario("wifi").unwrap().wifi.unwrap();
        assert_eq!(w.aps.iter().map(|a| a.bars()).collect::<Vec<_>>(), vec![4, 3, 2, 1]);
    }
}
