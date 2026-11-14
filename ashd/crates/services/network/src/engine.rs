//! The part both backends share: read the kernel's view of the links, keep the daily
//! totals / link age / throughput history / flap debouncing, and derive the primary
//! connection. The read-only backend publishes this as is; the NetworkManager backend
//! overlays profiles, Wi-Fi and its own connection state.

use std::{collections::VecDeque, path::PathBuf, time::Instant};

use crate::{
    flap::Flap,
    model::{
        Backend, HISTORY_LEN, IfKind, Interface, LinkEvent, LinkEventKind, LinkStatus, NetworkState, Primary, SAMPLE_INTERVAL, Sample,
        Step, StepStatus, Throughput,
    },
    sysfs::{self, Addrs, Paths, Snapshot},
    usage::Usage,
};

/// A link that is up without any address for this long is "limited", not "connecting".
pub const CONNECT_GRACE_SECS: u64 = 30;

/// Per-tick inputs that are not files.
pub struct Tick<'a> {
    pub now: Instant,
    /// Local date, `2026-11-14`.
    pub date: &'a str,
    pub unix: u64,
    pub sampling: bool,
}

/// Throughput of one interface from successive byte counters.
#[derive(Default)]
struct Sampler {
    last: Option<(Instant, String, u64, u64)>,
    history: VecDeque<Sample>,
    current: Sample,
    sampled_at: Option<Instant>,
}

impl Sampler {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record the counters; with `record` the rate over the elapsed time is appended to
    /// the history (the slow poll only refreshes the current rate).
    fn feed(&mut self, now: Instant, ifname: &str, rx: u64, tx: u64, record: bool) {
        if let Some((t, name, lrx, ltx)) = &self.last
            && name == ifname
        {
            let dt = now.duration_since(*t).as_secs_f32();
            if dt > 0.05 {
                let s = Sample { rx_bps: rx.saturating_sub(*lrx) as f32 / dt, tx_bps: tx.saturating_sub(*ltx) as f32 / dt };
                self.current = s;
                if record {
                    self.history.push_back(s);
                    while self.history.len() > HISTORY_LEN {
                        self.history.pop_front();
                    }
                    self.sampled_at = Some(now);
                }
            }
        } else {
            // Different interface (or first sample): start over.
            self.history.clear();
            self.current = Sample::default();
            self.sampled_at = None;
        }
        self.last = Some((now, ifname.to_string(), rx, tx));
    }

    fn snapshot(&self) -> Throughput {
        let history: Vec<Sample> = self.history.iter().copied().collect();
        let window_max_bps = history.iter().fold(0.0f32, |m, s| m.max(s.rx_bps).max(s.tx_bps));
        Throughput {
            rx_bps: self.current.rx_bps,
            tx_bps: self.current.tx_bps,
            history,
            interval: SAMPLE_INTERVAL,
            window_max_bps,
            sampled_at: self.sampled_at,
        }
    }
}

pub struct Core {
    pub paths: Paths,
    addrs: Box<dyn Fn() -> Addrs + Send>,
    pub usage: Usage,
    flap: Flap,
    sampler: Sampler,
    event_seq: u64,
    last_event: Option<LinkEvent>,
    was_sampling: bool,
}

impl Core {
    pub fn new(paths: Paths, addrs: Box<dyn Fn() -> Addrs + Send>, usage_file: Option<PathBuf>) -> Self {
        Self {
            paths,
            addrs,
            usage: Usage::load(usage_file),
            flap: Flap::default(),
            sampler: Sampler::default(),
            event_seq: 0,
            last_event: None,
            was_sampling: false,
        }
    }

    /// Read everything and derive the state.
    pub fn tick(&mut self, t: &Tick<'_>) -> NetworkState {
        let snap = sysfs::read_snapshot(&self.paths, &*self.addrs);
        self.derive(&snap, t)
    }

