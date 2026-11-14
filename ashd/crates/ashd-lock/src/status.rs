//! Footer status items that are cheap to read: the battery, if there is one
//! (`/sys/class/power_supply/*` with `type` = Battery and `scope` ≠ Device).

use std::path::Path;

use ashd_lockui::{StatusIcon, StatusItem};

pub fn read() -> Vec<StatusItem> {
    read_from(Path::new("/sys/class/power_supply"))
}

fn read_from(root: &Path) -> Vec<StatusItem> {
    let Ok(dir) = std::fs::read_dir(root) else { return Vec::new() };
    let mut caps: Vec<(String, u32)> = Vec::new();
    for entry in dir.flatten() {
        let p = entry.path();
        let get = |f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_string()).ok();
        if get("type").as_deref() != Some("Battery") || get("scope").as_deref() == Some("Device") {
            continue;
        }
        if let Some(c) = get("capacity").and_then(|c| c.parse().ok()) {
            caps.push((entry.file_name().to_string_lossy().into_owned(), c));
        }
    }
    caps.sort();
    match caps.first() {
        Some((_, c)) => vec![StatusItem { icon: StatusIcon::Battery, text: format!("{c}%") }],
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_battery_and_skips_peripherals() {
        let dir = std::env::temp_dir().join(format!("ashd-lock-ps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, ty, scope, cap) in [("BAT0", "Battery", None, "86"), ("hidpp_battery_0", "Battery", Some("Device"), "40"), ("AC", "Mains", None, "")] {
            let d = dir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("type"), ty).unwrap();
            if let Some(s) = scope {
                std::fs::write(d.join("scope"), s).unwrap();
            }
            std::fs::write(d.join("capacity"), cap).unwrap();
        }
        let items = read_from(&dir);
        assert_eq!(items, vec![StatusItem { icon: StatusIcon::Battery, text: "86%".into() }]);
        assert!(read_from(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
