//! The plain data the UI needs (det-06 Wi-Fi list, det-12 Ethernet / VPN states).

use std::{
    net::{IpAddr, Ipv4Addr},
    time::{Duration, Instant},
};

use serde::Serialize;
use zeroize::Zeroizing;

/// Which backend produced the state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum Backend {
    /// Nothing sampled yet.
    #[default]
    None,
    /// `/sys/class/net`, `/proc/net/*`, `/etc/resolv.conf` — no daemon, nothing configurable.
    ReadOnly,
    /// `org.freedesktop.NetworkManager` on the system bus.
    NetworkManager,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum IfKind {
    #[default]
    Wired,
    Wifi,
    /// WireGuard / tun / tailscale / ppp …: shown as a VPN row, never as the primary link.
    Tunnel,
    /// Bridges (docker0, br-…, virbr0).
    Bridge,
    /// veth pairs and other virtual devices.
    Virtual,
    Loopback,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum OperState {
    Up,
    Down,
    Dormant,
    LowerLayerDown,
    NotPresent,
    #[default]
    Unknown,
}

impl OperState {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "up" => Self::Up,
            "down" => Self::Down,
            "dormant" => Self::Dormant,
            "lowerlayerdown" => Self::LowerLayerDown,
            "notpresent" => Self::NotPresent,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Duplex {
    Full,
    Half,
}

/// An address with its prefix length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct IpNet {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl IpNet {
    pub fn new(addr: IpAddr, prefix: u8) -> Self {
        Self { addr, prefix }
    }
    pub fn is_link_local(&self) -> bool {
        match self.addr {
            IpAddr::V4(a) => a.is_link_local(),
            IpAddr::V6(a) => (a.segments()[0] & 0xffc0) == 0xfe80,
        }
    }
}

impl std::fmt::Display for IpNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Bytes moved since local midnight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub rx: u64,
    pub tx: u64,
}

impl Totals {
    pub fn total(&self) -> u64 {
        self.rx.saturating_add(self.tx)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Interface {
    pub name: String,
    pub kind: IfKind,
    pub operstate: OperState,
    /// `None` when the kernel refuses to say (interface down).
    pub carrier: Option<bool>,
    pub speed_mbps: Option<u32>,
    pub duplex: Option<Duplex>,
    pub mtu: Option<u32>,
    pub mac: Option<String>,
    /// Link-local addresses last.
    pub ipv4: Vec<IpNet>,
    /// Global addresses first, link-local last.
    pub ipv6: Vec<IpNet>,
    /// Gateway of the default route through this interface.
    pub gateway: Option<Ipv4Addr>,
    pub is_default: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub today: Totals,
    /// Seconds since the carrier came up (tracked by the service; see `link_uptime`).
    pub link_up_secs: Option<u64>,
}

impl Interface {
    pub fn has_carrier(&self) -> bool {
        self.carrier == Some(true) && self.operstate != OperState::Down
    }
    /// First IPv4 address that is not link-local.
    pub fn global_ipv4(&self) -> Option<&IpNet> {
        self.ipv4.iter().find(|a| !a.is_link_local())
    }
    pub fn global_ipv6(&self) -> Option<&IpNet> {
        self.ipv6.iter().find(|a| !a.is_link_local())
    }
    /// `1 Gb/s`, `100 Mb/s`, `2.5 Gb/s` — the mockup's wording.
    pub fn speed_text(&self) -> Option<String> {
        self.speed_mbps.map(speed_text)
    }
    /// `1G`, `2.5G`, `100M` — the status bar's compact form.
    pub fn speed_short(&self) -> Option<String> {
        let s = self.speed_mbps?;
        Some(if s >= 1000 {
            let g = s as f32 / 1000.0;
            if (g - g.round()).abs() < 0.05 { format!("{}G", g.round() as u32) } else { format!("{g:.1}G") }
        } else {
            format!("{s}M")
        })
    }
    /// `1000 Mb/s · full duplex · MTU 1500`.
    pub fn link_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = self.speed_mbps {
            parts.push(format!("{s} Mb/s"));
        }
        match self.duplex {
            Some(Duplex::Full) => parts.push("full duplex".into()),
            Some(Duplex::Half) => parts.push("half duplex".into()),
            None => {}
        }
        if let Some(m) = self.mtu {
            parts.push(format!("MTU {m}"));
        }
        parts.join(" · ")
    }
}

pub fn speed_text(mbps: u32) -> String {
    if mbps >= 1000 {
        let g = mbps as f32 / 1000.0;
        if (g - g.round()).abs() < 0.05 { format!("{} Gb/s", g.round() as u32) } else { format!("{g:.1} Gb/s") }
    } else {
        format!("{mbps} Mb/s")
    }
}

/// What the status bar and the tile show (det-12 `K = up | connecting | limited | down`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum LinkStatus {
    #[default]
    Unknown,
    Connected,
    Connecting,
    /// Link up but no usable internet path ("no net", amber dot).
    Limited,
    /// No carrier / not associated.
    Unplugged,
    /// Carrier present but the link is deliberately not in use (NetworkManager "Disconnect").
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StepStatus {
    Done,
    Active,
    Pending,
    Warn,
    Fail,
}

/// One line of the connecting / limited step list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    pub label: String,
    pub detail: String,
    pub status: StepStatus,
}

