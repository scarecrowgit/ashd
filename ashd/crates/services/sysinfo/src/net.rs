//! Read-only network summary for the status bar (M1): the interface of the default
//! route, wired or wireless, its operstate, carrier and link speed — straight from
//! `/proc/net/route` and `/sys/class/net/<if>/`. No NetworkManager, no netlink: a few
//! tiny file reads per sample.

use std::{fs, path::Path};

/// Wired or wireless (by `/sys/class/net/<if>/wireless` or `phy80211`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkKind {
    #[default]
    Wired,
    Wifi,
}

/// The states of the det-12 status-bar indicator that can be derived without a
/// connection manager.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetState {
    /// Nothing sampled yet / no physical interface at all.
    #[default]
    Unknown,
    /// Default route over an interface whose link is up.
    Connected,
    /// Link up but no default route ("no internet": warn dot).
    NoInternet,
    /// A physical interface exists but has no carrier (cable unplugged / not associated).
    NoCarrier,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkSummary {
    pub state: NetState,
    /// Interface the summary is about (`enp5s0`, `wlan0`), if any.
    pub interface: Option<String>,
    pub kind: LinkKind,
    /// `/sys/class/net/<if>/operstate` (`up`, `down`, `dormant`, …).
    pub operstate: String,
    /// Link speed in Mbit/s (wired only; wifi drivers report -1 / nothing).
    pub speed_mbps: Option<u32>,
}

impl NetworkSummary {
    /// Short speed label as on the mockup: `1G`, `2.5G`, `100M`.
    pub fn speed_label(&self) -> Option<String> {
        let s = self.speed_mbps?;
        Some(if s >= 1000 {
            let g = s as f32 / 1000.0;
            if (g - g.round()).abs() < 0.05 { format!("{}G", g.round() as u32) } else { format!("{g:.1}G") }
        } else {
            format!("{s}M")
        })
    }
}

/// Interface of the default IPv4 route with the lowest metric, from `/proc/net/route`
/// content (header line + whitespace-separated columns).
pub fn default_route_interface(route_table: &str) -> Option<String> {
    route_table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Iface Destination Gateway Flags RefCnt Use Metric Mask …
            if cols.len() < 8 || cols[1] != "00000000" || cols[7] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(cols[3], 16).ok()?;
            // RTF_UP
            if flags & 0x1 == 0 {
                return None;
            }
            let metric: u32 = cols[6].parse().ok()?;
            Some((metric, cols[0].to_string()))
        })
        .min_by_key(|(m, _)| *m)
        .map(|(_, iface)| iface)
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Interface facts from `<sys_net>/<iface>/`.
fn link(sys_net: &Path, iface: &str) -> (LinkKind, String, Option<bool>, Option<u32>) {
    let dir = sys_net.join(iface);
    let kind = if dir.join("wireless").exists() || dir.join("phy80211").exists() { LinkKind::Wifi } else { LinkKind::Wired };
    let operstate = read_trim(&dir.join("operstate")).unwrap_or_default();
    // Reading `carrier`/`speed` of a down interface fails with EINVAL: treat as unknown.
    let carrier = read_trim(&dir.join("carrier")).and_then(|c| c.parse::<u8>().ok()).map(|c| c == 1);
    let speed = read_trim(&dir.join("speed")).and_then(|s| s.parse::<i64>().ok()).filter(|s| *s > 0).map(|s| s as u32);
    (kind, operstate, carrier, speed)
}

