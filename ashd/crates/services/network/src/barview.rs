//! What the status bar's network control shows (det-12 "Status bar indicator"): a pure
//! function of the state, so the bar only maps glyphs and tones to its own widgets.
//!
//! * connected wired: ethernet glyph + link speed (`1G`); connected Wi-Fi: signal glyph;
//! * connecting: accent glyph pulsing (1.2 s) + `…`;
//! * limited: amber dot on the glyph + `no net`;
//! * unplugged / not associated: slashed glyph, muted, `off`;
//! * a connected tunnel adds a shield + its name (`wg0`, `tailscale0`) in accent.

use crate::model::{IfKind, LinkStatus, NetworkState};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph {
    Ethernet,
    EthernetOff,
    /// Signal strength 0..=1.
    Wifi(f32),
    WifiOff,
    GlobeOff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Accent,
    Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarNet {
    pub glyph: Glyph,
    pub tone: Tone,
    pub text: Option<String>,
    pub text_tone: Tone,
    /// Amber dot at the corner of the glyph.
    pub warn_dot: bool,
    /// Pulse the glyph (connecting).
    pub pulse: bool,
    /// Name of a connected tunnel, shortened.
    pub vpn: Option<String>,
}

/// Tunnel names are shortened to this many characters.
pub const VPN_NAME_MAX: usize = 9;

fn short(name: &str) -> String {
    // `tailscale0` → `tailscale`: the trailing unit number is noise in a 10 px label.
    let name = if name.chars().count() > VPN_NAME_MAX { name.trim_end_matches(|c: char| c.is_ascii_digit()) } else { name };
    if name.chars().count() <= VPN_NAME_MAX { name.to_string() } else { name.chars().take(VPN_NAME_MAX).collect::<String>() + "…" }
}

pub fn bar_net(s: &NetworkState) -> BarNet {
    let vpn = s.vpns.iter().find(|v| v.connected).map(|v| short(&v.name));
    let Some(p) = s.primary.as_ref() else {
        return BarNet { glyph: Glyph::GlobeOff, tone: Tone::Muted, text: None, text_tone: Tone::Muted, warn_dot: false, pulse: false, vpn };
    };
    let wifi = p.kind == IfKind::Wifi;
    let strength = s.wifi.as_ref().and_then(|w| w.active()).map(|a| f32::from(a.strength) / 100.0).unwrap_or(1.0);
    let live = if wifi { Glyph::Wifi(strength) } else { Glyph::Ethernet };
    let off = if wifi { Glyph::WifiOff } else { Glyph::EthernetOff };
    let speed = s.primary_interface().and_then(|i| i.speed_short());
    let (glyph, tone, text, text_tone, warn_dot, pulse) = match p.status {
        LinkStatus::Connected => (live, Tone::Normal, if wifi { None } else { speed }, Tone::Normal, false, false),
        LinkStatus::Connecting => (live, Tone::Accent, Some("…".to_string()), Tone::Accent, false, true),
        LinkStatus::Limited => (live, Tone::Normal, Some("no net".to_string()), Tone::Warn, true, false),
        LinkStatus::Unplugged | LinkStatus::Disconnected => (off, Tone::Muted, Some("off".to_string()), Tone::Muted, false, false),
        LinkStatus::Unknown => (Glyph::GlobeOff, Tone::Muted, None, Tone::Muted, false, false),
    };
    BarNet { glyph, tone, text, text_tone, warn_dot, pulse, vpn }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::scenario;

    #[test]
    fn states_match_det_12() {
        let c = bar_net(&scenario("connected").unwrap());
        assert_eq!((c.glyph, c.text.as_deref(), c.warn_dot, c.pulse, c.vpn.is_some()), (Glyph::Ethernet, Some("1G"), false, false, false));
        let k = bar_net(&scenario("connecting").unwrap());
        assert_eq!((k.tone, k.text.as_deref(), k.pulse), (Tone::Accent, Some("…"), true));
        let l = bar_net(&scenario("limited").unwrap());
        assert_eq!((l.text.as_deref(), l.text_tone, l.warn_dot), (Some("no net"), Tone::Warn, true));
        let u = bar_net(&scenario("unplugged").unwrap());
        assert_eq!((u.glyph, u.tone, u.text.as_deref()), (Glyph::EthernetOff, Tone::Muted, Some("off")));
        let o = bar_net(&scenario("offline").unwrap());
        assert_eq!((o.glyph, o.text), (Glyph::GlobeOff, None));
        let w = bar_net(&scenario("wifi").unwrap());
        assert!(matches!(w.glyph, Glyph::Wifi(s) if s > 0.5) && w.text.is_none());
    }

    #[test]
    fn vpn_adds_a_shield_with_the_short_name() {
        let v = bar_net(&scenario("vpn").unwrap());
        assert_eq!(v.vpn.as_deref(), Some("tailscale"));
        assert_eq!(short("wg0"), "wg0");
        assert_eq!(short("a-very-long-tunnel"), "a-very-lo…");
    }
}