impl Step {
    pub fn new(label: &str, detail: impl Into<String>, status: StepStatus) -> Self {
        Self { label: label.into(), detail: detail.into(), status }
    }
}

/// The connection the shell is about (default route, else the best physical link).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Primary {
    pub ifname: String,
    pub kind: IfKind,
    pub status: LinkStatus,
    /// NM profile name / Wi-Fi SSID.
    pub profile: Option<String>,
    /// Explains `Connecting` / `Limited`; empty when connected.
    pub steps: Vec<Step>,
    /// One line for the "limited" explanation ("The cable is fine, but …").
    pub hint: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum VpnKind {
    WireGuard,
    Tailscale,
    Tun,
    Other,
}

/// A tunnel interface with an address. Read-only unless the NM backend can toggle it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Vpn {
    pub name: String,
    pub kind: VpnKind,
    pub connected: bool,
    pub addrs: Vec<IpAddr>,
    /// The tunnel carries the default route (full tunnel).
    pub default_route: bool,
    /// NM backend only.
    pub toggleable: bool,
}

/// One throughput sample (bytes per second over the last interval).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Sample {
    pub rx_bps: f32,
    pub tx_bps: f32,
}

/// Throughput of the primary interface. `history` is filled only while a consumer asked
/// for sampling (`Handle::set_sampling(true)`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Throughput {
    pub rx_bps: f32,
    pub tx_bps: f32,
    /// Oldest first, one entry per [`Throughput::interval`], at most [`HISTORY_LEN`].
    pub history: Vec<Sample>,
    pub interval: Duration,
    /// Max of rx and tx over `history` — the consumer latches its sparkline scale from
    /// this when the view opens and never rescales mid-view.
    pub window_max_bps: f32,
    /// When the newest sample was taken: scroll offset = `sampled_at.elapsed() / interval`.
    #[serde(skip)]
    pub sampled_at: Option<Instant>,
}

