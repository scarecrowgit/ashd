//! The Bluetooth service against a mock BlueZ (`org.bluez`: ObjectManager, an adapter,
//! three devices, Battery1, AgentManager1 and a pairing flow that calls our Agent1) served
//! over a private peer-to-peer connection — no bus, never the real adapter.

use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_channel::Receiver;
use ashd_svc_bluetooth::{BluetoothState, Busy, DeviceKind, Handle, PairingKind, PairingReply, start_on};
use zbus::{
    Connection,
    connection::Builder,
    interface,
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedObjectPath},
};

type Log = Arc<Mutex<Vec<String>>>;

const ADAPTER: &str = "/org/bluez/hci0";
const HEADPHONES: &str = "/org/bluez/hci0/dev_11_22_33_44_55_01";
const KEYBOARD: &str = "/org/bluez/hci0/dev_11_22_33_44_55_02";
const MOUSE: &str = "/org/bluez/hci0/dev_11_22_33_44_55_03";
const AGENT: &str = "/org/ashd/bluez/agent";

struct Adapter {
    log: Log,
    powered: bool,
    discovering: bool,
}

#[interface(name = "org.bluez.Adapter1")]
impl Adapter {
    async fn start_discovery(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push("StartDiscovery".into());
        self.discovering = true;
        self.discovering_changed(&em).await?;
        Ok(())
    }
    async fn stop_discovery(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push("StopDiscovery".into());
        self.discovering = false;
        self.discovering_changed(&em).await?;
        Ok(())
    }
    async fn remove_device(&self, device: ObjectPath<'_>, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push(format!("RemoveDevice {device}"));
        conn.object_server().remove::<Device, _>(device).await?;
        Ok(())
    }
    #[zbus(property)]
    fn address(&self) -> String {
        "AA:BB:CC:00:00:01".into()
    }
    #[zbus(property)]
    fn alias(&self) -> String {
        "mock-box".into()
    }
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered
    }
    #[zbus(property)]
    fn set_powered(&mut self, on: bool) {
        self.log.lock().unwrap().push(format!("Powered={on}"));
        self.powered = on;
    }
    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.discovering
    }
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn pairable(&self) -> bool {
        true
    }
}

struct Device {
    log: Log,
    address: &'static str,
    name: &'static str,
    icon: &'static str,
    paired: bool,
    trusted: bool,
    connected: bool,
    rssi: Option<i16>,
    /// Which agent request `Pair` makes: `confirm`, `authorize`, `reject`.
    flow: &'static str,
}

#[interface(name = "org.bluez.Device1")]
impl Device {
    async fn connect(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push(format!("Connect {}", self.address));
        self.connected = true;
        self.connected_changed(&em).await?;
        Ok(())
    }
    async fn disconnect(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push(format!("Disconnect {}", self.address));
        self.connected = false;
        self.connected_changed(&em).await?;
        Ok(())
    }
    async fn pair(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push(format!("Pair {}", self.address));
        let me = OwnedObjectPath::try_from(format!("/org/bluez/hci0/dev_{}", self.address.replace(':', "_"))).unwrap();
        let agent = zbus::Proxy::new(conn, "org.bluez", AGENT, "org.bluez.Agent1").await?;
        let answer: zbus::Result<()> = match self.flow {
            "confirm" => agent.call("RequestConfirmation", &(&me, 123_456u32)).await,
            "authorize" => agent.call("RequestAuthorization", &(&me,)).await,
            _ => Err(zbus::Error::Failure("unused".into())),
        };
        if let Err(e) = answer {
            self.log.lock().unwrap().push(format!("agent said no: {e}"));
            return Err(zbus::fdo::Error::Failed("Authentication Rejected".into()));
        }
        self.paired = true;
        self.paired_changed(&em).await?;
        Ok(())
    }
    #[zbus(property)]
    fn address(&self) -> String {
        self.address.into()
    }
    #[zbus(property)]
    fn name(&self) -> String {
        self.name.into()
    }
    #[zbus(property)]
    fn alias(&self) -> String {
        self.name.into()
    }
    #[zbus(property)]
    fn icon(&self) -> String {
        self.icon.into()
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        self.paired
    }
    #[zbus(property)]
    fn trusted(&self) -> bool {
        self.trusted
    }
    #[zbus(property)]
    fn set_trusted(&mut self, v: bool) {
        self.log.lock().unwrap().push(format!("Trusted={v} {}", self.address));
        self.trusted = v;
    }
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.connected
    }
    #[zbus(property)]
    fn blocked(&self) -> bool {
        false
    }
    #[zbus(property, name = "RSSI")]
    fn rssi(&self) -> zbus::fdo::Result<i16> {
        self.rssi.ok_or_else(|| zbus::fdo::Error::Failed("no rssi".into()))
    }
}

