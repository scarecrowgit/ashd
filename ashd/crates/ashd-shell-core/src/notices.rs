//! Shell-originated notifications: service events that deserve a toast, posted through
//! the notifications service's local path (`Handle::post`: same toasts and history as
//! D-Bus notifications, no bus round trip).
//!
//! - network link events (`NetworkState::last_event`, already debounced by the service: a
//!   flap within 5 s is one "reconnected" event) — det-12 wording, the compact 340 px toast
//!   (category `network.*`), one toast at a time (tag `network-link`).

use std::time::Duration;

use gpui::App;
use ashd_svc_network::{IfKind, LinkEvent, LinkEventKind};
use ashd_svc_notifications::{LocalNotification, Urgency};

use crate::Services;

/// The toast for a link event (det-12).
pub fn network_notice(ev: &LinkEvent, wifi: bool) -> LocalNotification {
    let link = if wifi { "Wi-Fi" } else { "Ethernet" };
    let (summary, icon, category) = match ev.kind {
        LinkEventKind::Connected => (format!("{link} connected"), "ethernet", "network.connected"),
        LinkEventKind::Reconnected => (format!("{link} reconnected"), "ethernet", "network.connected"),
        LinkEventKind::Limited => ("Connected, no internet".to_string(), "ethernet", "network.error"),
        LinkEventKind::Unplugged if wifi => ("Wi-Fi disconnected".to_string(), "ethernet_off", "network.disconnected"),
        LinkEventKind::Unplugged => ("Ethernet cable unplugged".to_string(), "ethernet_off", "network.disconnected"),
    };
    let (icon, urgency) = match (wifi, ev.kind) {
        (true, LinkEventKind::Unplugged) => ("wifi_off", Urgency::Normal),
        (true, _) => ("wifi", Urgency::Normal),
        _ => (icon, Urgency::Normal),
    };
    LocalNotification {
        app_name: "network".into(),
        summary,
        body: ev.detail.clone(),
        icon: Some(format!("{}{icon}", ashd_svc_notifications::ASHD_ICON_PREFIX)),
        urgency,
        // "No internet" stays until it is read; the rest go after the default time.
        timeout: (ev.kind == LinkEventKind::Limited).then_some(Duration::ZERO),
        actions: Vec::new(),
        category: Some(category.into()),
        tag: Some("network-link".into()),
    }
}

/// Watch the services and post their notices.
pub(crate) fn wire(services: &Services, cx: &mut App) {
    let notifications = services.notifications.handle.clone();
    let mut handled = 0u64;
    cx.observe(&services.network.state, move |state, cx| {
        let s = state.read(cx);
        let Some(ev) = s.last_event.as_ref().filter(|e| e.seq > handled) else { return };
        handled = ev.seq;
        let wifi = s.interfaces.iter().any(|i| i.name == ev.ifname && i.kind == IfKind::Wifi);
        tracing::info!("network event {:?} on {}: toast", ev.kind, ev.ifname);
        notifications.post(network_notice(ev, wifi));
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: LinkEventKind, detail: &str) -> LinkEvent {
        LinkEvent { seq: 1, kind, ifname: "enp5s0".into(), detail: detail.into() }
    }

    #[test]
    fn det12_wording() {
        let n = network_notice(&ev(LinkEventKind::Connected, "1 Gb/s · 192.168.1.24"), false);
        assert_eq!((n.app_name.as_str(), n.summary.as_str(), n.body.as_str()), ("network", "Ethernet connected", "1 Gb/s · 192.168.1.24"));
        assert_eq!(n.icon.as_deref(), Some("ashd:ethernet"));
        assert_eq!(n.timeout, None);
        assert_eq!(network_notice(&ev(LinkEventKind::Reconnected, ""), false).summary, "Ethernet reconnected");
        let limited = network_notice(&ev(LinkEventKind::Limited, "No DHCP answer on enp5s0"), false);
        assert_eq!((limited.summary.as_str(), limited.category.as_deref()), ("Connected, no internet", Some("network.error")));
        assert_eq!(limited.timeout, Some(Duration::ZERO), "stays until read");
        let gone = network_notice(&ev(LinkEventKind::Unplugged, "enp5s0 · was up 4d 02h"), false);
        assert_eq!((gone.summary.as_str(), gone.icon.as_deref()), ("Ethernet cable unplugged", Some("ashd:ethernet_off")));
        assert_eq!(network_notice(&ev(LinkEventKind::Unplugged, ""), true).summary, "Wi-Fi disconnected");
        // every link notice replaces the previous one
        assert!([LinkEventKind::Connected, LinkEventKind::Unplugged].iter().all(|k| network_notice(&ev(*k, ""), false).tag.as_deref() == Some("network-link")));
    }
}