    pub fn derive(&mut self, snap: &Snapshot, t: &Tick<'_>) -> NetworkState {
        let mut ifaces = sysfs::build_interfaces(snap);
        let uptime = sysfs::read_uptime_secs(&self.paths);

        for i in &mut ifaces {
            let raw = snap.raw.iter().find(|r| r.name == i.name);
            if matches!(i.kind, IfKind::Wired | IfKind::Wifi | IfKind::Tunnel) {
                i.today = self.usage.update(t.date, &i.name, i.rx_bytes, i.tx_bytes);
            }
            if matches!(i.kind, IfKind::Wired | IfKind::Wifi) {
                i.link_up_secs = self.usage.link_age(&i.name, i.has_carrier(), raw.and_then(|r| r.carrier_up_count), t.unix, uptime);
            }
        }

        let primary_ix = sysfs::pick_primary(&ifaces, &snap.routes);
        let primary = primary_ix.map(|ix| derive_primary(&ifaces[ix], None));

        // Throughput of the primary only.
        if t.sampling != self.was_sampling {
            // Fresh view: start the history over, the next tick is the baseline.
            self.sampler.reset();
            self.was_sampling = t.sampling;
        }
        match primary_ix {
            Some(ix) => self.sampler.feed(t.now, &ifaces[ix].name.clone(), ifaces[ix].rx_bytes, ifaces[ix].tx_bytes, t.sampling),
            None => self.sampler.reset(),
        }

        // Link events for every physical interface.
        for i in &ifaces {
            if !matches!(i.kind, IfKind::Wired | IfKind::Wifi) {
                continue;
            }
            let status = if primary_ix.is_some_and(|ix| ifaces[ix].name == i.name) {
                primary.as_ref().map(|p| p.status).unwrap_or_default()
            } else {
                derive_primary(i, None).status
            };
            if let Some(kind) = self.flap.observe(t.now, &i.name, i.has_carrier(), status) {
                self.event_seq += 1;
                self.last_event = Some(LinkEvent { seq: self.event_seq, kind, ifname: i.name.clone(), detail: event_detail(kind, i) });
            }
        }

        let vpns = sysfs::detect_vpns(&ifaces, &snap.routes);
        self.usage.flush_if_due(t.now);

        NetworkState {
            available: true,
            backend: Backend::ReadOnly,
            can_configure: false,
            primary,
            interfaces: ifaces,
            dns: snap.dns.clone(),
            vpns,
            throughput: self.sampler.snapshot(),
            sampling: t.sampling,
            last_event: self.last_event.clone(),
            ..Default::default()
        }
    }
}

/// What the primary connection looks like from the kernel's facts alone.
/// `profile` is filled by the NetworkManager backend.
pub fn derive_primary(i: &Interface, profile: Option<String>) -> Primary {
    let age = i.link_up_secs.unwrap_or(0);
    let wired = i.kind != IfKind::Wifi;
    let speed = i.speed_mbps.map(|s| match i.duplex {
        Some(crate::model::Duplex::Full) => format!("{s} Mb/s · full"),
        _ => format!("{s} Mb/s"),
    });
    let link_label = if wired { "Cable detected" } else { "Associated" };
    let mut steps = Vec::new();
    let mut hint = None;

    let status = if !i.has_carrier() {
        LinkStatus::Unplugged
    } else if i.ipv4.is_empty() && i.global_ipv6().is_none() {
        if age < CONNECT_GRACE_SECS { LinkStatus::Connecting } else { LinkStatus::Limited }
    } else if i.global_ipv4().is_none() && i.global_ipv6().is_none() {
        // Only 169.254.x.y: nobody answered.
        LinkStatus::Limited
    } else if !i.is_default {
        LinkStatus::Limited
    } else {
        LinkStatus::Connected
    };

    match status {
        LinkStatus::Connecting => {
            steps.push(Step::new(link_label, i.name.clone(), StepStatus::Done));
            steps.push(Step::new("Link negotiated", speed.unwrap_or_default(), StepStatus::Done));
            steps.push(Step::new("Getting an address", format!("waiting · {age} s"), StepStatus::Active));
            steps.push(Step::new("Checking internet", "—", StepStatus::Pending));
        }
        LinkStatus::Limited => {
            steps.push(Step::new("Link up", i.speed_mbps.map(|s| format!("{s} Mb/s")).unwrap_or_default(), StepStatus::Done));
            match (i.global_ipv4(), i.ipv4.first()) {
                (Some(a), _) => steps.push(Step::new("Address", a.to_string(), StepStatus::Done)),
                (None, Some(ll)) => {
                    steps.push(Step::new("No DHCP answer — self-assigned", ll.addr.to_string(), StepStatus::Warn));
                }
                (None, None) => steps.push(Step::new("No address assigned", "—", StepStatus::Fail)),
            }
            if !i.is_default {
                steps.push(Step::new("No default route", "—", StepStatus::Fail));
            }
            hint = Some(if i.global_ipv4().is_none() {
                "The cable is fine, but nothing on the network handed out an address. Check the router, or switch to a static profile."
                    .to_string()
            } else {
                "The link has an address but no route to the internet. Check the router or the gateway setting.".to_string()
            });
        }
        _ => {}
    }

    Primary { ifname: i.name.clone(), kind: i.kind, status, profile, steps, hint }
}

