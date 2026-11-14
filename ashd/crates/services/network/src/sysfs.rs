//! Read-only facts from `/sys/class/net`, `/proc/net/route`, `/etc/resolv.conf` and
//! `getifaddrs`. Nothing here sends a packet or changes anything: the kernel already knows.

use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

use crate::model::{Duplex, IfKind, Interface, IpNet, OperState, Vpn, VpnKind};

/// Where to read from (the real files, or a fixture directory in tests).
#[derive(Clone, Debug)]
pub struct Paths {
    pub sys_net: PathBuf,
    pub proc_route: PathBuf,
    pub resolv_conf: PathBuf,
    /// systemd-resolved's list of real upstream servers (used when `resolv_conf` only
    /// names the 127.0.0.53 stub).
    pub resolved_conf: PathBuf,
    pub proc_uptime: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        Self {
            sys_net: "/sys/class/net".into(),
            proc_route: "/proc/net/route".into(),
            resolv_conf: "/etc/resolv.conf".into(),
            resolved_conf: "/run/systemd/resolve/resolv.conf".into(),
            proc_uptime: "/proc/uptime".into(),
        }
    }
    /// Everything under one fixture root (tests).
    pub fn under(root: &Path) -> Self {
        Self {
            sys_net: root.join("sys/class/net"),
            proc_route: root.join("proc/net/route"),
            resolv_conf: root.join("etc/resolv.conf"),
            resolved_conf: root.join("run/systemd/resolve/resolv.conf"),
            proc_uptime: root.join("proc/uptime"),
        }
    }
}

/// `(interface, address, prefix length)` for every address of every interface.
pub type Addrs = Vec<(String, IpAddr, u8)>;

/// Local addresses through `getifaddrs(3)` — a netlink dump inside libc, no traffic.
pub fn system_addrs() -> Addrs {
    let Ok(list) = nix::ifaddrs::getifaddrs() else { return Vec::new() };
    list.filter_map(|ifa| {
        let addr = ifa.address?;
        let mask = ifa.netmask;
        if let Some(a) = addr.as_sockaddr_in() {
            let prefix = mask.and_then(|m| m.as_sockaddr_in().map(|m| u32::from(m.ip()).count_ones() as u8)).unwrap_or(32);
            Some((ifa.interface_name, IpAddr::V4(a.ip()), prefix))
        } else if let Some(a) = addr.as_sockaddr_in6() {
            let prefix = mask.and_then(|m| m.as_sockaddr_in6().map(|m| u128::from(m.ip()).count_ones() as u8)).unwrap_or(128);
            Some((ifa.interface_name, IpAddr::V6(a.ip()), prefix))
        } else {
            None
        }
    })
    .collect()
}

/// One row of `/proc/net/route` that is a default route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub iface: String,
    pub gateway: Ipv4Addr,
    pub metric: u32,
}

/// Default IPv4 routes (destination and mask 0, UP|GATEWAY), lowest metric first.
pub fn parse_default_routes(route_table: &str) -> Vec<Route> {
    let mut v: Vec<Route> = route_table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let c: Vec<&str> = line.split_whitespace().collect();
            // Iface Destination Gateway Flags RefCnt Use Metric Mask …
            if c.len() < 8 || c[1] != "00000000" || c[7] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(c[3], 16).ok()?;
            if flags & 0x1 == 0 {
                return None;
            }
            let gw = u32::from_str_radix(c[2], 16).ok()?;
            Some(Route { iface: c[0].to_string(), gateway: Ipv4Addr::from(gw.to_ne_bytes()), metric: c[6].parse().ok()? })
        })
        .collect();
    v.sort_by_key(|r| r.metric);
    v
}

/// `nameserver` lines of a resolv.conf, in order, without duplicates.
pub fn parse_nameservers(conf: &str) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for line in conf.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("nameserver") else { continue };
        let addr = rest.trim().split('%').next().unwrap_or("");
        if let Ok(ip) = addr.parse::<IpAddr>()
            && !out.contains(&ip)
        {
            out.push(ip);
        }
    }
    out
}

