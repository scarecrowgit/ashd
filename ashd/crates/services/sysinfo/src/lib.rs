//! CPU / memory / temperature / load / uptime sampling plus a read-only network and
//! battery summary for the status bar. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<SysinfoState>)`;
//! the receiver always holds the newest snapshot (bounded(1) + `force_send`).
//! Sampling is cheap: one `sysinfo::System` kept across samples (CPU usage is a delta
//! between two refreshes), hwmon sensors, and a handful of tiny `/proc` / `/sys` reads
//! every [`DEFAULT_INTERVAL`].
//! Owner: shell frame.

pub mod net;
pub mod power;

use std::time::Duration;

use async_channel::{Receiver, Sender};
pub use net::{LinkKind, NetState, NetworkSummary};
pub use power::{BatteryInfo, BatteryStatus};
use sysinfo::{Components, System};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysinfoState {
    /// At least one sample has been taken.
    pub ready: bool,
    /// Total CPU usage, 0..=1.
    pub cpu: f32,
    /// Per-core usage, 0..=1.
    pub cpu_per_core: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// Hottest CPU/package sensor, °C.
    pub temp_c: Option<f32>,
    pub load_avg: [f64; 3],
    pub uptime: Duration,
    /// Default-route interface summary (det-12 indicator states).
    pub network: NetworkSummary,
    /// System battery, if the machine has one.
    pub battery: Option<BatteryInfo>,
}

impl SysinfoState {
    /// Used memory fraction, 0..=1.
    pub fn mem_fraction(&self) -> f32 {
        if self.mem_total == 0 { 0.0 } else { self.mem_used as f32 / self.mem_total as f32 }
    }

    /// Used memory as on the status bar: `9.4G`, `812M`.
    pub fn mem_used_label(&self) -> String {
        human_bytes(self.mem_used)
    }
}

/// `9.4G`, `812M`, `0M` (binary units, one decimal under 100 G).
pub fn human_bytes(bytes: u64) -> String {
    const G: f64 = (1u64 << 30) as f64;
    const M: f64 = (1u64 << 20) as f64;
    let b = bytes as f64;
    if b >= G {
        let g = b / G;
        if g >= 100.0 { format!("{g:.0}G") } else { format!("{g:.1}G") }
    } else {
        format!("{:.0}M", b / M)
    }
}

pub enum Command {
    SetInterval(Duration),
    RefreshNow,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// Sampling interval (default [`DEFAULT_INTERVAL`]; clamped to ≥ 250 ms).
    pub fn set_interval(&self, interval: Duration) {
        let _ = self.tx.try_send(Command::SetInterval(interval));
    }

    pub fn refresh_now(&self) {
        let _ = self.tx.try_send(Command::RefreshNow);
    }

    /// A handle connected to nothing (tests, `--no-services`).
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);
const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// Spawn the `ashd-svc-sysinfo` thread.
pub fn start() -> (Handle, Receiver<SysinfoState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-sysinfo".into())
        .spawn(move || smol::block_on(run(rx, state_tx)))
        .expect("spawn ashd-svc-sysinfo");
    (Handle { tx }, state_rx)
}

/// Keeps the `sysinfo` handles between samples.
struct Sampler {
    sys: Option<System>,
    components: Option<Components>,
}

impl Sampler {
    fn new() -> Self {
        let sys = System::new().map_err(|e| tracing::warn!("sysinfo: no system info: {e}")).ok();
        let components =
            Components::new_with_refreshed_list().map_err(|e| tracing::debug!("sysinfo: no sensors: {e}")).ok();
        let mut s = Self { sys, components };
        // CPU usage is a delta: prime the first refresh so the first sample is real.
        if let Some(sys) = s.sys.as_mut() {
            sys.refresh_cpu_usage();
        }
        s
    }

