//! The NetworkManager backend against a mock NetworkManager served over a private
//! peer-to-peer connection (a socketpair): no bus, no real NetworkManager. Every mutating
//! path (activate, disconnect, static profile + safe-apply, Wi-Fi connect with a
//! password, scan, forget) is checked against the mock's call log.

use std::{
    collections::HashMap,
    fs,
    os::unix::net::UnixStream,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_channel::Receiver;
use ashd_svc_network::{
    Backend, Command, Handle, IpMethod, LinkStatus, NetworkState, NmMode, Options, Paths, Security, StaticConfig, Zeroizing, start_with,
};
use zbus::{
    Connection,
    connection::Builder,
    interface,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

type Log = Arc<Mutex<Vec<String>>>;
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

const NM: &str = "/org/freedesktop/NetworkManager";
const DEV_ETH: &str = "/org/freedesktop/NetworkManager/Devices/1";
const DEV_WIFI: &str = "/org/freedesktop/NetworkManager/Devices/2";
const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/1";
const PROFILE_HOME: &str = "/org/freedesktop/NetworkManager/Settings/1";
const PROFILE_LAB: &str = "/org/freedesktop/NetworkManager/Settings/2";
const PROFILE_WIFI: &str = "/org/freedesktop/NetworkManager/Settings/3";
const AP_HOME: &str = "/org/freedesktop/NetworkManager/AccessPoint/1";
const AP_STUDIO: &str = "/org/freedesktop/NetworkManager/AccessPoint/2";
const AP_CAFE: &str = "/org/freedesktop/NetworkManager/AccessPoint/3";

fn val(v: impl Into<Value<'static>>) -> OwnedValue {
    v.into().try_to_owned().unwrap()
}

fn op(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p).unwrap()
}

struct Manager {
    log: Log,
    connectivity: u32,
    wifi_enabled: bool,
}

#[interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    fn activate_connection(&self, connection: ObjectPath<'_>, device: ObjectPath<'_>, specific: ObjectPath<'_>) -> OwnedObjectPath {
        self.log.lock().unwrap().push(format!("ActivateConnection {connection} {device} {specific}"));
        op(ACTIVE)
    }
    fn add_and_activate_connection(&self, connection: Settings, device: ObjectPath<'_>, specific: ObjectPath<'_>) -> (OwnedObjectPath, OwnedObjectPath) {
        let sec = connection.get("802-11-wireless-security");
        let psk = sec.and_then(|s| s.get("psk")).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or("-");
        let ssid = connection
            .get("802-11-wireless")
            .and_then(|w| w.get("ssid"))
            .and_then(|v| Vec::<u8>::try_from(v.try_clone().unwrap()).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|| "-".into());
        let method = connection.get("ipv4").and_then(|i| i.get("method")).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or("-");
        let id = connection.get("connection").and_then(|c| c.get("id")).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or("-");
        let autoconnect = connection.get("connection").and_then(|c| c.get("autoconnect")).and_then(|v| v.downcast_ref::<bool>().ok());
        self.log.lock().unwrap().push(format!(
            "AddAndActivateConnection id={id} ssid={ssid} psk={psk} method={method} autoconnect={autoconnect:?} dev={device} ap={specific}"
        ));
        (op("/org/freedesktop/NetworkManager/Settings/9"), op(ACTIVE))
    }
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        self.connectivity
    }
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.wifi_enabled
    }
    #[zbus(property)]
    fn set_wireless_enabled(&mut self, on: bool) {
        self.log.lock().unwrap().push(format!("WirelessEnabled={on}"));
        self.wifi_enabled = on;
    }
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![op(DEV_ETH), op(DEV_WIFI)]
    }
}

struct SettingsRoot;

#[interface(name = "org.freedesktop.NetworkManager.Settings")]
impl SettingsRoot {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        vec![op(PROFILE_HOME), op(PROFILE_LAB), op(PROFILE_WIFI)]
    }
}

struct Profile {
    log: Log,
    path: &'static str,
    settings: Settings,
}

#[interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl Profile {
    fn get_settings(&self) -> Settings {
        self.settings.clone()
    }
    fn delete(&self) {
        self.log.lock().unwrap().push(format!("Delete {}", self.path));
    }
}