/// DNS servers: `/etc/resolv.conf`, replaced by systemd-resolved's real upstreams when
/// the file only points at the local stub.
pub fn read_dns(paths: &Paths) -> Vec<IpAddr> {
    let main = parse_nameservers(&fs::read_to_string(&paths.resolv_conf).unwrap_or_default());
    let stub = |ip: &IpAddr| matches!(ip, IpAddr::V4(a) if a.octets() == [127, 0, 0, 53]);
    if !main.is_empty() && main.iter().all(stub) {
        let up = parse_nameservers(&fs::read_to_string(&paths.resolved_conf).unwrap_or_default());
        if !up.is_empty() {
            return up;
        }
    }
    main
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Raw per-interface facts before addresses and routes are merged in.
#[derive(Clone, Debug, Default)]
pub struct RawIf {
    pub name: String,
    pub kind: IfKind,
    pub operstate: OperState,
    pub carrier: Option<bool>,
    pub speed_mbps: Option<u32>,
    pub duplex: Option<Duplex>,
    pub mtu: Option<u32>,
    pub mac: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// `carrier_up_count`: increments on every carrier-on transition.
    pub carrier_up_count: Option<u64>,
}

const ARPHRD_LOOPBACK: u32 = 772;
const ARPHRD_NONE: u32 = 65534;

/// Classify an interface from its sysfs directory.
pub fn classify(dir: &Path, name: &str) -> IfKind {
    let arphrd = read_trim(&dir.join("type")).and_then(|t| t.parse::<u32>().ok());
    if name == "lo" || arphrd == Some(ARPHRD_LOOPBACK) {
        return IfKind::Loopback;
    }
    let devtype = fs::read_to_string(dir.join("uevent"))
        .ok()
        .and_then(|u| u.lines().find_map(|l| l.strip_prefix("DEVTYPE=").map(str::to_string)))
        .unwrap_or_default();
    if dir.join("wireless").exists() || dir.join("phy80211").exists() || devtype == "wlan" {
        return IfKind::Wifi;
    }
    let physical = dir.join("device").exists();
    if physical {
        return IfKind::Wired;
    }
    let tunnel_type = matches!(arphrd, Some(ARPHRD_NONE | 768 | 769 | 776 | 778 | 779 | 512));
    let tunnel_name = ["wg", "tun", "tap", "tailscale", "ppp", "ipsec", "zt", "utun"].iter().any(|p| name.starts_with(p));
    if matches!(devtype.as_str(), "wireguard" | "ppp") || tunnel_type || tunnel_name {
        return IfKind::Tunnel;
    }
    if devtype == "bridge" || dir.join("bridge").exists() || ["br-", "docker", "virbr", "lxcbr", "cni"].iter().any(|p| name.starts_with(p)) {
        return IfKind::Bridge;
    }
    IfKind::Virtual
}

fn read_raw(sys_net: &Path, name: &str) -> RawIf {
    let dir = sys_net.join(name);
    let num = |f: &str| read_trim(&dir.join(f)).and_then(|s| s.parse::<i64>().ok());
    let mac = read_trim(&dir.join("address")).filter(|m| !m.is_empty() && m.chars().any(|c| c != '0' && c != ':'));
    RawIf {
        name: name.to_string(),
        kind: classify(&dir, name),
        operstate: read_trim(&dir.join("operstate")).map(|s| OperState::parse(&s)).unwrap_or_default(),
        // Reading `carrier` / `speed` of a down interface fails with EINVAL: unknown.
        carrier: num("carrier").map(|c| c == 1),
        speed_mbps: num("speed").filter(|s| *s > 0).map(|s| s as u32),
        duplex: match read_trim(&dir.join("duplex")).as_deref() {
            Some("full") => Some(Duplex::Full),
            Some("half") => Some(Duplex::Half),
            _ => None,
        },
        mtu: num("mtu").filter(|m| *m > 0).map(|m| m as u32),
        mac,
        rx_bytes: num("statistics/rx_bytes").unwrap_or(0).max(0) as u64,
        tx_bytes: num("statistics/tx_bytes").unwrap_or(0).max(0) as u64,
        carrier_up_count: num("carrier_up_count").map(|c| c.max(0) as u64),
    }
}

/// Everything one sample needs.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub raw: Vec<RawIf>,
    pub addrs: Addrs,
    pub routes: Vec<Route>,
    pub dns: Vec<IpAddr>,
}

