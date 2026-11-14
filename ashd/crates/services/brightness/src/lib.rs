//! Display brightness: laptop backlight, DDC/CI monitors, a fake for tests. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<BrightnessState>)`,
//! newest-snapshot channel. Owner: idle / night light / brightness.
//!
//! * **Backends** (`backend.rs`): `Backlight` — `/sys/class/backlight/*` for reading and
//!   logind `Session.SetBrightness` for writing (no udev rule needed); `DdcBackend` —
//!   external monitors found through `/sys/class/drm/*/{status,edid,ddc}` and driven with
//!   DDC/CI (VCP 0x10) over `/dev/i2c-N` (`ddc.rs`: framing, checksums, retries, all
//!   tested against a fake monitor); `Fake` — in memory.
//! * **Which ones run**: `ASHD_NESTED=1` → only the fake (`ASHD_FAKE_BRIGHTNESS=n` picks
//!   how many displays, default 1; nothing under `/sys` or `/dev` is touched). Otherwise
//!   the backlight (if `/sys/class/backlight` has a device) and — **only when enabled** in
//!   `shell.json` / `power.json` (`"brightness": {"ddc": true}`, which the NixOS option
//!   `ashd.brightness.ddc.enable` writes) — DDC. `"backlight": false` disables the first.
//! * **Writes** are optimistic (the state changes at once), coalesced and rate-limited per
//!   display (`queue.rs`): a slider drag becomes at most one write per backend interval
//!   (150 ms for DDC), the newest value always wins. Values are cached; hardware is read
//!   at startup, for newly appearing displays, and on [`Handle::refresh`] (at most every
//!   30 s per display).
//! * **Hotplug**: sysfs is rescanned every 20 s (cheap; no DDC traffic for displays that
//!   are already known). A monitor that cannot be opened or does not answer DDC/CI is
//!   left alone until it is unplugged.
//! * `available == false` when there is nothing to control; UIs hide their slider.

mod backend;
mod ddc;
mod edid;
mod queue;

