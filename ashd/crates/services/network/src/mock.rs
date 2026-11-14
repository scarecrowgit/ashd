//! `ASHD_NET_MOCK=<scenario>`: canned states for screenshots and UI development. Nothing
//! is read from the machine and no D-Bus is touched; commands change the canned state in
//! the obvious way (and are logged), so the whole panel flow can be clicked through.

use std::{
    net::{IpAddr, Ipv4Addr},
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use smol::future::or;

use crate::model::*;

/// All scenario names, for docs and `--help`.
pub const SCENARIOS: &[&str] =
    &["connected", "connecting", "limited", "unplugged", "offline", "vpn", "nm", "wifi", "wifi-password", "apply", "empty"];

const ENP: &str = "enp5s0";

fn ip(s: &str) -> IpAddr {
    s.parse().expect("literal address")
}

fn wired(connected: bool) -> Interface {
    Interface {
        name: ENP.into(),
        kind: IfKind::Wired,
        operstate: if connected { OperState::Up } else { OperState::Down },
        carrier: Some(connected),
        speed_mbps: connected.then_some(1000),
        duplex: connected.then_some(Duplex::Full),
        mtu: Some(1500),
        mac: Some("3c:7c:3f:5e:12:a1".into()),
        ipv4: if connected { vec![IpNet::new(ip("192.168.1.24"), 24)] } else { vec![] },
        ipv6: if connected { vec![IpNet::new(ip("2a02:8070::4c1d:a1"), 64)] } else { vec![] },
        gateway: connected.then(|| Ipv4Addr::new(192, 168, 1, 1)),
        is_default: connected,
        rx_bytes: 0,
        tx_bytes: 0,
        today: if connected { Totals { rx: 38_200_000_000, tx: 3_400_000_000 } } else { Totals::default() },
        link_up_secs: connected.then_some(4 * 86_400 + 2 * 3_600 + 11 * 60),
    }
}

fn wifi_iface(connected: bool) -> Interface {
    Interface {
        name: "wlan0".into(),
        kind: IfKind::Wifi,
        operstate: if connected { OperState::Up } else { OperState::Down },
        carrier: Some(connected),
        mtu: Some(1500),
        mac: Some("a4:6b:b6:12:90:5c".into()),
        ipv4: if connected { vec![IpNet::new(ip("192.168.1.77"), 24)] } else { vec![] },
        gateway: connected.then(|| Ipv4Addr::new(192, 168, 1, 1)),
        is_default: connected,
        link_up_secs: connected.then_some(2 * 3_600 + 7 * 60),
        today: Totals { rx: 1_200_000_000, tx: 90_000_000 },
        ..Default::default()
    }
}

fn tunnel(name: &str, kind: VpnKind, addr: &str, default_route: bool) -> (Interface, Vpn) {
    let a = ip(addr);
    (
        Interface {
            name: name.into(),
            kind: IfKind::Tunnel,
            operstate: OperState::Unknown,
            carrier: Some(true),
            mtu: Some(1280),
            ipv4: vec![IpNet::new(a, 32)],
            ..Default::default()
        },
        Vpn { name: name.into(), kind, connected: true, addrs: vec![a], default_route, toggleable: false },
    )
}

fn primary(i: &Interface, status: LinkStatus, profile: Option<&str>, steps: Vec<Step>, hint: Option<&str>) -> Primary {
    Primary { ifname: i.name.clone(), kind: i.kind, status, profile: profile.map(Into::into), steps, hint: hint.map(Into::into) }
}

fn ap(ssid: &str, strength: u8, security: Security, freq: u32, active: bool, known: bool) -> AccessPoint {
    AccessPoint { path: format!("/ap/{ssid}"), ssid: ssid.into(), strength, security, freq_mhz: freq, active, known }
}

fn base(ifaces: Vec<Interface>, primary: Option<Primary>) -> NetworkState {
    NetworkState {
        available: true,
        backend: Backend::ReadOnly,
        can_configure: false,
        primary,
        interfaces: ifaces,
        dns: vec![ip("1.1.1.1"), ip("9.9.9.9")],
        throughput: Throughput { interval: SAMPLE_INTERVAL, rx_bps: 84_200_000.0, tx_bps: 3_100_000.0, ..Default::default() },
        ..Default::default()
    }
}

fn profiles() -> Vec<Profile> {
    vec![
        Profile {
            id: "Home".into(),
            uuid: "u-home".into(),
            kind: ProfileKind::Wired,
            ifname: Some(ENP.into()),
            method: IpMethod::Dhcp,
            address: None,
            autoconnect: true,
            active: true,
        },
        Profile {
            id: "Lab".into(),
            uuid: "u-lab".into(),
            kind: ProfileKind::Wired,
            ifname: Some(ENP.into()),
            method: IpMethod::Static,
            address: Some("10.0.0.5/24".into()),
            autoconnect: false,
            active: false,
        },
    ]
}

fn wifi_state(connected: Option<&str>) -> WifiState {
    WifiState {
        device: "wlan0".into(),
        enabled: true,
        scanning: false,
        aps: vec![
            ap("home", 88, Security::Psk, 5240, connected == Some("home"), true),
            ap("studio-5G", 62, Security::Psk, 5180, false, false),
            ap("cafe-guest", 35, Security::Open, 2412, false, false),
            ap("DIRECT-printer", 18, Security::Psk, 2437, false, false),
        ],
        connecting: None,
        failed: None,
    }
}

/// The canned state of a scenario.
pub fn scenario(name: &str) -> Option<NetworkState> {
    let state = match name {
        "connected" => {
            let e = wired(true);
            let p = primary(&e, LinkStatus::Connected, Some("Home"), vec![], None);
            base(vec![e], Some(p))
        }
        "connecting" => {
            let mut e = wired(true);
            e.ipv4.clear();
            e.ipv6.clear();
            e.is_default = false;
            e.gateway = None;
            e.link_up_secs = Some(2);
            let steps = vec![
                Step::new("Cable detected", ENP, StepStatus::Done),
                Step::new("Link negotiated", "1000 Mb/s · full", StepStatus::Done),
                Step::new("Getting an address", "DHCP · 2 s", StepStatus::Active),
                Step::new("Checking internet", "—", StepStatus::Pending),
            ];
            let p = primary(&e, LinkStatus::Connecting, Some("Home"), steps, None);
            base(vec![e], Some(p))
        }
        "limited" => {
            let mut e = wired(true);
            e.ipv4 = vec![IpNet::new(ip("169.254.12.7"), 16)];
            e.ipv6.clear();
            e.is_default = false;
            e.gateway = None;
            let steps = vec![
                Step::new("Link up", "1000 Mb/s", StepStatus::Done),
                Step::new("No DHCP answer — self-assigned", "169.254.12.7", StepStatus::Warn),
                Step::new("Gateway unreachable", "192.168.1.1", StepStatus::Fail),
            ];
            let hint = "The cable is fine, but nothing on the network handed out an address. Check the router, or switch to a static profile.";
            let p = primary(&e, LinkStatus::Limited, Some("Home"), steps, Some(hint));
            base(vec![e], Some(p))
        }
        "unplugged" => {
            let mut e = wired(false);
            e.link_up_secs = None;
            let p = primary(&e, LinkStatus::Unplugged, None, vec![], None);
            let mut s = base(vec![e], Some(p));
            s.dns.clear();
            s
        }
        "offline" => {
            let mut s = base(vec![], None);
            s.dns.clear();
            s
        }
        "vpn" => {
            let e = wired(true);
            let (wg, wg_vpn) = tunnel("wg0", VpnKind::WireGuard, "10.8.0.2", false);
            let (ts, ts_vpn) = tunnel("tailscale0", VpnKind::Tailscale, "100.101.102.103", false);
            let p = primary(&e, LinkStatus::Connected, Some("Home"), vec![], None);
            let mut s = base(vec![e, ts, wg], Some(p));
            s.vpns = vec![ts_vpn, wg_vpn];
            s
        }
        "nm" | "apply" => {
            let e = wired(true);
            let w = wifi_iface(false);
            let p = primary(&e, LinkStatus::Connected, Some("Home"), vec![], None);
            let mut s = base(vec![e, w], Some(p));
            s.backend = Backend::NetworkManager;
            s.can_configure = true;
            s.profiles = profiles();
            s.wifi = Some(WifiState { enabled: false, ..wifi_state(None) });
            if name == "apply" {
                s.pending_apply = Some(PendingApply {
                    profile: "Lab".into(),
                    revert_to: Some("Home".into()),
                    deadline: Instant::now() + Duration::from_secs(SAFE_APPLY_SECS),
                });
                s.profiles[0].active = false;
                s.profiles[1].active = true;
            }
            s
        }
        "wifi" | "wifi-password" => {
            let e = wired(false);
            let w = wifi_iface(true);
            let p = primary(&w, LinkStatus::Connected, Some("home"), vec![], None);
            let mut s = base(vec![e, w], Some(p));
            s.backend = Backend::NetworkManager;
            s.can_configure = true;
            s.profiles = profiles();
            s.profiles[0].active = false;
            let mut wifi = wifi_state(Some("home"));
            if name == "wifi-password" {
                wifi.connecting = Some("studio-5G".into());
            }
            s.wifi = Some(wifi);
            s
        }
        "empty" => NetworkState::default(),
        _ => return None,
    };
    Some(state)
}

/// A repeatable pseudo-throughput curve (bytes/s) for the sparkline: bursts of download
/// over a quiet upload.
fn curve(n: u64) -> Sample {
    let t = n as f32;
    let rx = 40_000_000.0 + 38_000_000.0 * (t * 0.31).sin() + 9_000_000.0 * (t * 1.7).sin();
    let tx = 1_800_000.0 + 1_100_000.0 * (t * 0.43 + 1.0).sin();
    Sample { rx_bps: rx.max(0.0), tx_bps: tx.max(0.0) }
}

pub(crate) async fn run(name: &str, commands: Receiver<Command>, state: Sender<NetworkState>) {
    let Some(mut s) = scenario(name) else {
        tracing::warn!("ASHD_NET_MOCK: unknown scenario {name:?}; known: {SCENARIOS:?}");
        let _ = state.force_send(NetworkState::default());
        while commands.recv().await.is_ok() {}
        return;
    };
    let mut sampling = false;
    let mut n = 0u64;
    let mut event_seq = 0u64;
    let mut connect_at: Option<(Instant, String)> = None;
    let _ = state.force_send(s.clone());
    loop {
        let wake = Instant::now() + SAMPLE_INTERVAL;
        let ev = or(async { Some(commands.recv().await) }, async {
            smol::Timer::at(wake).await;
            None
        })
        .await;
        match ev {
            Some(Err(_)) => return,
            Some(Ok(cmd)) => {
                tracing::info!(target: "ashd_svc_network::mock", "{cmd:?}");
                apply_command(&mut s, cmd, &mut sampling, &mut connect_at, &mut event_seq);
            }
            None => {}
        }
        if let Some((at, ssid)) = &connect_at
            && at.elapsed() > Duration::from_secs(3)
        {
            finish_connect(&mut s, ssid);
            connect_at = None;
        }
        if sampling && s.status() == LinkStatus::Connected {
            n += 1;
            if s.throughput.history.is_empty() {
                s.throughput.history = (0..40).map(|k| curve(n.saturating_sub(40) + k)).collect();
            }
            let sample = curve(n);
            s.throughput.history.push(sample);
            while s.throughput.history.len() > HISTORY_LEN {
                s.throughput.history.remove(0);
            }
            s.throughput.rx_bps = sample.rx_bps;
            s.throughput.tx_bps = sample.tx_bps;
            s.throughput.window_max_bps = s.throughput.history.iter().fold(0.0f32, |m, x| m.max(x.rx_bps).max(x.tx_bps));
            s.throughput.sampled_at = Some(Instant::now());
        }
        s.sampling = sampling;
        let _ = state.force_send(s.clone());
    }
}

fn finish_connect(s: &mut NetworkState, ssid: &str) {
    if let Some(w) = s.wifi.as_mut() {
        w.connecting = None;
        for a in &mut w.aps {
            a.active = a.ssid == ssid;
            if a.active {
                a.known = true;
            }
        }
    }
}

/// Disconnect / Connect in the mock raise the link event a real cable would (the shell's
/// network toast can be exercised in the nested session).
fn mock_link_event(s: &mut NetworkState, kind: LinkEventKind, seq: &mut u64) {
    let Some(i) = s.primary_interface().cloned() else { return };
    *seq += 1;
    s.last_event = Some(LinkEvent { seq: *seq, kind, ifname: i.name.clone(), detail: crate::engine::event_detail(kind, &i) });
}

fn apply_command(s: &mut NetworkState, cmd: Command, sampling: &mut bool, connect_at: &mut Option<(Instant, String)>, seq: &mut u64) {
    match cmd {
        Command::SetSampling(on) => *sampling = on,
        Command::Refresh => {}
        Command::ActivateProfile { uuid } => {
            for p in &mut s.profiles {
                p.active = p.uuid == uuid;
            }
            s.pending_apply = None;
        }
        Command::DisconnectDevice { .. } => {
            if let Some(p) = s.primary.as_mut() {
                p.status = LinkStatus::Disconnected;
            }
            for p in &mut s.profiles {
                p.active = false;
            }
            mock_link_event(s, LinkEventKind::Unplugged, seq);
        }
        Command::ConnectDevice { .. } => {
            if let Some(p) = s.primary.as_mut() {
                p.status = LinkStatus::Connected;
            }
            if let Some(h) = s.profiles.first_mut() {
                h.active = true;
            }
            mock_link_event(s, LinkEventKind::Connected, seq);
        }
        Command::KeepApply => s.pending_apply = None,
        Command::RevertApply => {
            s.pending_apply = None;
            for p in &mut s.profiles {
                p.active = p.uuid == "u-home";
            }
        }
        Command::ApplyStatic(cfg) => {
            s.profiles.push(Profile {
                id: cfg.name.clone(),
                uuid: format!("u-{}", cfg.name),
                kind: ProfileKind::Wired,
                ifname: Some(cfg.ifname),
                method: IpMethod::Static,
                address: Some(format!("{}/{}", cfg.address, cfg.prefix)),
                autoconnect: false,
                active: true,
            });
            let prev = s.profiles.iter_mut().find(|p| p.active && p.id != cfg.name).map(|p| {
                p.active = false;
                p.id.clone()
            });
            s.pending_apply = Some(PendingApply { profile: cfg.name, revert_to: prev, deadline: Instant::now() + Duration::from_secs(SAFE_APPLY_SECS) });
        }
        Command::ForgetProfile { uuid } => s.profiles.retain(|p| p.uuid != uuid),
        Command::WifiSetEnabled(on) => {
            if let Some(w) = s.wifi.as_mut() {
                w.enabled = on;
            }
        }
        Command::WifiScan => {
            if let Some(w) = s.wifi.as_mut() {
                w.scanning = !w.scanning;
            }
        }
        Command::WifiConnect { ssid, .. } => {
            if let Some(w) = s.wifi.as_mut() {
                w.connecting = Some(ssid.clone());
                w.failed = None;
            }
            *connect_at = Some((Instant::now(), ssid));
        }
        Command::WifiDisconnect => {
            if let Some(w) = s.wifi.as_mut() {
                for a in &mut w.aps {
                    a.active = false;
                }
            }
        }
        Command::Reapply { .. } => {
            *seq += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_exists_and_is_coherent() {
        for name in SCENARIOS {
            let s = scenario(name).unwrap_or_else(|| panic!("{name}"));
            if let Some(p) = &s.primary {
                assert!(s.interfaces.iter().any(|i| i.name == p.ifname), "{name}: primary interface listed");
            }
        }
        assert!(scenario("nope").is_none());
        let c = scenario("connected").unwrap();
        assert_eq!(c.tile_title(), "Ethernet");
        assert_eq!(c.tile_subtitle(), "enp5s0 · 1 Gb/s");
        assert_eq!(scenario("offline").unwrap().tile_title(), "Offline");
        assert!(scenario("nm").unwrap().can_configure);
        assert!(!c.can_configure);
        assert_eq!(scenario("vpn").unwrap().vpns.len(), 2);
    }

    #[test]
    fn mock_commands_drive_the_flow() {
        let mut s = scenario("wifi").unwrap();
        let (mut sampling, mut at, mut seq) = (false, None, 0);
        apply_command(&mut s, Command::WifiConnect { ssid: "studio-5G".into(), password: None }, &mut sampling, &mut at, &mut seq);
        assert_eq!(s.wifi.as_ref().unwrap().connecting.as_deref(), Some("studio-5G"));
        finish_connect(&mut s, "studio-5G");
        assert_eq!(s.wifi.as_ref().unwrap().active().unwrap().ssid, "studio-5G");
        apply_command(&mut s, Command::SetSampling(true), &mut sampling, &mut at, &mut seq);
        assert!(sampling);
    }
}