    fn sample(&mut self) -> SysinfoState {
        let mut st = SysinfoState { ready: true, ..Default::default() };
        if let Some(sys) = self.sys.as_mut() {
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            st.cpu = (sys.global_cpu_usage() / 100.0).clamp(0.0, 1.0);
            st.cpu_per_core = sys.cpus().iter().map(|c| (c.usage() / 100.0).clamp(0.0, 1.0)).collect();
            st.mem_used = sys.used_memory();
            st.mem_total = sys.total_memory();
            st.swap_used = sys.used_swap();
            st.swap_total = sys.total_swap();
        }
        if let Ok(l) = System::load_average() {
            st.load_avg = [l.one, l.five, l.fifteen];
        }
        if let Ok(up) = System::uptime() {
            st.uptime = Duration::from_secs(up);
        }
        if let Some(c) = self.components.as_mut() {
            c.refresh(false);
            st.temp_c = cpu_temperature(c.iter().filter_map(|c| Some((c.label().to_string(), c.temperature()?))));
        }
        st.network = net::sample();
        st.battery = power::sample();
        st
    }
}

/// Hottest CPU-ish sensor (package / Tctl / Tdie / core / cpu), else the hottest of all.
fn cpu_temperature(sensors: impl Iterator<Item = (String, f32)>) -> Option<f32> {
    let all: Vec<(String, f32)> = sensors.filter(|(_, t)| t.is_finite() && *t > 0.0).collect();
    let is_cpu = |l: &str| {
        let l = l.to_ascii_lowercase();
        ["package", "tctl", "tdie", "k10temp", "coretemp", "cpu", "core"].iter().any(|k| l.contains(k))
    };
    let max = |it: &mut dyn Iterator<Item = f32>| it.fold(None, |m: Option<f32>, t| Some(m.map_or(t, |m| m.max(t))));
    max(&mut all.iter().filter(|(l, _)| is_cpu(l)).map(|(_, t)| *t)).or_else(|| max(&mut all.iter().map(|(_, t)| *t)))
}

async fn run(commands: Receiver<Command>, state: Sender<SysinfoState>) {
    let mut sampler = Sampler::new();
    let mut interval = DEFAULT_INTERVAL;
    // The first CPU delta needs a short gap after priming.
    smol::Timer::after(Duration::from_millis(300)).await;
    loop {
        let snapshot = sampler.sample();
        if state.force_send(snapshot).is_err() {
            return; // consumer gone
        }
        let tick = async {
            smol::Timer::after(interval).await;
            None
        };
        let cmd = async { Some(commands.recv().await) };
        match smol::future::or(cmd, tick).await {
            None | Some(Ok(Command::RefreshNow)) => {}
            Some(Ok(Command::SetInterval(d))) => interval = d.max(MIN_INTERVAL),
            // All handles dropped: keep sampling on the timer alone.
            Some(Err(_)) => {
                smol::Timer::after(interval).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_labels() {
        assert_eq!(human_bytes(9_400 * (1 << 20)), "9.2G");
        assert_eq!(human_bytes(812 * (1 << 20)), "812M");
        assert_eq!(human_bytes(128 * (1 << 30)), "128G");
    }

    #[test]
    fn temperature_prefers_cpu_sensors() {
        let s = vec![("amdgpu edge".to_string(), 70.0), ("k10temp Tctl".to_string(), 55.0), ("nvme".to_string(), 40.0)];
        assert_eq!(cpu_temperature(s.into_iter()), Some(55.0));
        let s = vec![("acpitz".to_string(), 30.0), ("nvme".to_string(), 41.0)];
        assert_eq!(cpu_temperature(s.into_iter()), Some(41.0));
        assert_eq!(cpu_temperature(std::iter::empty()), None);
    }

    #[test]
    fn service_publishes_real_samples() {
        let (handle, rx) = start();
        let s = smol::block_on(rx.recv()).expect("a sample");
        assert!(s.ready);
        assert!(s.mem_total > 0);
        assert!((0.0..=1.0).contains(&s.cpu));
        handle.refresh_now();
        assert!(smol::block_on(rx.recv()).is_ok());
    }
}
