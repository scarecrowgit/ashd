//! Battery presence/level from `/sys/class/power_supply` (status-bar chip only; the
//! full UPower service is a later milestone).

use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatteryStatus {
    Charging,
    Discharging,
    Full,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatteryInfo {
    /// 0..=100.
    pub percent: u8,
    pub status: BatteryStatus,
}

/// The first `type == Battery` supply (system batteries only: `scope` != `Device`, so a
/// wireless mouse does not show up as the laptop battery).
pub fn sample_at(root: &Path) -> Option<BatteryInfo> {
    let mut entries: Vec<_> = fs::read_dir(root).ok()?.flatten().map(|e| e.path()).collect();
    entries.sort();
    entries.into_iter().find_map(|dir| {
        let read = |f: &str| fs::read_to_string(dir.join(f)).ok().map(|s| s.trim().to_string());
        if read("type")? != "Battery" || read("scope").as_deref() == Some("Device") {
            return None;
        }
        if read("present").as_deref() == Some("0") {
            return None;
        }
        let percent = read("capacity").and_then(|c| c.parse::<u16>().ok()).map(|c| c.min(100) as u8).or_else(|| {
            let now: f64 = read("energy_now").or_else(|| read("charge_now"))?.parse().ok()?;
            let full: f64 = read("energy_full").or_else(|| read("charge_full"))?.parse().ok()?;
            (full > 0.0).then(|| ((now / full) * 100.0).round().clamp(0.0, 100.0) as u8)
        })?;
        let status = match read("status").as_deref() {
            Some("Charging") => BatteryStatus::Charging,
            Some("Discharging") | Some("Not charging") => BatteryStatus::Discharging,
            Some("Full") => BatteryStatus::Full,
            _ => BatteryStatus::Unknown,
        };
        Some(BatteryInfo { percent, status })
    })
}

pub fn sample() -> Option<BatteryInfo> {
    sample_at(Path::new("/sys/class/power_supply"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_system_battery_only() {
        let dir = std::env::temp_dir().join(format!("ashd-sysinfo-bat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let w = |p: &str, f: &str, v: &str| {
            fs::create_dir_all(dir.join(p)).unwrap();
            fs::write(dir.join(p).join(f), format!("{v}\n")).unwrap();
        };
        w("AC", "type", "Mains");
        w("hidpp_battery_0", "type", "Battery");
        w("hidpp_battery_0", "scope", "Device");
        w("hidpp_battery_0", "capacity", "40");
        assert_eq!(sample_at(&dir), None);
        w("BAT0", "type", "Battery");
        w("BAT0", "capacity", "86");
        w("BAT0", "status", "Discharging");
        assert_eq!(sample_at(&dir), Some(BatteryInfo { percent: 86, status: BatteryStatus::Discharging }));
        let _ = fs::remove_dir_all(&dir);
    }
}