pub fn read_snapshot(paths: &Paths, addrs: &dyn Fn() -> Addrs) -> Snapshot {
    let mut names: Vec<String> = fs::read_dir(&paths.sys_net)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Snapshot {
        raw: names.iter().map(|n| read_raw(&paths.sys_net, n)).collect(),
        addrs: addrs(),
        routes: parse_default_routes(&fs::read_to_string(&paths.proc_route).unwrap_or_default()),
        dns: read_dns(paths),
    }
}

/// Seconds since boot (`/proc/uptime`), to date a link that has been up since boot.
pub fn read_uptime_secs(paths: &Paths) -> Option<u64> {
    let s = fs::read_to_string(&paths.proc_uptime).ok()?;
    s.split_whitespace().next()?.parse::<f64>().ok().map(|f| f as u64)
}

fn kind_rank(k: IfKind) -> u8 {
    match k {
        IfKind::Wired => 0,
        IfKind::Wifi => 1,
        IfKind::Tunnel => 2,
        IfKind::Bridge => 3,
        IfKind::Virtual => 4,
        IfKind::Loopback => 5,
    }
}

/// Merge raw facts, addresses and routes into [`Interface`]s (stable order: wired, wifi,
/// tunnels, bridges, others, loopback; then by name).
pub fn build_interfaces(snap: &Snapshot) -> Vec<Interface> {
    let mut v: Vec<Interface> = snap
        .raw
        .iter()
        .map(|r| {
            let mut ipv4 = Vec::new();
            let mut ipv6 = Vec::new();
            for (n, a, p) in &snap.addrs {
                if n == &r.name {
                    match a {
                        IpAddr::V4(_) => ipv4.push(IpNet::new(*a, *p)),
                        IpAddr::V6(_) => ipv6.push(IpNet::new(*a, *p)),
                    }
                }
            }
            // Global before link-local (stable otherwise).
            ipv4.sort_by_key(|a| a.is_link_local());
            ipv6.sort_by_key(|a| a.is_link_local());
            let route = snap.routes.iter().find(|rt| rt.iface == r.name);
            Interface {
                name: r.name.clone(),
                kind: r.kind,
                operstate: r.operstate,
                carrier: r.carrier,
                speed_mbps: r.speed_mbps,
                duplex: r.duplex,
                mtu: r.mtu,
                mac: r.mac.clone(),
                ipv4,
                ipv6,
                gateway: route.map(|rt| rt.gateway),
                is_default: route.is_some(),
                rx_bytes: r.rx_bytes,
                tx_bytes: r.tx_bytes,
                ..Default::default()
            }
        })
        .collect();
    v.sort_by(|a, b| kind_rank(a.kind).cmp(&kind_rank(b.kind)).then_with(|| a.name.cmp(&b.name)));
    v
}

/// The interface the shell is about: the lowest-metric default route that is not a
/// tunnel, else the best physical link (wired with carrier, any wired, wifi with carrier,
/// any wifi).
pub fn pick_primary(ifaces: &[Interface], routes: &[Route]) -> Option<usize> {
    for r in routes {
        if let Some(i) = ifaces.iter().position(|i| i.name == r.iface && i.kind != IfKind::Tunnel) {
            return Some(i);
        }
    }
    let phys = |k: IfKind| ifaces.iter().position(|i| i.kind == k && i.has_carrier());
    let any = |k: IfKind| ifaces.iter().position(|i| i.kind == k);
    phys(IfKind::Wired).or(phys(IfKind::Wifi)).or(any(IfKind::Wired)).or(any(IfKind::Wifi))
}

fn vpn_kind(name: &str) -> VpnKind {
    if name.starts_with("tailscale") {
        VpnKind::Tailscale
    } else if name.starts_with("wg") {
        VpnKind::WireGuard
    } else if name.starts_with("tun") || name.starts_with("tap") {
        VpnKind::Tun
    } else {
        VpnKind::Other
    }
}