struct Battery(u8);

#[interface(name = "org.bluez.Battery1")]
impl Battery {
    #[zbus(property)]
    fn percentage(&self) -> u8 {
        self.0
    }
}

struct AgentManager(Log);

#[interface(name = "org.bluez.AgentManager1")]
impl AgentManager {
    fn register_agent(&self, agent: ObjectPath<'_>, capability: &str) {
        self.0.lock().unwrap().push(format!("RegisterAgent {agent} {capability}"));
    }
    fn unregister_agent(&self, agent: ObjectPath<'_>) {
        self.0.lock().unwrap().push(format!("UnregisterAgent {agent}"));
    }
    fn request_default_agent(&self, agent: ObjectPath<'_>) {
        self.0.lock().unwrap().push(format!("RequestDefaultAgent {agent}"));
    }
}

struct Fixture {
    server: Connection,
    log: Log,
    rx: Receiver<BluetoothState>,
    handle: Handle,
}

fn device(log: &Log, address: &'static str, name: &'static str, icon: &'static str, paired: bool, connected: bool, rssi: Option<i16>, flow: &'static str) -> Device {
    Device { log: log.clone(), address, name, icon, paired, trusted: paired, connected, rssi, flow }
}

fn fixture(fake: bool) -> Fixture {
    let log: Log = Log::default();
    let (a, b) = UnixStream::pair().unwrap();
    let (server, client) = smol::block_on(async {
        let server = Builder::async_io_unix_stream(a)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at("/", zbus::fdo::ObjectManager)
            .unwrap()
            .serve_at("/org/bluez", AgentManager(log.clone()))
            .unwrap()
            .serve_at(ADAPTER, Adapter { log: log.clone(), powered: true, discovering: false })
            .unwrap()
            .serve_at(HEADPHONES, device(&log, "11:22:33:44:55:01", "Headphones", "audio-headphones", true, true, None, "confirm"))
            .unwrap()
            .serve_at(HEADPHONES, Battery(80))
            .unwrap()
            .serve_at(KEYBOARD, device(&log, "11:22:33:44:55:02", "Keyboard", "input-keyboard", true, false, None, "confirm"))
            .unwrap()
            .serve_at(MOUSE, device(&log, "11:22:33:44:55:03", "Mouse", "input-mouse", false, false, Some(-60), "confirm"))
            .unwrap()
            .build();
        let client = Builder::async_io_unix_stream(b).p2p().build();
        let (s, c) = smol::future::zip(server, client).await;
        (s.unwrap(), c.unwrap())
    });
    let (handle, rx) = start_on(client, fake);
    Fixture { server, log, rx, handle }
}