/// Toast body for a link event (`1 Gb/s · 192.168.1.24`).
pub fn event_detail(kind: LinkEventKind, i: &Interface) -> String {
    match kind {
        LinkEventKind::Connected | LinkEventKind::Reconnected => {
            let mut parts = Vec::new();
            if let Some(s) = i.speed_text() {
                parts.push(s);
            }
            if let Some(a) = i.global_ipv4() {
                parts.push(a.addr.to_string());
            }
            if parts.is_empty() { i.name.clone() } else { parts.join(" · ") }
        }
        LinkEventKind::Limited => match (i.global_ipv4(), i.ipv4.first()) {
            (None, Some(ll)) => format!("No DHCP answer on {} — using {}", i.name, ll.addr),
            (None, None) => format!("No address on {}", i.name),
            _ => format!("No route to the internet on {}", i.name),
        },
        LinkEventKind::Unplugged => {
            if i.kind == IfKind::Wifi { format!("{} lost its network", i.name) } else { format!("Cable removed from {}", i.name) }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::IpAddr, time::Duration};

    use super::*;
    use crate::sysfs::Paths;

    fn put(root: &std::path::Path, name: &str, files: &[(&str, &str)], dirs: &[&str]) {
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
eno1\t00000000\t0164A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";

    struct Rig {
        _dir: tempfile::TempDir,
        paths: Paths,
        core: Core,
        addrs: std::sync::Arc<std::sync::Mutex<Addrs>>,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        fs::create_dir_all(paths.proc_route.parent().unwrap()).unwrap();
        fs::write(&paths.proc_route, ROUTE).unwrap();
        let addrs: std::sync::Arc<std::sync::Mutex<Addrs>> = Default::default();
        let a2 = addrs.clone();
        let core = Core::new(paths.clone(), Box::new(move || a2.lock().unwrap().clone()), None);
        Rig { _dir: dir, paths, core, addrs }
    }

    fn eno1(r: &Rig, oper: &str, carrier: &str, rx: u64, tx: u64) {
        put(
            &r.paths.sys_net,
            "eno1",
            &[
                ("operstate", oper),
                ("carrier", carrier),
                ("speed", "1000"),
                ("duplex", "full"),
                ("mtu", "1500"),
                ("carrier_up_count", "1"),
                ("statistics/rx_bytes", &rx.to_string()),
                ("statistics/tx_bytes", &tx.to_string()),
            ],
            &["device"],
        );
    }

    fn tick(r: &mut Rig, t: Instant, sampling: bool) -> NetworkState {
        r.core.tick(&Tick { now: t, date: "2026-11-14", unix: 1_000_000, sampling })
    }

    #[test]
    fn connected_limited_connecting_unplugged() {
        let mut r = rig();
        let t0 = Instant::now();
        eno1(&r, "up", "1", 0, 0);
        *r.addrs.lock().unwrap() = vec![("eno1".into(), "192.168.100.24".parse::<IpAddr>().unwrap(), 24)];
        let s = tick(&mut r, t0, false);
        let p = s.primary.clone().unwrap();
        assert_eq!((p.ifname.as_str(), p.status), ("eno1", LinkStatus::Connected));
        assert_eq!(s.tile_title(), "Ethernet");
        assert_eq!(s.tile_subtitle(), "eno1 · 1 Gb/s");
        assert!(!s.can_configure);
        assert_eq!(s.backend, Backend::ReadOnly);

        // Self-assigned address only: limited, with the step list.
        *r.addrs.lock().unwrap() = vec![("eno1".into(), "169.254.12.7".parse().unwrap(), 16)];
        let s = tick(&mut r, t0 + Duration::from_secs(1), false);
        let p = s.primary.clone().unwrap();
        assert_eq!(p.status, LinkStatus::Limited);
        assert!(p.steps.iter().any(|st| st.label.starts_with("No DHCP answer") && st.status == StepStatus::Warn));
        assert!(p.hint.is_some());
        assert_eq!(s.tile_subtitle(), "no internet");

        // No address at all, link just came up (carrier_up_count 1, "uptime" unknown): link
        // age is `now` → connecting.
        *r.addrs.lock().unwrap() = vec![];
        fs::write(&r.paths.proc_route, "Iface\tDestination\n").unwrap();
        fs::remove_file(r.paths.sys_net.join("eno1/carrier_up_count")).unwrap();
        fs::write(r.paths.sys_net.join("eno1/carrier"), "0\n").unwrap();
        let s = tick(&mut r, t0 + Duration::from_secs(2), false);
        assert_eq!(s.status(), LinkStatus::Unplugged);
        assert_eq!(s.tile_subtitle(), "cable unplugged");
        fs::write(r.paths.sys_net.join("eno1/carrier"), "1\n").unwrap();
        let s = tick(&mut r, t0 + Duration::from_secs(3), false);
        assert_eq!(s.status(), LinkStatus::Connecting);
        assert_eq!(s.primary.unwrap().steps.len(), 4);
    }

    #[test]
    fn history_only_while_sampling_and_window_max() {
        let mut r = rig();
        let t0 = Instant::now();
        *r.addrs.lock().unwrap() = vec![("eno1".into(), "192.168.100.24".parse().unwrap(), 24)];
        eno1(&r, "up", "1", 0, 0);
        let s = tick(&mut r, t0, false);
        assert!(s.throughput.history.is_empty());
        // Sampling on: baseline tick, then 500 ms samples.
        let s = tick(&mut r, t0 + Duration::from_secs(1), true);
        assert!(s.sampling && s.throughput.history.is_empty());
        eno1(&r, "up", "1", 500_000, 100_000);
        let s = tick(&mut r, t0 + Duration::from_millis(1500), true);
        assert_eq!(s.throughput.history.len(), 1);
        assert!((s.throughput.rx_bps - 1_000_000.0).abs() < 1.0);
        eno1(&r, "up", "1", 500_000 + 250_000, 100_000 + 50_000);
        let s = tick(&mut r, t0 + Duration::from_millis(2000), true);
        assert_eq!(s.throughput.history.len(), 2);
        assert!((s.throughput.window_max_bps - 1_000_000.0).abs() < 1.0);
        assert!(s.throughput.sampled_at.is_some());
        // Ring buffer is bounded.
        for k in 0..100u64 {
            eno1(&r, "up", "1", 1_000_000 + k, 0);
            tick(&mut r, t0 + Duration::from_millis(2500 + 500 * k), true);
        }
        let s = tick(&mut r, t0 + Duration::from_secs(100), true);
        assert!(s.throughput.history.len() <= HISTORY_LEN);
        // Sampling off then on: the history starts over.
        tick(&mut r, t0 + Duration::from_secs(101), false);
        let s = tick(&mut r, t0 + Duration::from_secs(102), true);
        assert!(s.throughput.history.is_empty());
    }

    #[test]
    fn today_totals_and_link_age_come_from_the_tracker() {
        let mut r = rig();
        let t0 = Instant::now();
        *r.addrs.lock().unwrap() = vec![("eno1".into(), "192.168.100.24".parse().unwrap(), 24)];
        eno1(&r, "up", "1", 1000, 1000);
        fs::write(&r.paths.proc_uptime, "400.0 1.0\n").unwrap_or_default();
        let s = tick(&mut r, t0, false);
        assert_eq!(s.interfaces[0].today.total(), 0);
        eno1(&r, "up", "1", 3000, 1500);
        let s = tick(&mut r, t0 + Duration::from_secs(5), false);
        assert_eq!((s.interfaces[0].today.rx, s.interfaces[0].today.tx), (2000, 500));
        assert!(s.interfaces[0].link_up_secs.is_some());
    }

    #[test]
    fn flap_yields_one_event() {
        let mut r = rig();
        let t0 = Instant::now();
        *r.addrs.lock().unwrap() = vec![("eno1".into(), "192.168.100.24".parse().unwrap(), 24)];
        eno1(&r, "up", "1", 0, 0);
        assert!(tick(&mut r, t0, false).last_event.is_none());
        eno1(&r, "down", "0", 0, 0);
        assert!(tick(&mut r, t0 + Duration::from_secs(1), false).last_event.is_none());
        eno1(&r, "up", "1", 0, 0);
        let s = tick(&mut r, t0 + Duration::from_secs(3), false);
        let ev = s.last_event.unwrap();
        assert_eq!((ev.seq, ev.kind), (1, LinkEventKind::Reconnected));
        assert_eq!(ev.detail, "1 Gb/s · 192.168.100.24");
        // Nothing more.
        let s = tick(&mut r, t0 + Duration::from_secs(8), false);
        assert_eq!(s.last_event.unwrap().seq, 1);
    }
}
