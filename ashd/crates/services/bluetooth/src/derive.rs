//! BlueZ's object tree → [`BluetoothState`]. Pure: the service keeps the cache up to date
//! from `GetManagedObjects` and the signals, this turns it into the snapshot.

use std::collections::{BTreeMap, HashMap};

use zbus::zvariant::OwnedValue;

use crate::model::{Adapter, Busy, BluetoothState, Device, DeviceKind};

pub const ADAPTER: &str = "org.bluez.Adapter1";
pub const DEVICE: &str = "org.bluez.Device1";
pub const BATTERY: &str = "org.bluez.Battery1";

pub type Props = HashMap<String, OwnedValue>;
pub type Interfaces = HashMap<String, Props>;
/// Object path → interfaces → properties.
pub type Objects = BTreeMap<String, Interfaces>;

fn text(p: &Props, k: &str) -> Option<String> {
    p.get(k).and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_string))
}

fn flag(p: &Props, k: &str) -> bool {
    p.get(k).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false)
}

/// Apply `PropertiesChanged` (changed values, invalidated names) to the cache.
pub fn apply_changed(objects: &mut Objects, path: &str, iface: &str, changed: Props, invalidated: &[String]) {
    let props = objects.entry(path.to_string()).or_default().entry(iface.to_string()).or_default();
    for (k, v) in changed {
        props.insert(k, v);
    }
    for k in invalidated {
        props.remove(k);
    }
}

pub fn kind_of(icon: Option<&str>, class: Option<u32>) -> DeviceKind {
    match icon {
        Some("audio-headset") => return DeviceKind::Headset,
        Some("audio-headphones") => return DeviceKind::Headphones,
        Some("audio-card" | "audio-speakers") => return DeviceKind::Speaker,
        Some("input-keyboard") => return DeviceKind::Keyboard,
        Some("input-mouse" | "input-tablet-pointing") => return DeviceKind::Mouse,
        Some("input-gaming") => return DeviceKind::Gamepad,
        Some("phone") => return DeviceKind::Phone,
        Some("computer") => return DeviceKind::Computer,
        Some("input-tablet") => return DeviceKind::Tablet,
        _ => {}
    }
    // Class of Device: major class in bits 8..12, minor in 2..8.
    let Some(c) = class else { return DeviceKind::Other };
    match (c >> 8) & 0x1f {
        1 => DeviceKind::Computer,
        2 => DeviceKind::Phone,
        4 => match (c >> 2) & 0x3f {
            1 | 2 => DeviceKind::Headset,
            6 => DeviceKind::Headphones,
            5 | 7 | 8 => DeviceKind::Speaker,
            _ => DeviceKind::Speaker,
        },
        5 => match (c >> 6) & 0x3 {
            1 => DeviceKind::Keyboard,
            2 => DeviceKind::Mouse,
            _ => DeviceKind::Gamepad,
        },
        7 => DeviceKind::Watch,
        _ => DeviceKind::Other,
    }
}

/// The adapter to use: the first one (`hci0`).
pub fn adapter_of(objects: &Objects) -> Option<Adapter> {
    objects.iter().find_map(|(path, ifs)| {
        let p = ifs.get(ADAPTER)?;
        Some(Adapter {
            path: path.clone(),
            address: text(p, "Address").unwrap_or_default(),
            alias: text(p, "Alias").or_else(|| text(p, "Name")).unwrap_or_default(),
            powered: flag(p, "Powered"),
            discovering: flag(p, "Discovering"),
            discoverable: flag(p, "Discoverable"),
            pairable: flag(p, "Pairable"),
        })
    })
}

pub fn devices_of(objects: &Objects, adapter_path: &str, busy: &HashMap<String, Busy>) -> Vec<Device> {
    let prefix = format!("{adapter_path}/");
    let mut v: Vec<Device> = objects
        .iter()
        .filter(|(path, _)| path.starts_with(&prefix))
        .filter_map(|(path, ifs)| {
            let p = ifs.get(DEVICE)?;
            let icon = text(p, "Icon");
            let class = p.get("Class").and_then(|v| v.downcast_ref::<u32>().ok());
            let battery = ifs.get(BATTERY).and_then(|b| b.get("Percentage")).and_then(|v| v.downcast_ref::<u8>().ok());
            let name = text(p, "Name").unwrap_or_default();
            Some(Device {
                path: path.clone(),
                address: text(p, "Address").unwrap_or_default(),
                alias: text(p, "Alias").unwrap_or_else(|| name.clone()),
                name,
                kind: kind_of(icon.as_deref(), class),
                icon,
                class,
                paired: flag(p, "Paired"),
                trusted: flag(p, "Trusted"),
                connected: flag(p, "Connected"),
                blocked: flag(p, "Blocked"),
                rssi: p.get("RSSI").and_then(|v| v.downcast_ref::<i16>().ok()),
                battery,
                busy: busy.get(path).copied(),
            })
        })
        .collect();
    v.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            // Stronger signal first; no RSSI last.
            .then_with(|| b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN)))
            .then_with(|| a.label().to_lowercase().cmp(&b.label().to_lowercase()))
    });
    v
}