fn wait_for(rx: &Receiver<BluetoothState>, what: &str, pred: impl Fn(&BluetoothState) -> bool) -> BluetoothState {
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

fn wait_log(log: &Log, want: &str) {
    for _ in 0..200 {
        if log.lock().unwrap().iter().any(|l| l.contains(want)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("mock never saw {want:?}; log: {:?}", log.lock().unwrap());
}

fn logged(log: &Log, want: &str) -> bool {
    log.lock().unwrap().iter().any(|l| l.contains(want))
}

#[test]
fn reads_adapter_devices_and_battery() {
    let f = fixture(false);
    let s = wait_for(&f.rx, "devices", |s| s.available && s.devices.len() == 3);
    let a = s.adapter.as_ref().unwrap();
    assert_eq!((a.alias.as_str(), a.powered, a.discovering), ("mock-box", true, false));
    // Connected first, then paired, then the stranger.
    let names: Vec<_> = s.devices.iter().map(|d| d.label()).collect();
    assert_eq!(names, vec!["Headphones", "Keyboard", "Mouse"]);
    assert_eq!(s.devices[0].battery, Some(80));
    assert_eq!(s.devices[0].kind, DeviceKind::Headphones);
    assert!(s.devices[0].connected && s.devices[1].paired && !s.devices[2].paired);
    assert_eq!(s.devices[2].rssi, Some(-60));
    assert_eq!(s.tile_subtitle(), "1 device");
    drop(f.server);
}

#[test]
fn power_scan_connect_trust_remove() {
    let f = fixture(false);
    wait_for(&f.rx, "ready", |s| s.devices.len() == 3);

    f.handle.set_powered(false);
    wait_log(&f.log, "Powered=false");
    let s = wait_for(&f.rx, "powered off", |s| !s.powered());
    assert_eq!(s.tile_subtitle(), "off");
    f.handle.set_powered(true);
    wait_for(&f.rx, "powered on", |s| s.powered());

    f.handle.start_discovery();
    let s = wait_for(&f.rx, "scanning", |s| s.scanning() && s.scan_deadline.is_some());
    assert!(s.scan_deadline.unwrap() > std::time::Instant::now());
    f.handle.stop_discovery();
    wait_for(&f.rx, "scan stopped", |s| !s.scanning() && s.scan_deadline.is_none());
    wait_log(&f.log, "StopDiscovery");

    f.handle.connect(KEYBOARD);
    wait_log(&f.log, "Connect 11:22:33:44:55:02");
    let s = wait_for(&f.rx, "keyboard connected", |s| s.device(KEYBOARD).is_some_and(|d| d.connected && d.busy.is_none()));
    assert_eq!(s.connected_count(), 2);
    assert_eq!(s.tile_subtitle(), "2 devices");
    f.handle.disconnect(KEYBOARD);
    wait_for(&f.rx, "keyboard disconnected", |s| s.device(KEYBOARD).is_some_and(|d| !d.connected && d.busy.is_none()));

    f.handle.set_trusted(MOUSE, true);
    wait_log(&f.log, "Trusted=true 11:22:33:44:55:03");
    wait_for(&f.rx, "trusted", |s| s.device(MOUSE).is_some_and(|d| d.trusted));

    f.handle.remove(MOUSE);
    wait_log(&f.log, &format!("RemoveDevice {MOUSE}"));
    wait_for(&f.rx, "mouse gone", |s| s.device(MOUSE).is_none() && s.devices.len() == 2);
    drop(f.server);
}

#[test]
fn pairing_with_a_confirmation_prompt() {
    let f = fixture(false);
    wait_for(&f.rx, "ready", |s| s.devices.len() == 3);

    f.handle.pair(MOUSE);
    let s = wait_for(&f.rx, "prompt", |s| s.pairing.is_some());
    let req = s.pairing.clone().unwrap();
    assert_eq!(req.kind, PairingKind::Confirm { passkey: 123_456 });
    assert_eq!((req.path.as_str(), req.name.as_str(), req.address.as_str()), (MOUSE, "Mouse", "11:22:33:44:55:03"));
    assert_eq!(s.device(MOUSE).unwrap().busy, Some(Busy::Pairing));
    assert!(logged(&f.log, "RegisterAgent /org/ashd/bluez/agent KeyboardDisplay"));

    f.handle.reply_pairing(req.seq, PairingReply::Accept);
    // Paired, then trusted, then connected by the service.
    wait_log(&f.log, "Trusted=true 11:22:33:44:55:03");
    wait_log(&f.log, "Connect 11:22:33:44:55:03");
    let s = wait_for(&f.rx, "paired and connected", |s| s.device(MOUSE).is_some_and(|d| d.paired && d.connected && d.busy.is_none()));
    assert!(s.pairing.is_none());
    wait_log(&f.log, "UnregisterAgent /org/ashd/bluez/agent");
    drop(f.server);
}

#[test]
fn rejected_pairing_reports_an_error() {
    let f = fixture(false);
    wait_for(&f.rx, "ready", |s| s.devices.len() == 3);
    f.handle.pair(MOUSE);
    let req = wait_for(&f.rx, "prompt", |s| s.pairing.is_some()).pairing.unwrap();
    f.handle.reply_pairing(req.seq, PairingReply::Reject);
    let s = wait_for(&f.rx, "error", |s| s.error.is_some() && s.device(MOUSE).is_some_and(|d| d.busy.is_none()));
    assert!(s.error.as_ref().unwrap().starts_with("pair:"));
    assert!(!s.device(MOUSE).unwrap().paired);
    assert!(!logged(&f.log, "Trusted=true 11:22:33:44:55:03"));
    drop(f.server);
}

#[test]
fn just_works_only_for_the_device_the_user_is_pairing() {
    let f = fixture(false);
    wait_for(&f.rx, "ready", |s| s.devices.len() == 3);

    // Stranger asks for authorisation while the user pairs nothing: a prompt, not an accept.
    let agent: zbus::Proxy<'_> = smol::block_on(zbus::Proxy::new(&f.server, "org.bluez", AGENT, "org.bluez.Agent1")).unwrap();
    let who = OwnedObjectPath::try_from(KEYBOARD).unwrap();
    let call = {
        let agent = agent.clone();
        let who = who.clone();
        std::thread::spawn(move || smol::block_on(async move { agent.call::<_, _, ()>("RequestAuthorization", &(&who,)).await }))
    };
    let s = wait_for(&f.rx, "authorize prompt", |s| s.pairing.is_some());
    let req = s.pairing.unwrap();
    assert_eq!(req.kind, PairingKind::Authorize);
    assert_eq!(req.name, "Keyboard");
    f.handle.reply_pairing(req.seq, PairingReply::Reject);
    assert!(call.join().unwrap().is_err(), "rejected request must fail");
    wait_for(&f.rx, "prompt cleared", |s| s.pairing.is_none());

    // The device the user started pairing with is accepted without a prompt.
    // (`Pair` on the mouse takes the `authorize` flow.)
    smol::block_on(async {
        let iface = f.server.object_server().interface::<_, Device>(MOUSE).await.unwrap();
        iface.get_mut().await.flow = "authorize";
    });
    f.handle.pair(MOUSE);
    wait_log(&f.log, "Connect 11:22:33:44:55:03");
    let s = wait_for(&f.rx, "paired without prompt", |s| s.device(MOUSE).is_some_and(|d| d.paired && d.connected));
    assert!(s.pairing.is_none());
    drop(f.server);
}

#[test]
fn nested_mode_logs_instead_of_sending() {
    let f = fixture(true);
    let ready = wait_for(&f.rx, "ready", |s| s.devices.len() == 3);
    f.handle.set_powered(false);
    f.handle.start_discovery();
    f.handle.connect(KEYBOARD);
    f.handle.pair(MOUSE);
    f.handle.remove(HEADPHONES);
    std::thread::sleep(Duration::from_millis(400));
    assert!(f.log.lock().unwrap().is_empty(), "nothing may reach BlueZ: {:?}", f.log.lock().unwrap());
    assert!(ready.powered() && ready.devices.len() == 3);
    // And nothing changed afterwards: no new state was published.
    assert!(f.rx.try_recv().is_err());
    drop(f.server);
}
