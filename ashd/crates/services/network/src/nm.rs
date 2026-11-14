//! The NetworkManager backend (`org.freedesktop.NetworkManager`, system bus, zbus 5).
//!
//! The kernel facts (addresses, speed, counters, VPN tunnels, history, daily totals) come
//! from [`Core`](crate::core::Core) exactly as in the read-only backend; NetworkManager
//! adds what only it knows: which profile is active, whether a device is connecting or
//! has limited connectivity, Wi-Fi (access points, scan), saved profiles and the commands.
//!
//! D-Bus is read with plain `GetAll` calls (no generated proxies) into [`NmSnapshot`], a
//! plain-data picture; [`overlay`] merges it into the shared [`NetworkState`]. Both are
//! testable without a bus; the bus paths are covered by `tests/mock_nm.rs` against a mock
//! NetworkManager on a private connection.

use std::{
    collections::HashMap,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use zbus::{
    Connection,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};
use zeroize::Zeroizing;

use crate::model::{
    AccessPoint, Backend, IfKind, IpMethod, LinkStatus, NetworkState, PendingApply, Profile, ProfileKind, Security,
    StaticConfig, Step, StepStatus, Vpn, VpnKind, WifiState,
};

pub const NM: &str = "org.freedesktop.NetworkManager";
const MANAGER: &str = "/org/freedesktop/NetworkManager";
const SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const PROPS: &str = "org.freedesktop.DBus.Properties";

pub type Props = HashMap<String, OwnedValue>;
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

// ── NM enums (the documented numeric values) ──────────────────────────────────

pub const DEVICE_TYPE_ETHERNET: u32 = 1;
pub const DEVICE_TYPE_WIFI: u32 = 2;
pub const DEV_UNAVAILABLE: u32 = 20;
pub const DEV_DISCONNECTED: u32 = 30;
pub const DEV_PREPARE: u32 = 40;
pub const DEV_NEED_AUTH: u32 = 60;
pub const DEV_SECONDARIES: u32 = 90;
pub const DEV_ACTIVATED: u32 = 100;
pub const DEV_FAILED: u32 = 120;
pub const CONNECTIVITY_NONE: u32 = 1;
pub const CONNECTIVITY_PORTAL: u32 = 2;
pub const CONNECTIVITY_LIMITED: u32 = 3;

const AP_FLAG_PRIVACY: u32 = 0x1;
const AP_SEC_KEY_MGMT_PSK: u32 = 0x100;
const AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;
const AP_SEC_KEY_MGMT_SAE: u32 = 0x400;

/// Security class from an AP's flags (`Flags`, `WpaFlags`, `RsnFlags`).
pub fn security_of(flags: u32, wpa: u32, rsn: u32) -> Security {
    let both = wpa | rsn;
    if both & AP_SEC_KEY_MGMT_802_1X != 0 {
        Security::Enterprise
    } else if both & AP_SEC_KEY_MGMT_SAE != 0 && both & AP_SEC_KEY_MGMT_PSK == 0 {
        Security::Sae
    } else if both & (AP_SEC_KEY_MGMT_PSK | AP_SEC_KEY_MGMT_SAE) != 0 {
        Security::Psk
    } else if flags & AP_FLAG_PRIVACY != 0 {
        Security::Wep
    } else {
        Security::Open
    }
}

// ── plain-data snapshot ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NmDevice {
    pub path: String,
    pub ifname: String,
    pub dtype: u32,
    pub state: u32,
    pub active_id: Option<String>,
    pub active_uuid: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NmAp {
    pub path: String,
    pub ssid: String,
    pub strength: u8,
    pub flags: u32,
    pub wpa: u32,
    pub rsn: u32,
    pub freq: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NmWifi {
    pub device_path: String,
    pub ifname: String,
    pub aps: Vec<NmAp>,
    pub active_ap: Option<String>,
    pub last_scan: i64,
    pub state: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NmSnapshot {
    pub connectivity: u32,
    pub wireless_enabled: bool,
    pub devices: Vec<NmDevice>,
    pub wifi: Option<NmWifi>,
    /// `(object path, profile)`.
    pub profiles: Vec<(String, Profile)>,
}

// ── reading ──────────────────────────────────────────────────────────────────

fn s_of(v: &OwnedValue) -> Option<String> {
    v.downcast_ref::<&str>().ok().map(str::to_string).or_else(|| v.downcast_ref::<ObjectPath<'_>>().ok().map(|p| p.to_string()))
}

fn u_of(v: &OwnedValue) -> Option<u32> {
    v.downcast_ref::<u32>().ok().or_else(|| v.downcast_ref::<u8>().ok().map(u32::from))
}

trait PropsExt {
    fn text(&self, k: &str) -> Option<String>;
    fn num(&self, k: &str) -> Option<u32>;
    fn flag(&self, k: &str) -> Option<bool>;
    fn paths(&self, k: &str) -> Vec<String>;
}

impl PropsExt for Props {
    fn text(&self, k: &str) -> Option<String> {
        self.get(k).and_then(s_of)
    }
    fn num(&self, k: &str) -> Option<u32> {
        self.get(k).and_then(u_of)
    }
    fn flag(&self, k: &str) -> Option<bool> {
        self.get(k).and_then(|v| v.downcast_ref::<bool>().ok())
    }
    fn paths(&self, k: &str) -> Vec<String> {
        self.get(k)
            .and_then(|v| Vec::<OwnedObjectPath>::try_from(v.try_clone().ok()?).ok())
            .map(|v| v.into_iter().map(|p| p.to_string()).collect())
            .unwrap_or_default()
    }
}

fn path(p: &str) -> zbus::Result<ObjectPath<'_>> {
    ObjectPath::try_from(p).map_err(Into::into)
}

async fn get_all(conn: &Connection, p: &str, iface: &str) -> zbus::Result<Props> {
    let msg = conn.call_method(Some(NM), path(p)?, Some(PROPS), "GetAll", &(iface,)).await?;
    msg.body().deserialize::<Props>()
}

fn real_path(p: Option<String>) -> Option<String> {
    p.filter(|p| p != "/" && !p.is_empty())
}

/// Parse the settings dict of a saved connection into a [`Profile`]. `None` for kinds the
/// panel does not list (bridges, tuns, …).
pub fn profile_of(settings: &Settings) -> Option<Profile> {
    let conn = settings.get("connection")?;
    let text = |k: &str| conn.get(k).and_then(s_of);
    let kind = match text("type")?.as_str() {
        "802-3-ethernet" => ProfileKind::Wired,
        "802-11-wireless" => ProfileKind::Wifi,
        "vpn" | "wireguard" => ProfileKind::Vpn,
        _ => ProfileKind::Other,
    };
    let (method, address) = match settings.get("ipv4") {
        Some(ip) => {
            let method = match ip.get("method").and_then(s_of).as_deref() {
                Some("manual") => IpMethod::Static,
                Some("link-local") => IpMethod::LinkLocal,
                Some("disabled") => IpMethod::Disabled,
                _ => IpMethod::Dhcp,
            };
            let address = ip
                .get("address-data")
                .and_then(|v| Vec::<HashMap<String, OwnedValue>>::try_from(v.try_clone().ok()?).ok())
                .and_then(|list| {
                    let first = list.into_iter().next()?;
                    let a = first.get("address").and_then(s_of)?;
                    let p = first.get("prefix").and_then(u_of)?;
                    Some(format!("{a}/{p}"))
                });
            (method, address)
        }
        None => (IpMethod::Dhcp, None),
    };
    Some(Profile {
        id: text("id")?,
        uuid: text("uuid")?,
        kind,
        ifname: text("interface-name"),
        method,
        address,
        autoconnect: conn.get("autoconnect").and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(true),
        active: false,
    })
}

/// Read NetworkManager's whole picture. Individual failures degrade to missing pieces.
pub async fn read_snapshot(conn: &Connection) -> zbus::Result<NmSnapshot> {
    let mgr = get_all(conn, MANAGER, "org.freedesktop.NetworkManager").await?;
    let mut snap = NmSnapshot {
        connectivity: mgr.num("Connectivity").unwrap_or(0),
        wireless_enabled: mgr.flag("WirelessEnabled").unwrap_or(false),
        ..Default::default()
    };

    for dp in mgr.paths("Devices") {
        let Ok(d) = get_all(conn, &dp, "org.freedesktop.NetworkManager.Device").await else { continue };
        let mut dev = NmDevice {
            path: dp.clone(),
            ifname: d.text("Interface").unwrap_or_default(),
            dtype: d.num("DeviceType").unwrap_or(0),
            state: d.num("State").unwrap_or(0),
            ..Default::default()
        };
        if let Some(ac) = real_path(d.text("ActiveConnection"))
            && let Ok(a) = get_all(conn, &ac, "org.freedesktop.NetworkManager.Connection.Active").await
        {
            dev.active_id = a.text("Id");
            dev.active_uuid = a.text("Uuid");
        }
        if dev.dtype == DEVICE_TYPE_WIFI
            && snap.wifi.is_none()
            && let Ok(w) = get_all(conn, &dp, "org.freedesktop.NetworkManager.Device.Wireless").await
        {
            let mut wifi = NmWifi {
                device_path: dp.clone(),
                ifname: dev.ifname.clone(),
                active_ap: real_path(w.text("ActiveAccessPoint")),
                last_scan: w.get("LastScan").and_then(|v| v.downcast_ref::<i64>().ok()).unwrap_or(0),
                state: dev.state,
                ..Default::default()
            };
            for ap in w.paths("AccessPoints") {
                let Ok(a) = get_all(conn, &ap, "org.freedesktop.NetworkManager.AccessPoint").await else { continue };
                let ssid = a
                    .get("Ssid")
                    .and_then(|v| Vec::<u8>::try_from(v.try_clone().ok()?).ok())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                wifi.aps.push(NmAp {
                    path: ap,
                    ssid,
                    strength: a.num("Strength").unwrap_or(0).min(100) as u8,
                    flags: a.num("Flags").unwrap_or(0),
                    wpa: a.num("WpaFlags").unwrap_or(0),
                    rsn: a.num("RsnFlags").unwrap_or(0),
                    freq: a.num("Frequency").unwrap_or(0),
                });
            }
            snap.wifi = Some(wifi);
        }
        snap.devices.push(dev);
    }

    if let Ok(list) = conn.call_method(Some(NM), path(SETTINGS)?, Some("org.freedesktop.NetworkManager.Settings"), "ListConnections", &()).await
        && let Ok(paths) = list.body().deserialize::<Vec<OwnedObjectPath>>()
    {
        for p in paths {
            let Ok(reply) = conn
                .call_method(Some(NM), p.as_ref(), Some("org.freedesktop.NetworkManager.Settings.Connection"), "GetSettings", &())
                .await
            else {
                continue;
            };
            if let Ok(settings) = reply.body().deserialize::<Settings>()
                && let Some(profile) = profile_of(&settings)
            {
                snap.profiles.push((p.to_string(), profile));
            }
        }
    }
    Ok(snap)
}

// ── merging into the shared state ─────────────────────────────────────────────

/// Time-dependent inputs the service tracks on its own.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    /// A scan was requested and has not completed yet.
    pub scanning: bool,
    /// SSID a connect command is in flight for.
    pub connecting: Option<String>,
    pub failed: Option<String>,
    pub pending_apply: Option<PendingApply>,
    pub error: Option<String>,
}

/// Merge NetworkManager's picture into `state` (which already holds the kernel facts).
pub fn overlay(state: &mut NetworkState, nm: &NmSnapshot, ov: &Overlay) {
    state.backend = Backend::NetworkManager;
    state.can_configure = true;
    state.error = ov.error.clone();
    state.pending_apply = ov.pending_apply.clone();

    // Profiles: mark the active ones.
    state.profiles = nm
        .profiles
        .iter()
        .map(|(_, p)| {
            let mut p = p.clone();
            p.active = nm.devices.iter().any(|d| d.active_uuid.as_deref() == Some(&p.uuid) && d.state == DEV_ACTIVATED);
            p
        })
        .collect();

    // Primary: refine the kernel-derived status with the device state.
    if let Some(p) = state.primary.as_mut()
        && let Some(dev) = nm.devices.iter().find(|d| d.ifname == p.ifname)
    {
        p.profile = dev.active_id.clone();
        match dev.state {
            DEV_PREPARE..=DEV_SECONDARIES => {
                p.status = LinkStatus::Connecting;
                p.steps = connecting_steps(dev, p.kind);
                p.hint = None;
            }
            DEV_ACTIVATED => {
                if p.status == LinkStatus::Connected && matches!(nm.connectivity, CONNECTIVITY_NONE | CONNECTIVITY_PORTAL | CONNECTIVITY_LIMITED) {
                    p.status = LinkStatus::Limited;
                    p.steps = vec![
                        Step::new("Link up", "", StepStatus::Done),
                        Step::new(
                            if nm.connectivity == CONNECTIVITY_PORTAL { "Sign-in required" } else { "No internet" },
                            "",
                            StepStatus::Warn,
                        ),
                    ];
                    p.hint = Some("Connected, but NetworkManager could not reach the internet.".into());
                }
            }
            DEV_FAILED => {
                p.status = LinkStatus::Limited;
                p.steps = vec![Step::new("Activation failed", "", StepStatus::Fail)];
                p.hint = Some("The connection could not be established. Check the profile or try again.".into());
            }
            DEV_DISCONNECTED | DEV_UNAVAILABLE if p.status != LinkStatus::Unplugged => {
                // Carrier present but NM is not using the link (the user disconnected it).
                p.status = LinkStatus::Disconnected;
                p.steps.clear();
                p.hint = None;
            }
            _ => {}
        }
        // A Wi-Fi primary without association is "unplugged" for the bar.
        if p.kind == IfKind::Wifi && matches!(dev.state, DEV_UNAVAILABLE | DEV_DISCONNECTED) {
            p.status = LinkStatus::Unplugged;
        }
    }

    // VPN profiles that NM manages (WireGuard / OpenVPN): mark the matching tunnel
    // toggleable; the kernel list stays the source of "connected".
    for v in &mut state.vpns {
        v.toggleable = nm.profiles.iter().any(|(_, p)| p.kind == ProfileKind::Vpn && p.ifname.as_deref() == Some(&v.name));
    }
    for (_, p) in nm.profiles.iter().filter(|(_, p)| p.kind == ProfileKind::Vpn) {
        let name = p.ifname.clone().unwrap_or_else(|| p.id.clone());
        if !state.vpns.iter().any(|v| v.name == name) {
            state.vpns.push(Vpn {
                name,
                kind: VpnKind::Other,
                connected: false,
                addrs: Vec::new(),
                default_route: false,
                toggleable: true,
            });
        }
    }

    // Wi-Fi.
    state.wifi = nm.wifi.as_ref().map(|w| {
        let known: Vec<&str> = nm.profiles.iter().filter(|(_, p)| p.kind == ProfileKind::Wifi).map(|(_, p)| p.id.as_str()).collect();
        let mut aps: Vec<AccessPoint> = Vec::new();
        for ap in w.aps.iter().filter(|a| !a.ssid.is_empty()) {
            let item = AccessPoint {
                path: ap.path.clone(),
                ssid: ap.ssid.clone(),
                strength: ap.strength,
                security: security_of(ap.flags, ap.wpa, ap.rsn),
                freq_mhz: ap.freq,
                active: w.active_ap.as_deref() == Some(&ap.path),
                known: known.contains(&ap.ssid.as_str()),
            };
            // Same SSID on several radios/BSSIDs: keep the strongest (the active one wins).
            match aps.iter_mut().find(|a| a.ssid == item.ssid) {
                Some(existing) if item.active || (!existing.active && item.strength > existing.strength) => *existing = item,
                Some(_) => {}
                None => aps.push(item),
            }
        }
        aps.sort_by(|a, b| b.active.cmp(&a.active).then(b.strength.cmp(&a.strength)).then_with(|| a.ssid.cmp(&b.ssid)));
        WifiState {
            device: w.ifname.clone(),
            enabled: nm.wireless_enabled,
            scanning: ov.scanning,
            aps,
            connecting: ov.connecting.clone().or_else(|| {
                (matches!(w.state, DEV_PREPARE..=DEV_SECONDARIES)).then(|| {
                    nm.devices.iter().find(|d| d.path == w.device_path).and_then(|d| d.active_id.clone()).unwrap_or_default()
                })
            }),
            failed: ov.failed.clone().or_else(|| (w.state == DEV_FAILED).then(|| "connection failed".to_string())),
        }
    });
}

fn connecting_steps(dev: &NmDevice, kind: IfKind) -> Vec<Step> {
    let phase = |n: u32| dev.state > n;
    let step = |label: &str, detail: &str, done_after: u32| {
        let status = if phase(done_after) {
            StepStatus::Done
        } else if dev.state >= done_after.saturating_sub(10) {
            StepStatus::Active
        } else {
            StepStatus::Pending
        };
        Step::new(label, detail, status)
    };
    let mut v = vec![
        Step::new(if kind == IfKind::Wifi { "Associated" } else { "Cable detected" }, dev.ifname.clone(), StepStatus::Done),
        step("Link negotiated", "", 40),
        step("Getting an address", "", 70),
        Step::new("Checking internet", "—", if dev.state > 80 { StepStatus::Active } else { StepStatus::Pending }),
    ];
    if dev.state == DEV_NEED_AUTH {
        v[1] = Step::new("Authentication required", "", StepStatus::Warn);
    }
    v
}

// ── probing & commands ────────────────────────────────────────────────────────

/// Does `org.freedesktop.NetworkManager` have an owner on this bus?
pub async fn present(conn: &Connection) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else { return false };
    let Ok(name) = zbus::names::BusName::try_from(NM) else { return false };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

fn s(v: &str) -> OwnedValue {
    Value::from(v).try_to_owned().expect("str value")
}

fn b(v: bool) -> OwnedValue {
    Value::from(v).try_to_owned().expect("bool value")
}

fn dict(items: Vec<(&str, OwnedValue)>) -> HashMap<String, OwnedValue> {
    items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn new_uuid() -> String {
    // Version-4 shaped from the clock and the pid: unique enough for a profile id, no RNG crate.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let x = t ^ (u128::from(std::process::id()) << 64) ^ 0x9e37_79b9_7f4a_7c15_f39c_c060_5ced_c834;
    let h = format!("{x:032x}");
    format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
}

/// Settings for a static IPv4 profile.
pub fn static_profile_settings(cfg: &StaticConfig, uuid: &str) -> Settings {
    let addr = dict(vec![("address", s(&cfg.address.to_string())), ("prefix", Value::from(u32::from(cfg.prefix)).try_to_owned().expect("u32"))]);
    let mut ipv4 = vec![
        ("method", s("manual")),
        ("address-data", Value::from(vec![addr]).try_to_owned().expect("address-data")),
    ];
    if let Some(gw) = cfg.gateway {
        ipv4.push(("gateway", s(&gw.to_string())));
    }
    if !cfg.dns.is_empty() {
        // `dns` is an array of u32 in network byte order.
        let dns: Vec<u32> = cfg.dns.iter().map(|d| u32::from_ne_bytes(d.octets())).collect();
        ipv4.push(("dns", Value::from(dns).try_to_owned().expect("dns")));
    }
    let mut out = Settings::new();
    out.insert(
        "connection".into(),
        dict(vec![
            ("id", s(&cfg.name)),
            ("uuid", s(uuid)),
            ("type", s("802-3-ethernet")),
            ("interface-name", s(&cfg.ifname)),
            // Applied by hand; the previous profile stays the automatic one.
            ("autoconnect", b(false)),
        ]),
    );
    out.insert("802-3-ethernet".into(), dict(vec![]));
    out.insert("ipv4".into(), dict(ipv4));
    out.insert("ipv6".into(), dict(vec![("method", s("auto"))]));
    out
}

/// Settings for a new Wi-Fi profile. The passphrase is copied into the D-Bus value only
/// here, at the last moment.
pub fn wifi_profile_settings(ssid: &str, security: Security, password: Option<&Zeroizing<String>>, uuid: &str) -> Settings {
    let mut out = Settings::new();
    out.insert(
        "connection".into(),
        dict(vec![("id", s(ssid)), ("uuid", s(uuid)), ("type", s("802-11-wireless"))]),
    );
    out.insert(
        "802-11-wireless".into(),
        dict(vec![
            ("ssid", Value::from(ssid.as_bytes().to_vec()).try_to_owned().expect("ssid")),
            ("mode", s("infrastructure")),
        ]),
    );
    let key_mgmt = match security {
        Security::Open => None,
        Security::Wep => Some("none"),
        Security::Sae => Some("sae"),
        Security::Psk | Security::Enterprise => Some("wpa-psk"),
    };
    if let (Some(km), Some(pw)) = (key_mgmt, password) {
        let secret = if km == "none" { "wep-key0" } else { "psk" };
        out.insert("802-11-wireless-security".into(), dict(vec![("key-mgmt", s(km)), (secret, s(pw.as_str()))]));
    }
    out
}

/// The command side of the backend: stateless helpers over a connection.
pub struct Commands<'a> {
    pub conn: &'a Connection,
    pub nm: &'a NmSnapshot,
}

impl Commands<'_> {
    async fn call(&self, p: &str, iface: &str, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> zbus::Result<zbus::Message> {
        self.conn.call_method(Some(NM), path(p)?, Some(iface), method, body).await
    }

    fn device(&self, ifname: &str) -> Option<&NmDevice> {
        self.nm.devices.iter().find(|d| d.ifname == ifname)
    }

    fn profile_path(&self, uuid: &str) -> Option<&str> {
        self.nm.profiles.iter().find(|(_, p)| p.uuid == uuid).map(|(pp, _)| pp.as_str())
    }

    async fn activate(&self, profile: &str, device: &str, specific: &str) -> zbus::Result<()> {
        self.call(
            MANAGER,
            "org.freedesktop.NetworkManager",
            "ActivateConnection",
            &(path(profile)?, path(device)?, path(specific)?),
        )
        .await
        .map(|_| ())
    }

    /// Connect `ifname` with its best profile (NM picks when passed `/`).
    pub async fn connect_device(&self, ifname: &str) -> Result<(), String> {
        let dev = self.device(ifname).ok_or_else(|| format!("no such device: {ifname}"))?;
        let profile = self
            .nm
            .profiles
            .iter()
            .find(|(_, p)| p.ifname.as_deref() == Some(ifname) && p.autoconnect)
            .or_else(|| self.nm.profiles.iter().find(|(_, p)| p.kind == ProfileKind::Wired && p.ifname.is_none() && dev.dtype == DEVICE_TYPE_ETHERNET))
            .map(|(pp, _)| pp.clone())
            .unwrap_or_else(|| "/".into());
        self.activate(&profile, &dev.path, "/").await.map_err(|e| e.to_string())
    }

    pub async fn disconnect_device(&self, ifname: &str) -> Result<(), String> {
        let dev = self.device(ifname).ok_or_else(|| format!("no such device: {ifname}"))?;
        self.call(&dev.path, "org.freedesktop.NetworkManager.Device", "Disconnect", &()).await.map(|_| ()).map_err(|e| e.to_string())
    }

    pub async fn activate_profile(&self, uuid: &str) -> Result<(), String> {
        let pp = self.profile_path(uuid).ok_or("unknown profile")?;
        let profile = self.nm.profiles.iter().find(|(p, _)| p == pp).map(|(_, p)| p).ok_or("unknown profile")?;
        let dev = match &profile.ifname {
            Some(n) => self.device(n),
            None => {
                let want = if profile.kind == ProfileKind::Wifi { DEVICE_TYPE_WIFI } else { DEVICE_TYPE_ETHERNET };
                self.nm.devices.iter().find(|d| d.dtype == want)
            }
        }
        .ok_or("no device for this profile")?;
        self.activate(pp, &dev.path, "/").await.map_err(|e| e.to_string())
    }

    pub async fn forget_profile(&self, uuid: &str) -> Result<(), String> {
        let pp = self.profile_path(uuid).ok_or("unknown profile")?;
        self.call(pp, "org.freedesktop.NetworkManager.Settings.Connection", "Delete", &()).await.map(|_| ()).map_err(|e| e.to_string())
    }

    /// Add and activate a static profile; returns its object path.
    pub async fn apply_static(&self, cfg: &StaticConfig) -> Result<String, String> {
        let dev = self.device(&cfg.ifname).ok_or_else(|| format!("no such device: {}", cfg.ifname))?;
        let settings = static_profile_settings(cfg, &new_uuid());
        let msg = self
            .call(MANAGER, "org.freedesktop.NetworkManager", "AddAndActivateConnection", &(settings, path(&dev.path).map_err(|e| e.to_string())?, path("/").map_err(|e| e.to_string())?))
            .await
            .map_err(|e| e.to_string())?;
        let (profile, _active): (OwnedObjectPath, OwnedObjectPath) = msg.body().deserialize().map_err(|e| e.to_string())?;
        Ok(profile.to_string())
    }

    pub async fn delete_path(&self, profile_path: &str) {
        let _ = self.call(profile_path, "org.freedesktop.NetworkManager.Settings.Connection", "Delete", &()).await;
    }

    pub async fn wifi_set_enabled(&self, on: bool) -> Result<(), String> {
        self.call(MANAGER, PROPS, "Set", &("org.freedesktop.NetworkManager", "WirelessEnabled", Value::from(on)))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub async fn wifi_scan(&self) -> Result<(), String> {
        let w = self.nm.wifi.as_ref().ok_or("no Wi-Fi device")?;
        let opts: HashMap<String, OwnedValue> = HashMap::new();
        self.call(&w.device_path, "org.freedesktop.NetworkManager.Device.Wireless", "RequestScan", &(opts,))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Connect to `ssid`: a saved profile is activated as is; otherwise a profile with the
    /// given passphrase is created and activated in one NetworkManager call.
    pub async fn wifi_connect(&self, ssid: &str, password: Option<&Zeroizing<String>>) -> Result<(), String> {
        let w = self.nm.wifi.as_ref().ok_or("no Wi-Fi device")?;
        let ap = w
            .aps
            .iter()
            .filter(|a| a.ssid == ssid)
            .max_by_key(|a| a.strength)
            .ok_or_else(|| format!("{ssid} is out of range"))?;
        let saved = self.nm.profiles.iter().find(|(_, p)| p.kind == ProfileKind::Wifi && p.id == ssid);
        if let (Some((pp, _)), None) = (saved, password) {
            return self.activate(pp, &w.device_path, &ap.path).await.map_err(|e| e.to_string());
        }
        let security = security_of(ap.flags, ap.wpa, ap.rsn);
        if security != Security::Open && password.is_none() {
            return Err("password required".into());
        }
        // Replace a stale saved profile (wrong password before) instead of piling them up.
        if let Some((pp, _)) = saved {
            self.delete_path(pp).await;
        }
        let settings = wifi_profile_settings(ssid, security, password, &new_uuid());
        self.call(
            MANAGER,
            "org.freedesktop.NetworkManager",
            "AddAndActivateConnection",
            &(settings, path(&w.device_path).map_err(|e| e.to_string())?, path(&ap.path).map_err(|e| e.to_string())?),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub async fn wifi_disconnect(&self) -> Result<(), String> {
        let w = self.nm.wifi.as_ref().ok_or("no Wi-Fi device")?;
        self.call(&w.device_path, "org.freedesktop.NetworkManager.Device", "Disconnect", &()).await.map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Safe-apply bookkeeping: what to restore, and when.
pub fn pending(profile: &str, revert_to: Option<String>, now: Instant, window: Duration) -> PendingApply {
    PendingApply { profile: profile.to_string(), revert_to, deadline: now + window }
}

/// Parse `a.b.c.d/len` for the static editor.
pub fn parse_cidr(text: &str) -> Option<(Ipv4Addr, u8)> {
    let (a, p) = text.trim().split_once('/')?;
    let prefix: u8 = p.trim().parse().ok().filter(|p| *p <= 32)?;
    Some((a.trim().parse().ok()?, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_classes() {
        assert_eq!(security_of(0, 0, 0), Security::Open);
        assert_eq!(security_of(1, 0, 0), Security::Wep);
        assert_eq!(security_of(1, 0, AP_SEC_KEY_MGMT_PSK), Security::Psk);
        assert_eq!(security_of(1, 0, AP_SEC_KEY_MGMT_SAE), Security::Sae);
        assert_eq!(security_of(1, 0, AP_SEC_KEY_MGMT_SAE | AP_SEC_KEY_MGMT_PSK), Security::Psk);
        assert_eq!(security_of(1, 0, AP_SEC_KEY_MGMT_802_1X), Security::Enterprise);
    }

    #[test]
    fn cidr() {
        assert_eq!(parse_cidr("10.0.0.5/24"), Some((Ipv4Addr::new(10, 0, 0, 5), 24)));
        assert_eq!(parse_cidr("10.0.0.5/33"), None);
        assert_eq!(parse_cidr("10.0.0.5"), None);
        assert_eq!(parse_cidr("x/24"), None);
    }

    #[test]
    fn uuid_shape() {
        let u = new_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.matches('-').count(), 4);
    }

    #[test]
    fn static_settings_shape() {
        let cfg = StaticConfig {
            name: "Lab".into(),
            ifname: "enp5s0".into(),
            address: Ipv4Addr::new(10, 0, 0, 5),
            prefix: 24,
            gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
            dns: vec![Ipv4Addr::new(1, 1, 1, 1)],
        };
        let st = static_profile_settings(&cfg, "u-1");
        let p = profile_of(&st).unwrap();
        assert_eq!((p.id.as_str(), p.method, p.address.as_deref(), p.autoconnect), ("Lab", IpMethod::Static, Some("10.0.0.5/24"), false));
        assert_eq!(p.ifname.as_deref(), Some("enp5s0"));
    }

    #[test]
    fn wifi_settings_carry_the_secret_only_in_the_security_section() {
        let pw = Zeroizing::new("hunter22".to_string());
        let st = wifi_profile_settings("studio-5G", Security::Psk, Some(&pw), "u-2");
        let sec = st.get("802-11-wireless-security").unwrap();
        assert_eq!(sec.get("psk").and_then(s_of).as_deref(), Some("hunter22"));
        assert_eq!(sec.get("key-mgmt").and_then(s_of).as_deref(), Some("wpa-psk"));
        assert!(!st.contains_key("ipv4"));
        // Open networks have no security section; a secured one without a password too.
        assert!(!wifi_profile_settings("cafe", Security::Open, None, "u").contains_key("802-11-wireless-security"));
        assert!(!wifi_profile_settings("x", Security::Psk, None, "u").contains_key("802-11-wireless-security"));
    }

    fn dev(state: u32) -> NmDevice {
        NmDevice { path: "/d/1".into(), ifname: "enp5s0".into(), dtype: DEVICE_TYPE_ETHERNET, state, active_id: Some("Home".into()), active_uuid: Some("u-home".into()) }
    }

    fn base_state() -> NetworkState {
        NetworkState {
            available: true,
            backend: Backend::ReadOnly,
            primary: Some(crate::model::Primary {
                ifname: "enp5s0".into(),
                kind: IfKind::Wired,
                status: LinkStatus::Connected,
                profile: None,
                steps: vec![],
                hint: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn overlay_refines_status_and_profiles() {
        let home = Profile { id: "Home".into(), uuid: "u-home".into(), kind: ProfileKind::Wired, ifname: Some("enp5s0".into()), autoconnect: true, ..Default::default() };
        let lab = Profile { id: "Lab".into(), uuid: "u-lab".into(), method: IpMethod::Static, address: Some("10.0.0.5/24".into()), ..home.clone() };
        let mut nm = NmSnapshot {
            connectivity: 4,
            devices: vec![dev(DEV_ACTIVATED)],
            profiles: vec![("/p/1".into(), home), ("/p/2".into(), lab)],
            ..Default::default()
        };
        let mut st = base_state();
        overlay(&mut st, &nm, &Overlay::default());
        assert!(st.can_configure);
        assert_eq!(st.backend, Backend::NetworkManager);
        assert_eq!(st.primary.as_ref().unwrap().profile.as_deref(), Some("Home"));
        assert_eq!(st.primary.as_ref().unwrap().status, LinkStatus::Connected);
        assert_eq!(st.profiles.iter().filter(|p| p.active).map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["Home"]);
        assert_eq!(st.profiles[1].summary(), "static · 10.0.0.5/24");
        assert_eq!(st.profiles[0].summary(), "DHCP · auto-connect");

        // Connectivity "limited" while activated.
        nm.connectivity = CONNECTIVITY_LIMITED;
        let mut st = base_state();
        overlay(&mut st, &nm, &Overlay::default());
        assert_eq!(st.primary.as_ref().unwrap().status, LinkStatus::Limited);

        // IP config in progress.
        nm.devices = vec![dev(70)];
        let mut st = base_state();
        overlay(&mut st, &nm, &Overlay::default());
        let p = st.primary.unwrap();
        assert_eq!(p.status, LinkStatus::Connecting);
        assert_eq!(p.steps[2].status, StepStatus::Active);

        // Disconnected by the user (carrier is still there).
        nm.devices = vec![dev(DEV_DISCONNECTED)];
        let mut st = base_state();
        overlay(&mut st, &nm, &Overlay::default());
        assert_eq!(st.primary.unwrap().status, LinkStatus::Disconnected);
    }

    #[test]
    fn overlay_wifi_dedups_ssids_and_sorts() {
        let ap = |path: &str, ssid: &str, strength: u8, rsn: u32, freq: u32| NmAp { path: path.into(), ssid: ssid.into(), strength, flags: 1, wpa: 0, rsn, freq };
        let nm = NmSnapshot {
            connectivity: 4,
            wireless_enabled: true,
            wifi: Some(NmWifi {
                device_path: "/d/2".into(),
                ifname: "wlan0".into(),
                aps: vec![
                    ap("/a/1", "studio-5G", 55, AP_SEC_KEY_MGMT_PSK, 5180),
                    ap("/a/2", "home", 80, AP_SEC_KEY_MGMT_PSK, 5240),
                    ap("/a/3", "home", 30, AP_SEC_KEY_MGMT_PSK, 2412),
                    NmAp { path: "/a/4".into(), ssid: "cafe-guest".into(), strength: 25, ..Default::default() },
                    NmAp { path: "/a/5".into(), ssid: "".into(), strength: 99, ..Default::default() },
                ],
                active_ap: Some("/a/2".into()),
                ..Default::default()
            }),
            profiles: vec![("/p/1".into(), Profile { id: "home".into(), uuid: "u".into(), kind: ProfileKind::Wifi, ..Default::default() })],
            ..Default::default()
        };
        let mut st = NetworkState::default();
        overlay(&mut st, &nm, &Overlay { scanning: true, ..Default::default() });
        let w = st.wifi.unwrap();
        assert!(w.enabled && w.scanning);
        assert_eq!(w.aps.iter().map(|a| a.ssid.as_str()).collect::<Vec<_>>(), vec!["home", "studio-5G", "cafe-guest"]);
        assert!(w.aps[0].active && w.aps[0].known);
        assert_eq!(w.aps[0].band(), "5 GHz");
        assert_eq!(w.aps[0].bars(), 4);
        assert_eq!(w.aps[2].security, Security::Open);
        assert_eq!(w.active().unwrap().ssid, "home");
    }
}