fn profile_settings(id: &str, uuid: &str, ty: &str, ifname: Option<&str>, method: &str, address: Option<(&str, u32)>, autoconnect: bool) -> Settings {
    let mut conn: HashMap<String, OwnedValue> =
        HashMap::from([("id".into(), val(id.to_string())), ("uuid".into(), val(uuid.to_string())), ("type".into(), val(ty.to_string())), ("autoconnect".into(), val(autoconnect))]);
    if let Some(i) = ifname {
        conn.insert("interface-name".into(), val(i.to_string()));
    }
    let mut ipv4: HashMap<String, OwnedValue> = HashMap::from([("method".into(), val(method.to_string()))]);
    if let Some((a, p)) = address {
        let item: HashMap<String, OwnedValue> = HashMap::from([("address".into(), val(a.to_string())), ("prefix".into(), val(p))]);
        ipv4.insert("address-data".into(), val(vec![item]));
    }
    HashMap::from([("connection".to_string(), conn), ("ipv4".to_string(), ipv4)])
}

struct Device {
    log: Log,
    path: &'static str,
    ifname: &'static str,
    dtype: u32,
    state: u32,
    active: bool,
}

#[interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    fn disconnect(&self) {
        self.log.lock().unwrap().push(format!("Disconnect {}", self.path));
    }
    #[zbus(property, name = "Interface")]
    fn ifname(&self) -> String {
        self.ifname.into()
    }
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        self.dtype
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn active_connection(&self) -> OwnedObjectPath {
        op(if self.active { ACTIVE } else { "/" })
    }
}

struct ActiveConn(&'static str, &'static str);

#[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl ActiveConn {
    #[zbus(property)]
    fn id(&self) -> String {
        self.0.into()
    }
    #[zbus(property)]
    fn uuid(&self) -> String {
        self.1.into()
    }
}

struct Wireless {
    log: Log,
}

#[interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    fn request_scan(&self, _options: HashMap<String, OwnedValue>) {
        self.log.lock().unwrap().push("RequestScan".into());
    }
    #[zbus(property)]
    fn access_points(&self) -> Vec<OwnedObjectPath> {
        vec![op(AP_HOME), op(AP_STUDIO), op(AP_CAFE)]
    }
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        op("/")
    }
    #[zbus(property)]
    fn last_scan(&self) -> i64 {
        1000
    }
}

struct Ap {
    ssid: &'static str,
    strength: u8,
    rsn: u32,
    freq: u32,
}

#[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl Ap {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        self.ssid.as_bytes().to_vec()
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        self.strength
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        u32::from(self.rsn != 0)
    }
    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.rsn
    }
    #[zbus(property)]
    fn frequency(&self) -> u32 {
        self.freq
    }
}

struct Fixture {
    _server: Connection,
    _dir: tempfile::TempDir,
    log: Log,
    rx: Receiver<NetworkState>,
    handle: Handle,
}

fn put(root: &Path, name: &str, files: &[(&str, &str)], dirs: &[&str]) {
    let d = root.join(name);
    fs::create_dir_all(&d).unwrap();
    for dir in dirs {
        fs::create_dir_all(d.join(dir)).unwrap();
    }
    for (f, c) in files {
        let p = d.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, format!("{c}\n")).unwrap();
    }
}

