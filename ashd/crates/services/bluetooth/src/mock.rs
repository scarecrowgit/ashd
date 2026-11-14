//! `ASHD_BT_MOCK=<scenario>`: canned states for screenshots and UI development. Nothing is
//! read from the machine and no D-Bus is touched; commands change the canned state in the
//! obvious way, so the whole panel (scan, connect, pairing prompt) can be clicked through.

use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use smol::future::or;

use crate::model::*;

pub const SCENARIOS: &[&str] = &["off", "on", "devices", "scanning", "pairing", "pairing-display", "error", "unavailable"];

fn dev(n: u8, name: &str, kind: DeviceKind, icon: &str, paired: bool, connected: bool, rssi: Option<i16>, battery: Option<u8>) -> Device {
    let address = format!("11:22:33:44:55:{n:02X}");
    Device {
        path: format!("/org/bluez/hci0/dev_{}", address.replace(':', "_")),
        address,
        name: name.into(),
        alias: name.into(),
        icon: Some(icon.into()),
        class: None,
        kind,
        paired,
        trusted: paired,
        connected,
        blocked: false,
        rssi,
        battery,
        busy: None,
    }
}

fn adapter(powered: bool, discovering: bool) -> Adapter {
    Adapter {
        path: "/org/bluez/hci0".into(),
        address: "AA:BB:CC:00:00:01".into(),
        alias: "ashd-box".into(),
        powered,
        discovering,
        discoverable: false,
        pairable: true,
    }
}

fn devices() -> Vec<Device> {
    vec![
        dev(1, "WH-1000XM5", DeviceKind::Headphones, "audio-headphones", true, true, None, Some(82)),
        dev(2, "Keychron K3", DeviceKind::Keyboard, "input-keyboard", true, true, None, Some(64)),
        dev(3, "Pixel 8", DeviceKind::Phone, "phone", true, false, None, None),
        dev(4, "MX Anywhere 3", DeviceKind::Mouse, "input-mouse", false, false, Some(-58), None),
    ]
}

pub fn scenario(name: &str) -> Option<BluetoothState> {
    let mut s = BluetoothState { available: true, adapter: Some(adapter(true, false)), ..Default::default() };
    match name {
        "off" => {
            s.adapter = Some(adapter(false, false));
            s.devices = devices().into_iter().filter(|d| d.paired).map(|d| Device { connected: false, battery: None, ..d }).collect();
        }
        "on" => {}
        "devices" => s.devices = devices(),
        "scanning" => {
            s.adapter = Some(adapter(true, true));
            s.devices = devices();
            s.devices.push(dev(5, "JBL Flip 6", DeviceKind::Speaker, "audio-card", false, false, Some(-71), None));
            s.scan_deadline = Some(Instant::now() + Duration::from_secs(SCAN_SECS));
        }
        "pairing" | "pairing-display" => {
            s.adapter = Some(adapter(true, true));
            s.devices = devices();
            let target = s.devices.iter_mut().find(|d| !d.paired).expect("an unpaired device");
            target.busy = Some(Busy::Pairing);
            let kind = if name == "pairing" { PairingKind::Confirm { passkey: 482_913 } } else { PairingKind::DisplayPasskey { passkey: 118_204, entered: 3 } };
            s.pairing = Some(PairingRequest { seq: 1, path: target.path.clone(), address: target.address.clone(), name: target.alias.clone(), kind });
        }
        "error" => {
            s.devices = devices();
            s.error = Some("connect: Page Timeout".into());
        }
        "unavailable" => s = BluetoothState::default(),
        _ => return None,
    }
    Some(s)
}

pub(crate) async fn run(name: &str, commands: Receiver<Command>, state: Sender<BluetoothState>) {
    let Some(mut s) = scenario(name) else {
        tracing::warn!("ASHD_BT_MOCK: unknown scenario {name:?}; known: {SCENARIOS:?}");
        let _ = state.force_send(BluetoothState::default());
        while commands.recv().await.is_ok() {}
        return;
    };
    let mut ops: Vec<(Instant, String, Busy)> = Vec::new();
    let _ = state.force_send(s.clone());
    loop {
        let wake = ops.iter().map(|(t, _, _)| *t).chain(s.scan_deadline).min().unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        let ev = or(async { Some(commands.recv().await) }, async {
            smol::Timer::at(wake).await;
            None
        })
        .await;
        match ev {
            Some(Err(_)) => return,
            Some(Ok(cmd)) => {
                tracing::info!(target: "ashd_svc_bluetooth::mock", "{cmd:?}");
                apply(&mut s, cmd, &mut ops);
            }
            None => {}
        }
        let now = Instant::now();
        if s.scan_deadline.is_some_and(|t| now >= t) {
            s.scan_deadline = None;
            if let Some(a) = s.adapter.as_mut() {
                a.discovering = false;
            }
        }
        let (due, rest): (Vec<_>, Vec<_>) = ops.drain(..).partition(|(t, _, _)| now >= *t);
        ops = rest;
        for (_, path, busy) in due {
            if let Some(d) = s.devices.iter_mut().find(|d| d.path == path) {
                d.busy = None;
                match busy {
                    Busy::Connecting => d.connected = true,
                    Busy::Disconnecting => d.connected = false,
                    Busy::Pairing => {}
                }
            }
        }
        let _ = state.force_send(s.clone());
    }
}

