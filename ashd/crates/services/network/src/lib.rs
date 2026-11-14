//! Network state for the shell (status bar, quick settings). No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<NetworkState>)`.
//! Two backends, chosen at run time:
//!
//! * **Read-only** (default when NetworkManager is not on the system bus): interfaces,
//!   addresses, gateway, DNS, link speed / duplex / MTU, carrier time, VPN tunnels,
//!   throughput and daily totals, all from `/sys/class/net`, `/proc/net/route`,
//!   `/etc/resolv.conf` and `getifaddrs`. It never sends a packet: "limited" is derived
//!   from the kernel's own facts (no address, only 169.254/16, no default route).
//!   `can_configure` is false and commands that would change something are refused.
//! * **NetworkManager** (`org.freedesktop.NetworkManager` has an owner): the same state
//!   plus profiles, Wi-Fi access points / scan and the commands (connect, disconnect,
//!   activate a profile, static profile with a 15 s safe-apply, Wi-Fi connect with a
//!   password handed to NM in a `Zeroizing` buffer and never logged). `can_configure`.
//!
//! Throughput is sampled every 500 ms only while a consumer called
//! `Handle::set_sampling(true)` (the open Ethernet section); otherwise a 5 s poll feeds
//! the status bar. Daily totals and the carrier-up time are kept in
//! `$ASHD_STATE_DIR/network.json`.
//!
//! **Environment switches**
//!
//! * `ASHD_NET_MOCK=<scenario>` — serve a canned state instead of reading the machine, so
//!   the UI can be screenshotted in each state: `connected`, `connecting`, `limited`,
//!   `unplugged`, `offline`, `vpn`, `nm` (NetworkManager-style with profiles and Wi-Fi),
//!   `wifi`, `wifi-password` (connecting with the inline password field), `apply` (the
//!   "Keep these network settings?" banner), `empty`. With a mock nothing is read.
//! * `ASHD_NESTED=1` / `ASHD_FAKE_POWER=1` — commands are *logged*
//!   (`ashd_svc_network::fake`), never sent to NetworkManager.
//!
//! Testing: `tests/mock_nm.rs` runs the NetworkManager backend against a mock NM served
//! over a private peer-to-peer connection; nothing touches the real system bus.

pub mod barview;
mod engine;
pub mod fmt;
mod flap;
pub mod mock;
mod model;
pub mod nm;
mod service;
pub mod sysfs;
mod usage;

use std::path::PathBuf;

use async_channel::{Receiver, Sender};
pub use model::*;
pub use service::NmMode;
pub use sysfs::{Addrs, Paths};
pub use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.try_send(cmd);
    }
    /// 500 ms throughput samples + history while `on` (the Ethernet section is open).
    pub fn set_sampling(&self, on: bool) {
        self.send(Command::SetSampling(on));
    }
    pub fn refresh(&self) {
        self.send(Command::Refresh);
    }
    pub fn connect_device(&self, ifname: &str) {
        self.send(Command::ConnectDevice { ifname: ifname.into() });
    }
    pub fn disconnect_device(&self, ifname: &str) {
        self.send(Command::DisconnectDevice { ifname: ifname.into() });
    }
    pub fn activate_profile(&self, uuid: &str) {
        self.send(Command::ActivateProfile { uuid: uuid.into() });
    }
    pub fn reapply(&self, ifname: &str) {
        self.send(Command::Reapply { ifname: ifname.into() });
    }
    pub fn apply_static(&self, cfg: StaticConfig) {
        self.send(Command::ApplyStatic(cfg));
    }
    pub fn keep_apply(&self) {
        self.send(Command::KeepApply);
    }
    pub fn revert_apply(&self) {
        self.send(Command::RevertApply);
    }
    pub fn forget_profile(&self, uuid: &str) {
        self.send(Command::ForgetProfile { uuid: uuid.into() });
    }
    pub fn wifi_set_enabled(&self, on: bool) {
        self.send(Command::WifiSetEnabled(on));
    }
    pub fn wifi_scan(&self) {
        self.send(Command::WifiScan);
    }
    /// `password` is moved into the command and zeroed when the service is done with it.
    pub fn wifi_connect(&self, ssid: &str, password: Option<Zeroizing<String>>) {
        self.send(Command::WifiConnect { ssid: ssid.into(), password });
    }
    pub fn wifi_disconnect(&self) {
        self.send(Command::WifiDisconnect);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// `ASHD_NESTED` or `ASHD_FAKE_POWER` is set to something other than `""` / `0`: commands
/// are logged instead of sent to NetworkManager.
pub fn fake_commands() -> bool {
    ["ASHD_NESTED", "ASHD_FAKE_POWER"].iter().any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
}

/// `$ASHD_STATE_DIR` or `$XDG_STATE_HOME/ashd` or `~/.local/state/ashd` (same rule as
/// `ashd_tokens::state::state_dir`).
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ASHD_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::state_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/state"))).unwrap_or_else(|| PathBuf::from("/tmp")).join("ashd")
}

/// How to run the service (the environment decides for [`start`]).
pub struct Options {
    pub paths: Paths,
    /// `None` = `getifaddrs`.
    pub addrs: Option<Box<dyn Fn() -> Addrs + Send>>,
    /// Where daily totals / carrier times are persisted; `None` = nowhere.
    pub usage_file: Option<PathBuf>,
    pub nm: NmMode,
    pub fake_commands: bool,
    /// Canned scenario (see the crate docs); nothing is read from the machine.
    pub mock: Option<String>,
    /// How long the "Keep these network settings?" banner waits before reverting.
    pub safe_apply: std::time::Duration,
}

impl Options {
    pub fn from_env() -> Self {
        Self {
            paths: Paths::system(),
            addrs: None,
            usage_file: Some(state_dir().join("network.json")),
            nm: NmMode::Auto,
            fake_commands: fake_commands(),
            mock: std::env::var("ASHD_NET_MOCK").ok().filter(|v| !v.is_empty()),
            safe_apply: std::time::Duration::from_secs(SAFE_APPLY_SECS),
        }
    }
}

/// Spawn the `ashd-svc-network` thread.
pub fn start() -> (Handle, Receiver<NetworkState>) {
    start_with(Options::from_env())
}

pub fn start_with(opts: Options) -> (Handle, Receiver<NetworkState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-network".into())
        .spawn(move || smol::block_on(service::run(rx, state_tx, opts)))
        .expect("spawn ashd-svc-network");
    (Handle { tx }, state_rx)
}

/// Read the machine once, synchronously, with the read-only backend (the `dump` example).
pub fn sample_once(paths: Paths) -> NetworkState {
    let mut core = engine::Core::new(paths, Box::new(sysfs::system_addrs), None);
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    core.tick(&engine::Tick { now: std::time::Instant::now(), date: &date, unix, sampling: false })
}