fn fixture(fake: bool, connectivity: u32, safe_apply: Duration) -> Fixture {
    let log: Log = Log::default();
    let (a, b) = UnixStream::pair().unwrap();
    let mk_dev = |path, ifname, dtype, state, active| Device { log: log.clone(), path, ifname, dtype, state, active };
    let (server, client) = smol::block_on(async {
        let server = Builder::async_io_unix_stream(a)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(NM, Manager { log: log.clone(), connectivity, wifi_enabled: true })
            .unwrap()
            .serve_at("/org/freedesktop/NetworkManager/Settings", SettingsRoot)
            .unwrap()
            .serve_at(PROFILE_HOME, Profile { log: log.clone(), path: PROFILE_HOME, settings: profile_settings("Home", "u-home", "802-3-ethernet", Some("enp5s0"), "auto", None, true) })
            .unwrap()
            .serve_at(
                PROFILE_LAB,
                Profile { log: log.clone(), path: PROFILE_LAB, settings: profile_settings("Lab", "u-lab", "802-3-ethernet", Some("enp5s0"), "manual", Some(("10.0.0.5", 24)), false) },
            )
            .unwrap()
            .serve_at(PROFILE_WIFI, Profile { log: log.clone(), path: PROFILE_WIFI, settings: profile_settings("home", "u-wifi", "802-11-wireless", None, "auto", None, true) })
            .unwrap()
            // The trial profile AddAndActivateConnection "creates" (deletable, not listed).
            .serve_at("/org/freedesktop/NetworkManager/Settings/9", Profile { log: log.clone(), path: "/org/freedesktop/NetworkManager/Settings/9", settings: Settings::new() })
            .unwrap()
            .serve_at(DEV_ETH, mk_dev(DEV_ETH, "enp5s0", 1, 100, true))
            .unwrap()
            .serve_at(DEV_WIFI, mk_dev(DEV_WIFI, "wlan0", 2, 30, false))
            .unwrap()
            .serve_at(DEV_WIFI, Wireless { log: log.clone() })
            .unwrap()
            .serve_at(ACTIVE, ActiveConn("Home", "u-home"))
            .unwrap()
            .serve_at(AP_HOME, Ap { ssid: "home", strength: 88, rsn: 0x100, freq: 5240 })
            .unwrap()
            .serve_at(AP_STUDIO, Ap { ssid: "studio-5G", strength: 62, rsn: 0x100, freq: 5180 })
            .unwrap()
            .serve_at(AP_CAFE, Ap { ssid: "cafe-guest", strength: 35, rsn: 0, freq: 2412 })
            .unwrap()
            .build();
        let client = Builder::async_io_unix_stream(b).p2p().build();
        let (s, c) = smol::future::zip(server, client).await;
        (s.unwrap(), c.unwrap())
    });

    // Kernel facts (sysfs fixture): enp5s0 up with an address and a default route.
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    put(
        &paths.sys_net,
        "enp5s0",
        &[("operstate", "up"), ("carrier", "1"), ("speed", "1000"), ("duplex", "full"), ("mtu", "1500"), ("carrier_up_count", "1")],
        &["device"],
    );
    put(&paths.sys_net, "wlan0", &[("operstate", "down"), ("type", "1")], &["device", "wireless"]);
    fs::create_dir_all(paths.proc_route.parent().unwrap()).unwrap();
    fs::write(&paths.proc_route, "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\nenp5s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n").unwrap();
    let addrs: Box<dyn Fn() -> ashd_svc_network::Addrs + Send> = Box::new(|| vec![("enp5s0".into(), "192.168.1.24".parse().unwrap(), 24)]);
    let (handle, rx) = start_with(Options {
        paths,
        addrs: Some(addrs),
        usage_file: None,
        nm: NmMode::Connection(client),
        fake_commands: fake,
        mock: None,
        safe_apply,
    });
    Fixture { _server: server, _dir: dir, log, rx, handle }
}

fn wait_for(rx: &Receiver<NetworkState>, what: &str, pred: impl Fn(&NetworkState) -> bool) -> NetworkState {
    smol::block_on(smol::future::or(
        async {
            loop {
                match rx.recv().await {
                    Ok(s) if pred(&s) => return s,
                    Ok(_) => {}
                    Err(_) => panic!("state channel closed waiting for {what}"),
                }
            }
        },
        async {
            smol::Timer::after(Duration::from_secs(10)).await;
            panic!("timed out waiting for: {what}")
        },
    ))
}