fn apply(s: &mut BluetoothState, cmd: Command, ops: &mut Vec<(Instant, String, Busy)>) {
    let later = |ms: u64| Instant::now() + Duration::from_millis(ms);
    match cmd {
        Command::SetPowered(on) => {
            if let Some(a) = s.adapter.as_mut() {
                a.powered = on;
                if !on {
                    a.discovering = false;
                    s.scan_deadline = None;
                    for d in &mut s.devices {
                        d.connected = false;
                        d.battery = None;
                    }
                }
            }
        }
        Command::StartDiscovery => {
            if let Some(a) = s.adapter.as_mut() {
                a.discovering = true;
                s.scan_deadline = Some(Instant::now() + Duration::from_secs(SCAN_SECS));
            }
        }
        Command::StopDiscovery => {
            if let Some(a) = s.adapter.as_mut() {
                a.discovering = false;
            }
            s.scan_deadline = None;
        }
        Command::Connect { path } => {
            if let Some(d) = s.devices.iter_mut().find(|d| d.path == path) {
                d.busy = Some(Busy::Connecting);
                ops.push((later(1200), path, Busy::Connecting));
            }
        }
        Command::Disconnect { path } => {
            if let Some(d) = s.devices.iter_mut().find(|d| d.path == path) {
                d.busy = Some(Busy::Disconnecting);
                ops.push((later(500), path, Busy::Disconnecting));
            }
        }
        Command::Pair { path } => {
            if let Some(d) = s.devices.iter_mut().find(|d| d.path == path) {
                d.busy = Some(Busy::Pairing);
                s.pairing = Some(PairingRequest {
                    seq: 1,
                    path: path.clone(),
                    address: d.address.clone(),
                    name: d.alias.clone(),
                    kind: PairingKind::Confirm { passkey: 482_913 },
                });
            }
        }
        Command::PairingReply { reply, .. } => {
            if let Some(req) = s.pairing.take()
                && let Some(d) = s.devices.iter_mut().find(|d| d.path == req.path)
            {
                d.busy = None;
                if reply == PairingReply::Accept {
                    d.paired = true;
                    d.trusted = true;
                    d.busy = Some(Busy::Connecting);
                    ops.push((later(1000), req.path, Busy::Connecting));
                }
            }
        }
        Command::SetTrusted { path, trusted } => {
            if let Some(d) = s.devices.iter_mut().find(|d| d.path == path) {
                d.trusted = trusted;
            }
        }
        Command::Remove { path } => s.devices.retain(|d| d.path != path),
        Command::Refresh => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_exist() {
        for n in SCENARIOS {
            assert!(scenario(n).is_some(), "{n}");
        }
        assert!(scenario("nope").is_none());
        assert_eq!(scenario("off").unwrap().tile_subtitle(), "off");
        assert_eq!(scenario("on").unwrap().tile_subtitle(), "on");
        assert_eq!(scenario("devices").unwrap().tile_subtitle(), "2 devices");
        assert!(!scenario("unavailable").unwrap().available);
        assert!(scenario("pairing").unwrap().pairing.is_some());
    }

    #[test]
    fn pairing_flow() {
        let mut s = scenario("devices").unwrap();
        let mut ops = Vec::new();
        let path = s.devices.iter().find(|d| !d.paired).unwrap().path.clone();
        apply(&mut s, Command::Pair { path: path.clone() }, &mut ops);
        let seq = s.pairing.as_ref().unwrap().seq;
        apply(&mut s, Command::PairingReply { seq, reply: PairingReply::Accept }, &mut ops);
        assert!(s.pairing.is_none());
        assert!(s.device(&path).unwrap().paired);
        assert_eq!(ops.len(), 1);
    }
}