/// Physical interfaces (those with a `device` link), sorted, wired first.
fn physical_interfaces(sys_net: &Path) -> Vec<String> {
    let mut v: Vec<(bool, String)> = fs::read_dir(sys_net)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("device").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .map(|name| {
            let wifi = sys_net.join(&name).join("wireless").exists() || sys_net.join(&name).join("phy80211").exists();
            (wifi, name)
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

/// Sample the network summary. `proc_route` = `/proc/net/route`, `sys_net` =
/// `/sys/class/net` (parameters for tests).
pub fn sample_at(proc_route: &Path, sys_net: &Path) -> NetworkSummary {
    let route = fs::read_to_string(proc_route).unwrap_or_default();
    if let Some(iface) = default_route_interface(&route) {
        let (kind, operstate, carrier, speed) = link(sys_net, &iface);
        let up = carrier != Some(false) && operstate != "down";
        return NetworkSummary {
            state: if up { NetState::Connected } else { NetState::NoCarrier },
            interface: Some(iface),
            kind,
            operstate,
            speed_mbps: if kind == LinkKind::Wired { speed } else { None },
        };
    }
    // No default route: describe the first physical interface (wired first): link up →
    // "no internet", otherwise "no carrier".
    let ifaces = physical_interfaces(sys_net);
    let facts: Vec<_> = ifaces.iter().map(|i| (i, link(sys_net, i))).collect();
    let pick = facts
        .iter()
        .find(|(_, (_, op, carrier, _))| *carrier == Some(true) && op == "up")
        .or_else(|| facts.first());
    match pick {
        Some((iface, (kind, operstate, carrier, speed))) => {
            let up = *carrier == Some(true) && operstate == "up";
            NetworkSummary {
                state: if up { NetState::NoInternet } else { NetState::NoCarrier },
                interface: Some((*iface).clone()),
                kind: *kind,
                operstate: operstate.clone(),
                speed_mbps: if up && *kind == LinkKind::Wired { *speed } else { None },
            }
        }
        None => NetworkSummary::default(),
    }
}

pub fn sample() -> NetworkSummary {
    sample_at(Path::new("/proc/net/route"), Path::new("/sys/class/net"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
enp5s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
enp5s0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn default_route_picks_lowest_metric() {
        assert_eq!(default_route_interface(ROUTE).as_deref(), Some("enp5s0"));
        assert_eq!(default_route_interface("Iface\tDestination\n"), None);
    }

    fn fake_iface(root: &Path, name: &str, oper: &str, carrier: &str, speed: &str, wifi: bool) {
        let d = root.join(name);
        fs::create_dir_all(d.join("device")).unwrap();
        if wifi {
            fs::create_dir_all(d.join("wireless")).unwrap();
        }
        fs::write(d.join("operstate"), format!("{oper}\n")).unwrap();
        fs::write(d.join("carrier"), format!("{carrier}\n")).unwrap();
        fs::write(d.join("speed"), format!("{speed}\n")).unwrap();
    }

    #[test]
    fn states() {
        let dir = std::env::temp_dir().join(format!("ashd-sysinfo-net-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let sys = dir.join("net");
        fake_iface(&sys, "enp5s0", "up", "1", "1000", false);
        fake_iface(&sys, "wlan0", "up", "1", "-1", true);
        let route = dir.join("route");
        fs::write(&route, ROUTE).unwrap();

        let s = sample_at(&route, &sys);
        assert_eq!(s.state, NetState::Connected);
        assert_eq!(s.kind, LinkKind::Wired);
        assert_eq!(s.speed_label().as_deref(), Some("1G"));

        // Unplugged: no route, carrier 0 on the wired interface, wifi down.
        fs::write(&route, "Iface\tDestination\n").unwrap();
        fake_iface(&sys, "enp5s0", "down", "0", "-1", false);
        fake_iface(&sys, "wlan0", "down", "0", "-1", true);
        let s = sample_at(&route, &sys);
        assert_eq!(s.state, NetState::NoCarrier);
        assert_eq!(s.interface.as_deref(), Some("enp5s0"));

        // Link up, no route: no internet.
        fake_iface(&sys, "enp5s0", "up", "1", "2500", false);
        let s = sample_at(&route, &sys);
        assert_eq!(s.state, NetState::NoInternet);
        assert_eq!(s.speed_label().as_deref(), Some("2.5G"));
        let _ = fs::remove_dir_all(&dir);
    }
}