fn wait_log(log: &Log, want: &str) -> String {
    for _ in 0..200 {
        if let Some(l) = log.lock().unwrap().iter().find(|l| l.contains(want)) {
            return l.clone();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("mock never saw {want:?}; log: {:?}", log.lock().unwrap());
}

fn count(log: &Log, want: &str) -> usize {
    log.lock().unwrap().iter().filter(|l| l.contains(want)).count()
}

#[test]
fn state_merges_kernel_facts_with_networkmanager() {
    let f = fixture(false, 4, Duration::from_secs(15));
    let s = wait_for(&f.rx, "nm state", |s| s.backend == Backend::NetworkManager && s.wifi.is_some());
    assert!(s.can_configure);
    let p = s.primary.clone().unwrap();
    assert_eq!((p.ifname.as_str(), p.status, p.profile.as_deref()), ("enp5s0", LinkStatus::Connected, Some("Home")));
    assert_eq!(s.tile_title(), "Ethernet");
    assert_eq!(s.tile_subtitle(), "enp5s0 · 1 Gb/s");

    assert_eq!(s.profiles.len(), 3);
    let home = s.profiles.iter().find(|p| p.id == "Home").unwrap();
    assert!(home.active && home.autoconnect);
    assert_eq!(home.summary(), "DHCP · auto-connect");
    let lab = s.profiles.iter().find(|p| p.id == "Lab").unwrap();
    assert_eq!((lab.method, lab.address.as_deref(), lab.active), (IpMethod::Static, Some("10.0.0.5/24"), false));
    assert_eq!(lab.summary(), "static · 10.0.0.5/24");

    let w = s.wifi.unwrap();
    assert!(w.enabled && !w.scanning);
    assert_eq!(w.aps.iter().map(|a| a.ssid.as_str()).collect::<Vec<_>>(), vec!["home", "studio-5G", "cafe-guest"]);
    assert!(w.aps[0].known && !w.aps[1].known);
    assert_eq!(w.aps[1].security, Security::Psk);
    assert_eq!(w.aps[2].security, Security::Open);
}

#[test]
fn limited_connectivity_shows_up() {
    let f = fixture(false, 3, Duration::from_secs(15));
    let s = wait_for(&f.rx, "limited", |s| s.status() == LinkStatus::Limited);
    assert!(s.primary.unwrap().hint.is_some());
}

#[test]
fn activate_disconnect_forget_scan_and_wifi_power() {
    let f = fixture(false, 4, Duration::from_secs(15));
    wait_for(&f.rx, "ready", |s| s.wifi.is_some());

    f.handle.activate_profile("u-lab");
    assert_eq!(wait_log(&f.log, "ActivateConnection /org"), format!("ActivateConnection {PROFILE_LAB} {DEV_ETH} /"));

    f.handle.disconnect_device("enp5s0");
    wait_log(&f.log, &format!("Disconnect {DEV_ETH}"));

    f.handle.forget_profile("u-lab");
    wait_log(&f.log, &format!("Delete {PROFILE_LAB}"));

    f.handle.wifi_set_enabled(false);
    wait_log(&f.log, "WirelessEnabled=false");

    f.handle.wifi_scan();
    wait_log(&f.log, "RequestScan");
    wait_for(&f.rx, "scanning flag", |s| s.wifi.as_ref().is_some_and(|w| w.scanning));
}

#[test]
fn wifi_connect_hands_the_password_to_networkmanager_only() {
    let f = fixture(false, 4, Duration::from_secs(15));
    wait_for(&f.rx, "ready", |s| s.wifi.is_some());

    // A saved profile needs no password: plain activation against the AP.
    f.handle.wifi_connect("home", None);
    assert_eq!(wait_log(&f.log, "ActivateConnection /org"), format!("ActivateConnection {PROFILE_WIFI} {DEV_WIFI} {AP_HOME}"));

    // A new secured network: profile + passphrase created in one call.
    f.handle.wifi_connect("studio-5G", Some(Zeroizing::new("hunter22".to_string())));
    let line = wait_log(&f.log, "AddAndActivateConnection id=studio-5G");
    assert!(line.contains("ssid=studio-5G") && line.contains("psk=hunter22"), "{line}");
    assert!(line.contains(&format!("dev={DEV_WIFI} ap={AP_STUDIO}")), "{line}");
    let s = wait_for(&f.rx, "connecting marker", |s| s.wifi.as_ref().is_some_and(|w| w.connecting.as_deref() == Some("studio-5G")));
    // Nothing in the published state carries the secret.
    assert!(!format!("{s:?}").contains("hunter22"));

    // A secured network without a password is refused before anything is sent.
    f.handle.wifi_connect("studio-5G", None);
    let s = wait_for(&f.rx, "error", |s| s.error.as_deref() == Some("password required"));
    assert!(s.wifi.unwrap().connecting.is_none());
    assert_eq!(count(&f.log, "AddAndActivateConnection"), 1);

    // An open network.
    f.handle.wifi_connect("cafe-guest", None);
    let line = wait_log(&f.log, "AddAndActivateConnection id=cafe-guest");
    assert!(line.contains("psk=-"), "{line}");
}

#[test]
fn command_debug_never_prints_the_secret() {
    let c = Command::WifiConnect { ssid: "x".into(), password: Some(Zeroizing::new("s3cret-pass".to_string())) };
    let text = format!("{c:?}");
    assert!(!text.contains("s3cret-pass") && text.contains("redacted"), "{text}");
}

fn lab_config() -> StaticConfig {
    StaticConfig {
        name: "Lab".into(),
        ifname: "enp5s0".into(),
        address: "10.0.0.5".parse().unwrap(),
        prefix: 24,
        gateway: Some("10.0.0.1".parse().unwrap()),
        dns: vec!["1.1.1.1".parse().unwrap()],
    }
}

#[test]
fn static_profile_is_kept_when_confirmed() {
    let f = fixture(false, 4, Duration::from_secs(30));
    wait_for(&f.rx, "ready", |s| s.wifi.is_some());
    f.handle.apply_static(lab_config());
    let line = wait_log(&f.log, "AddAndActivateConnection id=Lab");
    assert!(line.contains("method=manual") && line.contains("autoconnect=Some(false)"), "{line}");
    let s = wait_for(&f.rx, "banner", |s| s.pending_apply.is_some());
    let p = s.pending_apply.unwrap();
    assert_eq!((p.profile.as_str(), p.revert_to.as_deref()), ("Lab", Some("Home")));
    assert!(p.deadline > std::time::Instant::now());

    f.handle.keep_apply();
    wait_for(&f.rx, "banner gone", |s| s.pending_apply.is_none());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(count(&f.log, "ActivateConnection /org"), 0, "no revert after Keep: {:?}", f.log.lock().unwrap());
    assert_eq!(count(&f.log, "Delete"), 0);
}

#[test]
fn static_profile_reverts_after_the_timeout() {
    let f = fixture(false, 4, Duration::from_millis(900));
    wait_for(&f.rx, "ready", |s| s.wifi.is_some());
    f.handle.apply_static(lab_config());
    wait_for(&f.rx, "banner", |s| s.pending_apply.is_some());
    // No answer: back to Home, trial profile deleted.
    let line = wait_log(&f.log, "ActivateConnection /org");
    assert_eq!(line, format!("ActivateConnection {PROFILE_HOME} {DEV_ETH} /"));
    wait_log(&f.log, "Delete /org/freedesktop/NetworkManager/Settings/9");
    wait_for(&f.rx, "banner gone", |s| s.pending_apply.is_none());
}

#[test]
fn explicit_revert_is_immediate() {
    let f = fixture(false, 4, Duration::from_secs(30));
    wait_for(&f.rx, "ready", |s| s.wifi.is_some());
    f.handle.apply_static(lab_config());
    wait_for(&f.rx, "banner", |s| s.pending_apply.is_some());
    f.handle.revert_apply();
    wait_log(&f.log, &format!("ActivateConnection {PROFILE_HOME}"));
    wait_for(&f.rx, "banner gone", |s| s.pending_apply.is_none());
}

#[test]
fn nested_mode_logs_instead_of_sending() {
    let f = fixture(true, 4, Duration::from_secs(15));
    wait_for(&f.rx, "ready", |s| s.wifi.is_some());
    f.handle.activate_profile("u-lab");
    f.handle.disconnect_device("enp5s0");
    f.handle.wifi_connect("studio-5G", Some(Zeroizing::new("hunter22".to_string())));
    f.handle.apply_static(lab_config());
    f.handle.wifi_scan();
    f.handle.forget_profile("u-lab");
    std::thread::sleep(Duration::from_millis(400));
    assert!(f.log.lock().unwrap().is_empty(), "nothing may reach NetworkManager: {:?}", f.log.lock().unwrap());
    // Reading still works.
    f.handle.refresh();
    wait_for(&f.rx, "still serving", |s| s.backend == Backend::NetworkManager);
}
