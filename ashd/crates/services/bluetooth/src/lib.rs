//! BlueZ (`org.bluez`, system bus, zbus 5): adapter, devices, battery and a pairing agent.
//! No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<BluetoothState>)`.
//!
//! * `available` is false when BlueZ is not on the bus or has no adapter; the thread keeps
//!   looking (cheaply, back-off to 30 s) and the UI hides every Bluetooth element meanwhile.
//! * State comes from `GetManagedObjects` plus the `InterfacesAdded/Removed` and
//!   `PropertiesChanged` signals, merged into one cache ([`derive`]).
//! * Discovery started through [`Handle::start_discovery`] stops by itself after
//!   [`SCAN_SECS`] seconds.
//! * **Pairing agent**: an `org.bluez.Agent1` object is served from the start but only
//!   registered with BlueZ while a pairing *the user started* ([`Handle::pair`]) is in
//!   flight. Its requests land in `BluetoothState::pairing` (numeric comparison,
//!   displayed passkey / PIN, PIN / passkey entry); the UI answers with
//!   [`Handle::reply_pairing`]. Authorisation requests ("just works", service
//!   authorisation) are accepted automatically **only** for the device being paired, and
//!   shown as a prompt for any other device. Unanswered prompts are rejected after
//!   [`PROMPT_SECS`].
//!
//! **Switches.** `ASHD_BT_MOCK=<scenario>` serves a canned state (`off`, `on`, `devices`,
//! `scanning`, `pairing`, `pairing-display`, `error`, `unavailable`) and reads nothing.
//! `ASHD_NESTED=1` / `ASHD_FAKE_POWER=1`: mutating commands (power, scan, connect, pair,
//! trust, remove …) are logged (`ashd_svc_bluetooth::fake`), never sent to BlueZ; reading
//! state is unaffected.
//!
//! Testing: `tests/mock_bluez.rs` serves a mock BlueZ (adapter, three devices, pairing
//! flow) over a peer-to-peer connection; the real adapter is never touched by tests.

mod agent;
pub mod derive;
pub mod mock;
mod model;
mod service;

use async_channel::{Receiver, Sender};
pub use model::*;
pub use service::Source;
use zbus::Connection;

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.try_send(cmd);
    }
    pub fn set_powered(&self, on: bool) {
        self.send(Command::SetPowered(on));
    }
    /// Starts discovery; it stops by itself after [`SCAN_SECS`].
    pub fn start_discovery(&self) {
        self.send(Command::StartDiscovery);
    }
    pub fn stop_discovery(&self) {
        self.send(Command::StopDiscovery);
    }
    pub fn connect(&self, path: &str) {
        self.send(Command::Connect { path: path.into() });
    }
    pub fn disconnect(&self, path: &str) {
        self.send(Command::Disconnect { path: path.into() });
    }
    /// Pair with a device (then trust and connect). Starts the agent for this pairing.
    pub fn pair(&self, path: &str) {
        self.send(Command::Pair { path: path.into() });
    }
    pub fn set_trusted(&self, path: &str, trusted: bool) {
        self.send(Command::SetTrusted { path: path.into(), trusted });
    }
    /// Forget the device.
    pub fn remove(&self, path: &str) {
        self.send(Command::Remove { path: path.into() });
    }
    /// Answer `BluetoothState::pairing` (`seq` is the request's `seq`).
    pub fn reply_pairing(&self, seq: u64, reply: PairingReply) {
        self.send(Command::PairingReply { seq, reply });
    }
    pub fn refresh(&self) {
        self.send(Command::Refresh);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// `ASHD_NESTED` or `ASHD_FAKE_POWER` is set to something other than `""` / `0`.
pub fn fake_commands() -> bool {
    ["ASHD_NESTED", "ASHD_FAKE_POWER"].iter().any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
}

pub struct Options {
    pub source: Source,
    pub fake_commands: bool,
    pub mock: Option<String>,
}

impl Options {
    pub fn from_env() -> Self {
        Self { source: Source::System, fake_commands: fake_commands(), mock: std::env::var("ASHD_BT_MOCK").ok().filter(|v| !v.is_empty()) }
    }
}

/// Spawn the `ashd-svc-bluetooth` thread (system bus, looks for BlueZ with back-off).
pub fn start() -> (Handle, Receiver<BluetoothState>) {
    start_with(Options::from_env())
}

/// Like [`start`] over an existing connection (a peer-to-peer one to a mock, in tests).
pub fn start_on(conn: Connection, fake_commands: bool) -> (Handle, Receiver<BluetoothState>) {
    start_with(Options { source: Source::Conn(conn), fake_commands, mock: None })
}

pub fn start_with(opts: Options) -> (Handle, Receiver<BluetoothState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-bluetooth".into())
        .spawn(move || smol::block_on(service::run(rx, state_tx, opts)))
        .expect("spawn ashd-svc-bluetooth");
    (Handle { tx }, state_rx)
}
