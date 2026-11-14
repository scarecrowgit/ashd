//! The plain data the UI needs.

use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adapter {
    /// D-Bus object path (`/org/bluez/hci0`).
    pub path: String,
    pub address: String,
    pub alias: String,
    pub powered: bool,
    pub discovering: bool,
    pub discoverable: bool,
    pub pairable: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceKind {
    Headphones,
    Headset,
    Speaker,
    Keyboard,
    Mouse,
    Gamepad,
    Phone,
    Computer,
    Watch,
    Tablet,
    #[default]
    Other,
}

/// What the service is doing to a device right now (spinner row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Connecting,
    Disconnecting,
    Pairing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// D-Bus object path, the key of every device command.
    pub path: String,
    pub address: String,
    /// `Name` (the device's own).
    pub name: String,
    /// `Alias` (the user's label, defaults to the name).
    pub alias: String,
    /// BlueZ icon name (`audio-headset`, `input-keyboard`, …).
    pub icon: Option<String>,
    pub class: Option<u32>,
    pub kind: DeviceKind,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub blocked: bool,
    pub rssi: Option<i16>,
    /// From `org.bluez.Battery1`.
    pub battery: Option<u8>,
    pub busy: Option<Busy>,
}

impl Device {
    /// What to show: the alias, else the name, else the address.
    pub fn label(&self) -> &str {
        [self.alias.as_str(), self.name.as_str(), self.address.as_str()].into_iter().find(|s| !s.is_empty()).unwrap_or("")
    }
}

/// A request from the pairing agent that needs the user (or only shows a code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingKind {
    /// Numeric comparison: "does the other device show 482913?" → accept / reject.
    Confirm { passkey: u32 },
    /// Type this on the other device; `entered` digits so far.
    DisplayPasskey { passkey: u32, entered: u16 },
    DisplayPin { pin: String },
    RequestPin,
    RequestPasskey,
    /// A device that is not the one the user started pairing with asks to pair / connect.
    Authorize,
    AuthorizeService { uuid: String },
}

impl PairingKind {
    /// The request waits for an answer (as opposed to just showing a code).
    pub fn needs_answer(&self) -> bool {
        !matches!(self, Self::DisplayPasskey { .. } | Self::DisplayPin { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingRequest {
    /// Increments per request; answers must carry it.
    pub seq: u64,
    pub path: String,
    pub address: String,
    pub name: String,
    pub kind: PairingKind,
}

/// The user's answer to a [`PairingRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingReply {
    Accept,
    Reject,
    Pin(String),
    Passkey(u32),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BluetoothState {
    /// BlueZ is on the bus and has an adapter. When false the UI hides everything.
    pub available: bool,
    pub adapter: Option<Adapter>,
    /// Connected first, then paired, then by signal strength.
    pub devices: Vec<Device>,
    pub pairing: Option<PairingRequest>,
    /// Last command failure ("Failed to connect: Page Timeout").
    pub error: Option<String>,
    /// Discovery stops by itself at this instant ([`SCAN_SECS`] after it started).
    pub scan_deadline: Option<Instant>,
}

impl BluetoothState {
    pub fn powered(&self) -> bool {
        self.adapter.as_ref().is_some_and(|a| a.powered)
    }
    pub fn scanning(&self) -> bool {
        self.adapter.as_ref().is_some_and(|a| a.discovering)
    }
    pub fn connected_count(&self) -> usize {
        self.devices.iter().filter(|d| d.connected).count()
    }
    /// The tile's second line: `off`, `on`, `2 devices`.
    pub fn tile_subtitle(&self) -> String {
        if !self.powered() {
            return "off".into();
        }
        match self.connected_count() {
            0 => "on".into(),
            1 => "1 device".into(),
            n => format!("{n} devices"),
        }
    }
    pub fn device(&self, path: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.path == path)
    }
}

/// Discovery is stopped after this many seconds.
pub const SCAN_SECS: u64 = 30;
/// A pairing prompt nobody answers is rejected after this many seconds.
pub const PROMPT_SECS: u64 = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    SetPowered(bool),
    StartDiscovery,
    StopDiscovery,
    Connect { path: String },
    Disconnect { path: String },
    /// Pair, then trust and connect. The user started this: "just works" is accepted.
    Pair { path: String },
    SetTrusted { path: String, trusted: bool },
    /// Forget the device (remove the pairing).
    Remove { path: String },
    PairingReply { seq: u64, reply: PairingReply },
    Refresh,
}