/// `available` = an adapter exists.
pub fn state_of(objects: &Objects, busy: &HashMap<String, Busy>) -> BluetoothState {
    let adapter = adapter_of(objects);
    let devices = adapter.as_ref().map(|a| devices_of(objects, &a.path, busy)).unwrap_or_default();
    BluetoothState { available: adapter.is_some(), adapter, devices, ..Default::default() }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use zbus::zvariant::Value;

    use super::*;

    pub fn v(x: impl Into<Value<'static>>) -> OwnedValue {
        x.into().try_to_owned().expect("owned value")
    }

    pub fn props(items: Vec<(&str, OwnedValue)>) -> Props {
        items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    pub fn adapter_objects() -> Objects {
        let mut o = Objects::new();
        o.insert(
            "/org/bluez/hci0".into(),
            HashMap::from([(
                ADAPTER.to_string(),
                props(vec![
                    ("Address", v("AA:BB:CC:00:00:01")),
                    ("Alias", v("ashd-box")),
                    ("Powered", v(true)),
                    ("Discovering", v(false)),
                    ("Discoverable", v(false)),
                    ("Pairable", v(true)),
                ]),
            )]),
        );
        o
    }

    pub fn add_device(o: &mut Objects, mac: &str, name: &str, icon: &str, paired: bool, connected: bool, rssi: Option<i16>, battery: Option<u8>) {
        let path = format!("/org/bluez/hci0/dev_{}", mac.replace(':', "_"));
        let mut ifs: Interfaces = HashMap::new();
        let mut p = props(vec![
            ("Address", v(mac.to_string())),
            ("Name", v(name.to_string())),
            ("Alias", v(name.to_string())),
            ("Icon", v(icon.to_string())),
            ("Paired", v(paired)),
            ("Trusted", v(paired)),
            ("Connected", v(connected)),
            ("Blocked", v(false)),
        ]);
        if let Some(r) = rssi {
            p.insert("RSSI".into(), v(r));
        }
        ifs.insert(DEVICE.into(), p);
        if let Some(b) = battery {
            ifs.insert(BATTERY.into(), props(vec![("Percentage", v(b))]));
        }
        o.insert(path, ifs);
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::*, *};

    #[test]
    fn state_from_objects_sorts_and_reads_battery() {
        let mut o = adapter_objects();
        add_device(&mut o, "11:22:33:44:55:01", "WH-1000XM5", "audio-headphones", true, true, None, Some(82));
        add_device(&mut o, "11:22:33:44:55:02", "Keychron K3", "input-keyboard", true, false, None, None);
        add_device(&mut o, "11:22:33:44:55:03", "MX Anywhere", "input-mouse", false, false, Some(-60), None);
        add_device(&mut o, "11:22:33:44:55:04", "Far thing", "", false, false, Some(-90), None);
        let busy = HashMap::from([("/org/bluez/hci0/dev_11_22_33_44_55_03".to_string(), Busy::Pairing)]);
        let s = state_of(&o, &busy);
        assert!(s.available && s.powered() && !s.scanning());
        assert_eq!(s.adapter.as_ref().unwrap().alias, "ashd-box");
        let names: Vec<_> = s.devices.iter().map(|d| d.label()).collect();
        assert_eq!(names, vec!["WH-1000XM5", "Keychron K3", "MX Anywhere", "Far thing"]);
        assert_eq!(s.devices[0].battery, Some(82));
        assert_eq!(s.devices[0].kind, DeviceKind::Headphones);
        assert_eq!(s.devices[2].busy, Some(Busy::Pairing));
        assert_eq!(s.connected_count(), 1);
        assert_eq!(s.tile_subtitle(), "1 device");
    }

    #[test]
    fn property_changes_update_the_cache() {
        let mut o = adapter_objects();
        apply_changed(&mut o, "/org/bluez/hci0", ADAPTER, props(vec![("Powered", v(false))]), &[]);
        let s = state_of(&o, &HashMap::new());
        assert!(!s.powered());
        assert_eq!(s.tile_subtitle(), "off");
        apply_changed(&mut o, "/org/bluez/hci0", ADAPTER, Props::new(), &["Powered".to_string()]);
        assert!(!state_of(&o, &HashMap::new()).powered());
    }

    #[test]
    fn no_adapter_is_unavailable() {
        let s = state_of(&Objects::new(), &HashMap::new());
        assert!(!s.available);
        assert!(s.devices.is_empty());
    }

    #[test]
    fn kinds_from_class_of_device() {
        assert_eq!(kind_of(None, Some(0x240404)), DeviceKind::Headset);
        assert_eq!(kind_of(None, Some(0x240418)), DeviceKind::Headphones);
        assert_eq!(kind_of(None, Some(0x002540)), DeviceKind::Keyboard);
        assert_eq!(kind_of(None, Some(0x002580)), DeviceKind::Mouse);
        assert_eq!(kind_of(None, Some(0x5a020c)), DeviceKind::Phone);
        assert_eq!(kind_of(None, None), DeviceKind::Other);
        assert_eq!(kind_of(Some("input-gaming"), None), DeviceKind::Gamepad);
    }
}