/// Tunnel interfaces that carry an address (down tunnels without one are not VPNs the
/// user is "connected" to; they stay out of the list).
pub fn detect_vpns(ifaces: &[Interface], routes: &[Route]) -> Vec<Vpn> {
    ifaces
        .iter()
        .filter(|i| i.kind == IfKind::Tunnel)
        .filter(|i| !i.ipv4.is_empty() || i.ipv6.iter().any(|a| !a.is_link_local()))
        .map(|i| Vpn {
            name: i.name.clone(),
            kind: vpn_kind(&i.name),
            connected: i.operstate != OperState::Down && i.carrier != Some(false),
            addrs: i.ipv4.iter().chain(i.ipv6.iter().filter(|a| !a.is_link_local())).map(|a| a.addr).collect(),
            default_route: routes.first().is_some_and(|r| r.iface == i.name),
            toggleable: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fake_iface(root: &Path, name: &str, files: &[(&str, &str)], dirs: &[&str]) {
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

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
eno1\t00000000\t0164A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
eno1\t0064A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn default_routes_sorted_by_metric_with_decoded_gateway() {
        let r = parse_default_routes(ROUTE);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], Route { iface: "eno1".into(), gateway: Ipv4Addr::new(192, 168, 100, 1), metric: 100 });
        assert_eq!(r[1].iface, "wlan0");
        assert!(parse_default_routes("Iface\tDestination\n").is_empty());
    }

    #[test]
    fn nameservers_dedup_and_stub_replacement() {
        let conf = "# x\nsearch lan\nnameserver 1.1.1.1\nnameserver 1.1.1.1\nnameserver fe80::1%eth0\n";
        let v = parse_nameservers(conf);
        assert_eq!(v, vec!["1.1.1.1".parse::<IpAddr>().unwrap(), "fe80::1".parse().unwrap()]);

        let dir = tempfile::tempdir().unwrap();
        let p = Paths::under(dir.path());
        fs::create_dir_all(p.resolv_conf.parent().unwrap()).unwrap();
        fs::create_dir_all(p.resolved_conf.parent().unwrap()).unwrap();
        fs::write(&p.resolv_conf, "nameserver 127.0.0.53\n").unwrap();
        // No resolved file: keep the stub.
        assert_eq!(read_dns(&p), vec!["127.0.0.53".parse::<IpAddr>().unwrap()]);
        fs::write(&p.resolved_conf, "nameserver 9.9.9.9\nnameserver 1.1.1.1\n").unwrap();
        assert_eq!(read_dns(&p), vec!["9.9.9.9".parse::<IpAddr>().unwrap(), "1.1.1.1".parse().unwrap()]);
    }

    fn fixture(root: &Path) -> Paths {
        let p = Paths::under(root);
        fake_iface(
            &p.sys_net,
            "eno1",
            &[
                ("operstate", "up"),
                ("carrier", "1"),
                ("speed", "1000"),
                ("duplex", "full"),
                ("mtu", "1500"),
                ("address", "d8:5e:d3:6d:a1:44"),
                ("carrier_up_count", "1"),
                ("statistics/rx_bytes", "1000"),
                ("statistics/tx_bytes", "500"),
            ],
            &["device"],
        );
        fake_iface(&p.sys_net, "wlan0", &[("operstate", "down"), ("type", "1")], &["device", "wireless"]);
        fake_iface(&p.sys_net, "tailscale0", &[("operstate", "unknown"), ("carrier", "1"), ("type", "65534"), ("mtu", "1280")], &[]);
        fake_iface(&p.sys_net, "wg0", &[("operstate", "unknown"), ("type", "65534")], &[]);
        fake_iface(&p.sys_net, "docker0", &[("operstate", "down"), ("type", "1")], &["bridge"]);
        fake_iface(&p.sys_net, "veth1234", &[("operstate", "up"), ("type", "1")], &[]);
        fake_iface(&p.sys_net, "lo", &[("operstate", "unknown"), ("type", "772")], &[]);
        fs::create_dir_all(p.proc_route.parent().unwrap()).unwrap();
        fs::write(&p.proc_route, ROUTE).unwrap();
        p
    }

    fn addrs() -> Addrs {
        vec![
            ("eno1".into(), "192.168.100.24".parse().unwrap(), 24),
            ("eno1".into(), "fe80::1".parse().unwrap(), 64),
            ("eno1".into(), "2a02:8070::4c1d:a1".parse().unwrap(), 64),
            ("tailscale0".into(), "100.64.0.7".parse().unwrap(), 32),
            ("lo".into(), "127.0.0.1".parse().unwrap(), 8),
        ]
    }

    #[test]
    fn snapshot_classifies_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        let snap = read_snapshot(&p, &addrs);
        let ifs = build_interfaces(&snap);
        let by = |n: &str| ifs.iter().find(|i| i.name == n).unwrap();
        assert_eq!(by("eno1").kind, IfKind::Wired);
        assert_eq!(by("wlan0").kind, IfKind::Wifi);
        assert_eq!(by("tailscale0").kind, IfKind::Tunnel);
        assert_eq!(by("wg0").kind, IfKind::Tunnel);
        assert_eq!(by("docker0").kind, IfKind::Bridge);
        assert_eq!(by("veth1234").kind, IfKind::Virtual);
        assert_eq!(by("lo").kind, IfKind::Loopback);
        let e = by("eno1");
        assert_eq!((e.speed_mbps, e.duplex, e.mtu), (Some(1000), Some(Duplex::Full), Some(1500)));
        assert_eq!(e.mac.as_deref(), Some("d8:5e:d3:6d:a1:44"));
        assert_eq!(e.link_text(), "1000 Mb/s · full duplex · MTU 1500");
        assert_eq!(e.gateway, Some(Ipv4Addr::new(192, 168, 100, 1)));
        assert_eq!(e.ipv4[0].to_string(), "192.168.100.24/24");
        // Global IPv6 sorted before the link-local one.
        assert_eq!(e.ipv6[0].to_string(), "2a02:8070::4c1d:a1/64");
        assert_eq!((e.rx_bytes, e.tx_bytes), (1000, 500));
        // Wired first in the list.
        assert_eq!(ifs[0].name, "eno1");
        assert_eq!(pick_primary(&ifs, &snap.routes), Some(0));

        // Only tailscale0 has an address among the tunnels.
        let vpns = detect_vpns(&ifs, &snap.routes);
        assert_eq!(vpns.len(), 1);
        assert_eq!(vpns[0].name, "tailscale0");
        assert_eq!(vpns[0].kind, VpnKind::Tailscale);
        assert!(!vpns[0].default_route);
    }

    #[test]
    fn primary_without_routes_prefers_wired_with_carrier() {
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        fs::write(&p.proc_route, "Iface\tDestination\n").unwrap();
        let snap = read_snapshot(&p, &addrs);
        let ifs = build_interfaces(&snap);
        assert_eq!(ifs[pick_primary(&ifs, &snap.routes).unwrap()].name, "eno1");
        // Unplugged: still the wired interface, not the dead wifi.
        fake_iface(&p.sys_net, "eno1", &[("operstate", "down"), ("carrier", "0")], &["device"]);
        let snap = read_snapshot(&p, &addrs);
        let ifs = build_interfaces(&snap);
        assert_eq!(ifs[pick_primary(&ifs, &snap.routes).unwrap()].name, "eno1");
    }

    #[test]
    fn tunnel_default_route_does_not_become_primary() {
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
tailscale0\t00000000\t00000000\t0003\t0\t0\t10\t00000000\t0\t0\t0\n\
eno1\t00000000\t0164A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        fs::write(&p.proc_route, route).unwrap();
        let snap = read_snapshot(&p, &addrs);
        let ifs = build_interfaces(&snap);
        assert_eq!(ifs[pick_primary(&ifs, &snap.routes).unwrap()].name, "eno1");
        assert!(detect_vpns(&ifs, &snap.routes)[0].default_route);
    }

    #[test]
    fn uptime_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::under(dir.path());
        fs::create_dir_all(p.proc_uptime.parent().unwrap()).unwrap();
        fs::write(&p.proc_uptime, "12345.67 99999.00\n").unwrap();
        assert_eq!(read_uptime_secs(&p), Some(12345));
    }
}