use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
pub use backend::{Backend, BacklightWriter, DdcBackend, Fake, Found, LogindWriter, Sysfs};
pub use ddc::{DdcCi, DdcError, I2cBus, Timing};
use queue::WriteQueue;
use serde::Deserialize;
use smol::future::or;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BackendKind {
    #[default]
    Fake,
    Backlight,
    Ddc,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Display {
    /// Stable id: `backlight:intel_backlight`, `ddc:card1-DP-1`, `fake:0`.
    pub id: String,
    pub name: String,
    /// 0.0..=1.0
    pub value: f32,
    pub backend: BackendKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BrightnessState {
    /// At least one display can be controlled.
    pub available: bool,
    pub displays: Vec<Display>,
}

impl BrightnessState {
    pub fn display(&self, id: &str) -> Option<&Display> {
        self.displays.iter().find(|d| d.id == id)
    }
    /// The first display (the OSD shows this one).
    pub fn primary(&self) -> Option<&Display> {
        self.displays.first()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    All,
    Display(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Absolute level, 0.0..=1.0.
    Set(Target, f32),
    /// Relative change in percent points.
    Step(Target, i32),
    /// Re-read the hardware (rate-limited) and rescan.
    Refresh,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        if self.tx.try_send(cmd).is_err() {
            tracing::debug!("brightness service is not running");
        }
    }
    pub fn set(&self, target: Target, value: f32) {
        self.send(Command::Set(target, value));
    }
    pub fn step(&self, target: Target, percent: i32) {
        self.send(Command::Step(target, percent));
    }
    pub fn refresh(&self) {
        self.send(Command::Refresh);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// `"brightness"` section of `shell.json` / `power.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub backlight: bool,
    pub ddc: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { backlight: true, ddc: false }
    }
}

#[derive(Deserialize)]
struct ConfigFile {
    brightness: Option<RawConfig>,
}
#[derive(Deserialize, Default)]
struct RawConfig {
    backlight: Option<bool>,
    ddc: Option<bool>,
}

impl Config {
    /// `<etc>/shell.json`, `<etc>/power.json`, then the user's `shell.json`.
    pub fn load() -> Self {
        let etc = ashd_tokens::state::etc_dir();
        let mut paths = vec![etc.join("shell.json"), etc.join("power.json")];
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        {
            paths.push(dir.join("ashd/shell.json"));
        }
        Self::load_from(&paths)
    }

    pub fn load_from(paths: &[impl AsRef<std::path::Path>]) -> Self {
        let mut cfg = Self::default();
        for p in paths {
            let Ok(text) = std::fs::read_to_string(p) else { continue };
            match serde_json::from_str::<ConfigFile>(&text) {
                Ok(f) => {
                    if let Some(raw) = f.brightness {
                        cfg.backlight = raw.backlight.unwrap_or(cfg.backlight);
                        cfg.ddc = raw.ddc.unwrap_or(cfg.ddc);
                    }
                }
                Err(e) => tracing::warn!("brightness config {}: {e}", p.as_ref().display()),
            }
        }
        cfg
    }
}

/// `ASHD_NESTED` is set to something other than `""` / `0`.
fn nested() -> bool {
    std::env::var_os("ASHD_NESTED").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The backends for this environment (see the crate docs).
pub fn default_backends(cfg: Config) -> Vec<Box<dyn Backend>> {
    if nested() {
        let n = std::env::var("ASHD_FAKE_BRIGHTNESS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).min(8);
        return vec![Box::new(Fake::new(n))];
    }
    let mut v: Vec<Box<dyn Backend>> = Vec::new();
    if cfg.backlight {
        v.push(Box::new(backend::Backlight::new("/sys/class/backlight".into(), Box::new(LogindWriter::default()))));
    }
    if cfg.ddc {
        v.push(Box::new(DdcBackend::system()));
    }
    v
}

/// Spawn the `ashd-svc-brightness` thread.
pub fn start() -> (Handle, Receiver<BrightnessState>) {
    start_with(default_backends(Config::load()))
}

/// Like [`start`] with `n` fake displays, whatever the environment (UI tests, demos).
pub fn start_fake(n: usize) -> (Handle, Receiver<BrightnessState>) {
    start_with(vec![Box::new(Fake::new(n))])
}

/// Like [`start`] with explicit backends.
pub fn start_with(backends: Vec<Box<dyn Backend>>) -> (Handle, Receiver<BrightnessState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-brightness".into())
        .spawn(move || smol::block_on(run(rx, state_tx, backends)))
        .expect("spawn ashd-svc-brightness");
    (Handle { tx }, state_rx)
}

const RESCAN: Duration = Duration::from_secs(20);
const REREAD_MIN: Duration = Duration::from_secs(30);

struct Entry {
    display: Display,
    backend: usize,
    last_read: Instant,
}

/// Percent step on the 0..1 scale, rounded to whole percents.
fn stepped(value: f32, percent: i32) -> f32 {
    (((value * 100.0).round() + percent as f32) / 100.0).clamp(0.0, 1.0)
}

fn scan_all(backends: &mut [Box<dyn Backend>], old: &[Entry], queue: &WriteQueue) -> Vec<Entry> {
    let mut out = Vec::new();
    for (ix, b) in backends.iter_mut().enumerate() {
        let kind = b.kind();
        for f in b.scan() {
            // Keep the optimistic value while a write is on its way, and the read time.
            let previous = old.iter().find(|e| e.display.id == f.id);
            let value = match previous {
                Some(p) if queue.has_pending(&f.id) => p.display.value,
                _ => f.value.clamp(0.0, 1.0),
            };
            out.push(Entry {
                display: Display { id: f.id, name: f.name, value, backend: kind },
                backend: ix,
                last_read: previous.map_or_else(Instant::now, |p| p.last_read),
            });
        }
    }
    out
}

fn snapshot(entries: &[Entry]) -> BrightnessState {
    BrightnessState { available: !entries.is_empty(), displays: entries.iter().map(|e| e.display.clone()).collect() }
}

enum Wake {
    Command(Result<Command, async_channel::RecvError>),
    Timeout,
}

async fn run(commands: Receiver<Command>, state_tx: Sender<BrightnessState>, mut backends: Vec<Box<dyn Backend>>) {
    let mut queue = WriteQueue::default();
    let mut last_published = BrightnessState::default();
    let _ = state_tx.force_send(last_published.clone());
    // Probing can take a while (DDC reads); the unavailable snapshot above is already out.
    let mut entries = scan_all(&mut backends, &[], &queue);
    let mut next_scan = Instant::now() + RESCAN;

    loop {
        let snap = snapshot(&entries);
        if snap != last_published {
            let _ = state_tx.force_send(snap.clone());
            last_published = snap;
        }

        let now = Instant::now();
        let deadline = queue.next_deadline().map_or(next_scan, |d| d.min(next_scan));
        let wait = deadline.saturating_duration_since(now);
        let wake = or(async { Wake::Command(commands.recv().await) }, async {
            smol::Timer::after(wait).await;
            Wake::Timeout
        })
        .await;

        let mut batch = Vec::new();
        match wake {
            Wake::Command(Err(_)) => return,
            Wake::Command(Ok(cmd)) => {
                batch.push(cmd);
                // Everything already waiting is folded into one pass (a drag = one write).
                while let Ok(c) = commands.try_recv() {
                    batch.push(c);
                }
            }
            Wake::Timeout => {}
        }
        let mut refresh = false;
        for cmd in batch {
            match cmd {
                Command::Set(target, v) => {
                    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { continue };
                    for e in entries.iter_mut().filter(|e| matches(&target, &e.display.id)) {
                        e.display.value = v;
                        queue.push(&e.display.id, v, backends[e.backend].min_interval());
                    }
                }
                Command::Step(target, percent) => {
                    for e in entries.iter_mut().filter(|e| matches(&target, &e.display.id)) {
                        e.display.value = stepped(e.display.value, percent);
                        queue.push(&e.display.id, e.display.value, backends[e.backend].min_interval());
                    }
                }
                Command::Refresh => refresh = true,
            }
        }

        // Writes that may go out now (blocking: this thread has nothing else to do).
        for (id, value) in queue.due(Instant::now()) {
            let Some(e) = entries.iter_mut().find(|e| e.display.id == id) else { continue };
            if let Err(err) = backends[e.backend].set(&id, value) {
                tracing::warn!("brightness: cannot set {id}: {err}");
                // Show what the hardware really has.
                if let Ok(v) = backends[e.backend].read(&id) {
                    e.display.value = v;
                }
            }
        }

        if refresh {
            for e in entries.iter_mut() {
                if queue.has_pending(&e.display.id) || e.last_read.elapsed() < REREAD_MIN {
                    continue;
                }
                if let Ok(v) = backends[e.backend].read(&e.display.id) {
                    e.display.value = v;
                }
                e.last_read = Instant::now();
            }
        }
        if refresh || Instant::now() >= next_scan {
            entries = scan_all(&mut backends, &entries, &queue);
            for id in last_published.displays.iter().map(|d| d.id.clone()).filter(|id| !entries.iter().any(|e| &e.display.id == id)) {
                queue.forget(&id);
            }
            next_scan = Instant::now() + RESCAN;
        }
    }
}

fn matches(target: &Target, id: &str) -> bool {
    match target {
        Target::All => true,
        Target::Display(d) => d == id,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    /// The newest snapshot, kept between waits (the channel only carries changes).
    struct Watch<S> {
        rx: Receiver<S>,
        latest: Option<S>,
    }

    impl<S: Clone + std::fmt::Debug> Watch<S> {
        fn new(rx: Receiver<S>) -> Self {
            Self { rx, latest: None }
        }

        /// Every snapshot that arrived since the last call (the newest is also remembered).
        fn drain(&mut self) -> Vec<S> {
            let mut out = Vec::new();
            while let Ok(s) = self.rx.try_recv() {
                self.latest = Some(s.clone());
                out.push(s);
            }
            out
        }

        /// Wait (bounded) until the newest snapshot satisfies `pred`.
        fn until(&mut self, what: &str, pred: impl Fn(&S) -> bool) -> S {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.drain();
                if let Some(l) = self.latest.as_ref().filter(|l| pred(l)) {
                    return l.clone();
                }
                if Instant::now() >= deadline {
                    panic!("timed out waiting for {what}; last state {:?}", self.latest);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    use super::*;

    #[derive(Default)]
    struct Log {
        sets: Vec<(Instant, String, f32)>,
        reads: u32,
        fail_sets: bool,
    }

    struct Recording {
        kind: BackendKind,
        displays: Vec<(String, f32)>,
        interval: Duration,
        log: Arc<Mutex<Log>>,
    }

    impl Backend for Recording {
        fn kind(&self) -> BackendKind {
            self.kind
        }
        fn scan(&mut self) -> Vec<backend::Found> {
            self.displays.iter().map(|(id, v)| backend::Found { id: id.clone(), name: format!("name of {id}"), value: *v }).collect()
        }
        fn read(&mut self, id: &str) -> Result<f32, String> {
            self.log.lock().unwrap().reads += 1;
            self.displays.iter().find(|d| d.0 == id).map(|d| d.1).ok_or_else(|| "gone".into())
        }
        fn set(&mut self, id: &str, value: f32) -> Result<(), String> {
            let mut log = self.log.lock().unwrap();
            if log.fail_sets {
                return Err("bus error".into());
            }
            log.sets.push((Instant::now(), id.to_string(), value));
            if let Some(d) = self.displays.iter_mut().find(|d| d.0 == id) {
                d.1 = value;
            }
            Ok(())
        }
        fn min_interval(&self) -> Duration {
            self.interval
        }
    }

    fn lvl(s: &BrightnessState, i: usize) -> f32 {
        s.displays.get(i).map_or(f32::NAN, |d| d.value)
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    fn recording(kind: BackendKind, ids: &[(&str, f32)], interval_ms: u64) -> (Box<dyn Backend>, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        (
            Box::new(Recording { kind, displays: ids.iter().map(|(i, v)| (i.to_string(), *v)).collect(), interval: Duration::from_millis(interval_ms), log: log.clone() }),
            log,
        )
    }


    #[test]
    fn stepping_rounds_to_whole_percents_and_clamps() {
        assert_eq!(stepped(0.40, 5), 0.45);
        assert_eq!(stepped(0.403, 5), 0.45);
        assert_eq!(stepped(0.98, 5), 1.0);
        assert_eq!(stepped(0.02, -5), 0.0);
        assert_eq!(stepped(0.5, 0), 0.5);
    }

    #[test]
    fn config_merges_files_and_defaults_to_ddc_off() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        std::fs::write(&a, r#"{"dock":[],"brightness":{"backlight":false}}"#).unwrap();
        std::fs::write(&b, r#"{"brightness":{"ddc":true}}"#).unwrap();
        assert_eq!(Config::load_from(&[&a]), Config { backlight: false, ddc: false });
        assert_eq!(Config::load_from(&[&a, &b, &dir.path().join("none.json")]), Config { backlight: false, ddc: true });
        assert_eq!(Config::load_from(&[] as &[std::path::PathBuf]), Config { backlight: true, ddc: false });
    }

    #[test]
    fn the_fake_backend_lists_displays_and_follows_commands() {
        let (h, rx) = start_fake(2);
        let mut rx = Watch::new(rx);
        let s = rx.until("two displays", |s| s.displays.len() == 2);
        assert!(s.available);
        assert_eq!(s.displays[0].backend, BackendKind::Fake);
        assert_eq!(s.displays[0].name, "Fake display 1");
        assert!((s.displays[0].value - 0.40).abs() < 1e-6);

        h.step(Target::Display("fake:0".into()), 10);
        rx.until("step", |s| near(lvl(s, 0), 0.50) && near(lvl(s, 1), 0.70));
        h.set(Target::All, 0.25);
        rx.until("set all", |s| !s.displays.is_empty() && s.displays.iter().all(|d| near(d.value, 0.25)));
        h.step(Target::All, -50);
        let s = rx.until("clamped", |s| !s.displays.is_empty() && s.displays.iter().all(|d| d.value == 0.0));
        assert_eq!(s.primary().unwrap().id, "fake:0");
        h.set(Target::All, f32::NAN);
        h.set(Target::Display("nope".into()), 0.9);
        h.set(Target::All, 7.0);
        rx.until("clamped high", |s| !s.displays.is_empty() && s.displays.iter().all(|d| d.value == 1.0));
    }

    #[test]
    fn nothing_to_control_means_unavailable() {
        let (_h, rx) = start_with(Vec::new());
        let mut rx = Watch::new(rx);
        let s = rx.until("first", |_| true);
        assert_eq!(s, BrightnessState { available: false, displays: Vec::new() });
    }

    #[test]
    fn a_slider_flood_reaches_the_hardware_rate_limited_with_the_last_value() {
        let (b, log) = recording(BackendKind::Ddc, &[("ddc:a", 0.3)], 150);
        let (h, rx) = start_with(vec![b]);
        let mut rx = Watch::new(rx);
        rx.until("listed", |s| s.available);
        let t0 = Instant::now();
        // 40 updates, 5 ms apart: a 200 ms drag
        for i in 0..40 {
            h.set(Target::Display("ddc:a".into()), 0.30 + i as f32 / 100.0);
            std::thread::sleep(Duration::from_millis(5));
        }
        // the UI sees every value at once
        let s = rx.until("ui value", |s| near(lvl(s, 0), 0.69));
        assert_eq!(s.displays[0].backend, BackendKind::Ddc);
        // the hardware gets the final value after the interval
        let deadline = Instant::now() + Duration::from_secs(3);
        while !log.lock().unwrap().sets.last().is_some_and(|(_, _, v)| (*v - 0.69).abs() < 1e-6) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let log = log.lock().unwrap();
        let sets = &log.sets;
        assert!(sets.last().is_some_and(|(_, _, v)| (*v - 0.69).abs() < 1e-6), "final value written: {sets:?}");
        assert!(sets.len() <= 4, "40 updates became {} writes", sets.len());
        for w in sets.windows(2) {
            assert!(w[1].0 - w[0].0 >= Duration::from_millis(140), "writes at least ~150 ms apart");
        }
        assert!(sets[0].0 - t0 < Duration::from_millis(100), "the first write is not delayed");
    }

    #[test]
    fn a_failed_write_shows_what_the_hardware_has() {
        let (b, log) = recording(BackendKind::Ddc, &[("ddc:a", 0.3)], 0);
        let (h, rx) = start_with(vec![b]);
        let mut rx = Watch::new(rx);
        rx.until("listed", |s| s.available);
        log.lock().unwrap().fail_sets = true;
        h.set(Target::All, 0.9);
        // The write fails, the service re-reads the hardware and keeps showing its value
        // (the optimistic 0.9 is taken back within the same pass, so no snapshot differs).
        let deadline = Instant::now() + Duration::from_secs(5);
        while log.lock().unwrap().reads == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(log.lock().unwrap().reads > 0, "the display was read back after the failed write");
        std::thread::sleep(Duration::from_millis(50));
        rx.drain();
        assert!(near(lvl(rx.latest.as_ref().unwrap(), 0), 0.3), "{:?}", rx.latest);
    }

    #[test]
    fn displays_of_several_backends_are_listed_in_backend_order() {
        let (a, _) = recording(BackendKind::Backlight, &[("backlight:x", 0.5)], 0);
        let (b, _) = recording(BackendKind::Ddc, &[("ddc:card1-DP-1", 0.8)], 0);
        let (h, rx) = start_with(vec![a, b]);
        let mut rx = Watch::new(rx);
        let s = rx.until("both", |s| s.displays.len() == 2);
        assert_eq!(s.displays.iter().map(|d| d.backend).collect::<Vec<_>>(), [BackendKind::Backlight, BackendKind::Ddc]);
        h.step(Target::All, 10);
        rx.until("both stepped", |s| near(lvl(s, 0), 0.6) && near(lvl(s, 1), 0.9));
    }
}