/// 60 samples = 30 s at 500 ms.
pub const HISTORY_LEN: usize = 60;
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
pub const SLOW_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum Security {
    #[default]
    Open,
    Wep,
    /// WPA / WPA2 personal.
    Psk,
    /// WPA3 personal.
    Sae,
    /// 802.1X.
    Enterprise,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AccessPoint {
    /// NM object path (identifier for commands).
    pub path: String,
    pub ssid: String,
    /// 0..=100.
    pub strength: u8,
    pub security: Security,
    pub freq_mhz: u32,
    pub active: bool,
    /// A saved profile exists for this SSID (no password prompt needed).
    pub known: bool,
}

impl AccessPoint {
    pub fn secured(&self) -> bool {
        self.security != Security::Open
    }
    /// 0..=4 bars for the strength meter of the list.
    pub fn bars(&self) -> u8 {
        match self.strength {
            0..=19 => 1,
            20..=44 => 2,
            45..=69 => 3,
            _ => 4,
        }
    }
    pub fn band(&self) -> &'static str {
        if self.freq_mhz >= 5900 {
            "6 GHz"
        } else if self.freq_mhz >= 4900 {
            "5 GHz"
        } else {
            "2.4 GHz"
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WifiState {
    pub device: String,
    pub enabled: bool,
    pub scanning: bool,
    /// Strongest first, the active one on top.
    pub aps: Vec<AccessPoint>,
    /// SSID being connected to (spinner row / inline password field state).
    pub connecting: Option<String>,
    /// Last connect failure ("wrong password?").
    pub failed: Option<String>,
}

impl WifiState {
    pub fn active(&self) -> Option<&AccessPoint> {
        self.aps.iter().find(|a| a.active)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum IpMethod {
    #[default]
    Dhcp,
    Static,
    LinkLocal,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum ProfileKind {
    #[default]
    Wired,
    Wifi,
    Vpn,
    Other,
}

/// A saved NM connection profile.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Profile {
    pub id: String,
    pub uuid: String,
    pub kind: ProfileKind,
    /// Bound interface, if the profile names one.
    pub ifname: Option<String>,
    pub method: IpMethod,
    /// `10.0.0.5/24` for static profiles.
    pub address: Option<String>,
    pub autoconnect: bool,
    pub active: bool,
}

impl Profile {
    /// `DHCP · auto-connect`, `static · 10.0.0.5/24` (det-12 profile list).
    pub fn summary(&self) -> String {
        let method = match (self.method, &self.address) {
            (IpMethod::Static, Some(a)) => format!("static · {a}"),
            (IpMethod::Static, None) => "static".into(),
            (IpMethod::Dhcp, _) => "DHCP".into(),
            (IpMethod::LinkLocal, _) => "link-local".into(),
            (IpMethod::Disabled, _) => "disabled".into(),
        };
        if self.autoconnect && self.method == IpMethod::Dhcp { format!("{method} · auto-connect") } else { method }
    }
}

/// "Keep these network settings?" — a manual profile was applied; without an answer
/// the previous profile comes back at `deadline`.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingApply {
    pub profile: String,
    pub revert_to: Option<String>,
    pub deadline: Instant,
}

/// Seconds to answer the safe-apply banner.
pub const SAFE_APPLY_SECS: u64 = 15;

/// A debounced link event for toasts (rules of det-12: unplug / replug fire a toast, a
/// flap within [`FLAP_WINDOW`] shows one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkEvent {
    pub seq: u64,
    pub kind: LinkEventKind,
    pub ifname: String,
    /// `1 Gb/s · 192.168.1.24 · Home`, `No DHCP answer on enp5s0 — using 169.254.12.7`.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkEventKind {
    Connected,
    /// The cable bounced within the flap window: one toast, "reconnected".
    Reconnected,
    Limited,
    Unplugged,
}

/// A down/up within this window is one change.
pub const FLAP_WINDOW: Duration = Duration::from_secs(5);

/// Everything a consumer needs; a full snapshot, published after every change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkState {
    /// The service produced at least one sample.
    pub available: bool,
    pub backend: Backend,
    /// True only with the NetworkManager backend (profiles, Disconnect, Wi-Fi, …).
    pub can_configure: bool,
    pub primary: Option<Primary>,
    /// Wired and wireless first, then tunnels, then the rest.
    pub interfaces: Vec<Interface>,
    pub dns: Vec<IpAddr>,
    pub vpns: Vec<Vpn>,
    pub throughput: Throughput,
    pub sampling: bool,
    pub wifi: Option<WifiState>,
    pub profiles: Vec<Profile>,
    pub pending_apply: Option<PendingApply>,
    /// Newest debounced link event; compare `seq` with the last one you handled.
    pub last_event: Option<LinkEvent>,
    /// Last command failure.
    pub error: Option<String>,
}

impl NetworkState {
    pub fn primary_interface(&self) -> Option<&Interface> {
        let p = self.primary.as_ref()?;
        self.interfaces.iter().find(|i| i.name == p.ifname)
    }
    pub fn status(&self) -> LinkStatus {
        self.primary.as_ref().map(|p| p.status).unwrap_or_default()
    }
    /// The tile's title: `Ethernet`, `Wi-Fi` or `Offline`.
    pub fn tile_title(&self) -> &'static str {
        match self.primary.as_ref() {
            Some(p) if !matches!(p.status, LinkStatus::Unplugged | LinkStatus::Unknown) => match p.kind {
                IfKind::Wifi => "Wi-Fi",
                _ => "Ethernet",
            },
            Some(p) if p.kind == IfKind::Wifi => "Wi-Fi",
            Some(_) => "Ethernet",
            None => "Offline",
        }
    }
    /// The tile's second line: `enp5s0 · 1 Gb/s`, the SSID, `cable unplugged`, `no internet`.
    pub fn tile_subtitle(&self) -> String {
        let Some(p) = self.primary.as_ref() else { return "no network".into() };
        let iface = self.primary_interface();
        match p.status {
            LinkStatus::Connected => match p.kind {
                IfKind::Wifi => match (p.profile.clone(), self.wifi.as_ref().and_then(|w| w.active())) {
                    (_, Some(ap)) => format!("{} · {}", ap.ssid, ap.band()),
                    (Some(name), None) => name,
                    (None, None) => p.ifname.clone(),
                },
                _ => match iface.and_then(|i| i.speed_text()) {
                    Some(s) => format!("{} · {s}", p.ifname),
                    None => p.ifname.clone(),
                },
            },
            LinkStatus::Connecting => "connecting…".into(),
            LinkStatus::Limited => "no internet".into(),
            LinkStatus::Unplugged => {
                if p.kind == IfKind::Wifi { "not connected".into() } else { "cable unplugged".into() }
            }
            LinkStatus::Disconnected => "disconnected".into(),
            LinkStatus::Unknown => "…".into(),
        }
    }
}

/// A static IPv4 configuration for [`Command::ApplyStatic`].
#[derive(Clone, Debug, PartialEq)]
pub struct StaticConfig {
    pub name: String,
    pub ifname: String,
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
}

pub enum Command {
    /// Fast (500 ms) throughput sampling while a view is open; slow poll otherwise.
    SetSampling(bool),
    /// Re-read everything now.
    Refresh,
    // ── NetworkManager only (ignored with a log line by the read-only backend) ──
    ConnectDevice { ifname: String },
    DisconnectDevice { ifname: String },
    ActivateProfile { uuid: String },
    /// Re-apply the active connection (DHCP retry).
    Reapply { ifname: String },
    /// Add + activate a static profile and start the safe-apply countdown.
    ApplyStatic(StaticConfig),
    /// "Keep" on the safe-apply banner.
    KeepApply,
    /// "Revert" on the banner (or the timeout).
    RevertApply,
    ForgetProfile { uuid: String },
    WifiSetEnabled(bool),
    WifiScan,
    /// Connect to `ssid`; `password` goes straight into the NM call and is never logged.
    WifiConnect { ssid: String, password: Option<Zeroizing<String>> },
    WifiDisconnect,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The secret never reaches a log line.
            Self::WifiConnect { ssid, password } => f
                .debug_struct("WifiConnect")
                .field("ssid", ssid)
                .field("password", &password.as_ref().map(|_| "<redacted>"))
                .finish(),
            Self::SetSampling(v) => write!(f, "SetSampling({v})"),
            Self::Refresh => write!(f, "Refresh"),
            Self::ConnectDevice { ifname } => write!(f, "ConnectDevice({ifname})"),
            Self::DisconnectDevice { ifname } => write!(f, "DisconnectDevice({ifname})"),
            Self::ActivateProfile { uuid } => write!(f, "ActivateProfile({uuid})"),
            Self::Reapply { ifname } => write!(f, "Reapply({ifname})"),
            Self::ApplyStatic(c) => write!(f, "ApplyStatic({c:?})"),
            Self::KeepApply => write!(f, "KeepApply"),
            Self::RevertApply => write!(f, "RevertApply"),
            Self::ForgetProfile { uuid } => write!(f, "ForgetProfile({uuid})"),
            Self::WifiSetEnabled(v) => write!(f, "WifiSetEnabled({v})"),
            Self::WifiScan => write!(f, "WifiScan"),
            Self::WifiDisconnect => write!(f, "WifiDisconnect"),
        }
    }
}
