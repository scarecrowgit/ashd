//! The backends: a trait, the fake, sysfs backlight, DDC/CI displays.
//!
//! All of them are blocking (sysfs reads, D-Bus round trips, I2C transactions with
//! 50 ms waits) and run on the service thread, never on a UI thread.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    BackendKind,
    ddc::{DdcCi, DevI2c, I2cBus, Timing},
    edid,
};

/// A display a backend currently offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Unique across backends, stable while the display stays connected.
    pub id: String,
    pub name: String,
    /// 0.0..=1.0 as last read (a display whose first read fails is not listed at all).
    pub value: f32,
}

pub trait Backend: Send {
    fn kind(&self) -> BackendKind;
    /// The displays present now. Values of already-known displays come from the cache;
    /// only new displays are read from the hardware.
    fn scan(&mut self) -> Vec<Found>;
    /// Read the hardware again.
    fn read(&mut self, id: &str) -> Result<f32, String>;
    fn set(&mut self, id: &str, value: f32) -> Result<(), String>;
    /// Writes to the same display are at least this far apart.
    fn min_interval(&self) -> Duration;
}

// ── fake ─────────────────────────────────────────────────────────────────────────────────

/// In-memory displays: tests and every `ASHD_NESTED=1` session.
pub struct Fake {
    displays: Vec<(String, String, f32)>,
}

impl Fake {
    /// `n` displays named "Fake display 1" ... at distinct starting levels.
    pub fn new(n: usize) -> Self {
        let displays = (0..n).map(|i| (format!("fake:{i}"), format!("Fake display {}", i + 1), [0.40, 0.70, 0.55, 0.25][i % 4])).collect();
        Self { displays }
    }
}

impl Backend for Fake {
    fn kind(&self) -> BackendKind {
        BackendKind::Fake
    }
    fn scan(&mut self) -> Vec<Found> {
        self.displays.iter().map(|(id, name, value)| Found { id: id.clone(), name: name.clone(), value: *value }).collect()
    }
    fn read(&mut self, id: &str) -> Result<f32, String> {
        self.displays.iter().find(|d| d.0 == id).map(|d| d.2).ok_or_else(|| format!("no display {id}"))
    }
    fn set(&mut self, id: &str, value: f32) -> Result<(), String> {
        let d = self.displays.iter_mut().find(|d| d.0 == id).ok_or_else(|| format!("no display {id}"))?;
        d.2 = value;
        tracing::debug!("fake brightness: {id} = {value:.2}");
        Ok(())
    }
    fn min_interval(&self) -> Duration {
        Duration::ZERO
    }
}

// ── backlight ────────────────────────────────────────────────────────────────────────────

/// How a raw backlight value reaches the kernel.
pub trait BacklightWriter: Send {
    fn write(&mut self, device: &str, raw: u32) -> Result<(), String>;
}

/// Preferred order of `/sys/class/backlight/*/type` (ddcutil, brightnessctl, and the
/// kernel docs agree: raw hardware control beats platform beats firmware).
fn type_rank(kind: &str) -> u8 {
    match kind.trim() {
        "raw" => 0,
        "platform" => 1,
        "firmware" => 2,
        _ => 3,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BacklightDevice {
    pub name: String,
    pub max: u32,
    pub current: u32,
}

/// The best backlight device under `root` (`/sys/class/backlight`).
pub fn scan_backlight(root: &Path) -> Option<BacklightDevice> {
    let mut best: Option<(u8, BacklightDevice)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        let read_u32 = |f: &str| std::fs::read_to_string(dir.join(f)).ok().and_then(|s| s.trim().parse::<u32>().ok());
        let (Some(max), Some(current)) = (read_u32("max_brightness"), read_u32("brightness")) else { continue };
        if max == 0 {
            continue;
        }
        let rank = type_rank(&std::fs::read_to_string(dir.join("type")).unwrap_or_default());
        let dev = BacklightDevice { name: entry.file_name().to_string_lossy().into_owned(), max, current };
        let better = best.as_ref().is_none_or(|(r, b)| (rank, std::cmp::Reverse(dev.max)) < (*r, std::cmp::Reverse(b.max)));
        if better {
            best = Some((rank, dev));
        }
    }
    best.map(|(_, d)| d)
}

/// A laptop panel through sysfs reads and a pluggable writer (logind in production).
pub struct Backlight {
    root: PathBuf,
    writer: Box<dyn BacklightWriter>,
    device: Option<BacklightDevice>,
}

impl Backlight {
    pub fn new(root: PathBuf, writer: Box<dyn BacklightWriter>) -> Self {
        Self { root, writer, device: None }
    }

    fn id(dev: &BacklightDevice) -> String {
        format!("backlight:{}", dev.name)
    }
}

impl Backend for Backlight {
    fn kind(&self) -> BackendKind {
        BackendKind::Backlight
    }
    fn scan(&mut self) -> Vec<Found> {
        self.device = scan_backlight(&self.root);
        self.device
            .iter()
            .map(|d| Found { id: Self::id(d), name: "Built-in display".into(), value: d.current as f32 / d.max as f32 })
            .collect()
    }
    fn read(&mut self, id: &str) -> Result<f32, String> {
        let dev = self.device.clone().filter(|d| Self::id(d) == id).ok_or_else(|| format!("no backlight {id}"))?;
        let raw: u32 = std::fs::read_to_string(self.root.join(&dev.name).join("brightness"))
            .map_err(|e| e.to_string())?
            .trim()
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?;
        Ok(raw as f32 / dev.max as f32)
    }
    fn set(&mut self, id: &str, value: f32) -> Result<(), String> {
        let dev = self.device.clone().filter(|d| Self::id(d) == id).ok_or_else(|| format!("no backlight {id}"))?;
        // Never write 0: many panels go fully black, and the user cannot see the slider.
        let raw = ((value.clamp(0.0, 1.0) * dev.max as f32).round() as u32).clamp(1, dev.max);
        self.writer.write(&dev.name, raw)
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(20)
    }
}

/// `org.freedesktop.login1.Session.SetBrightness("backlight", name, value)` on the
/// caller's own session (`/org/freedesktop/login1/session/auto`): works without any
/// udev rule or group membership.
#[derive(Default)]
pub struct LogindWriter {
    conn: Option<zbus::Connection>,
}

impl BacklightWriter for LogindWriter {
    fn write(&mut self, device: &str, raw: u32) -> Result<(), String> {
        smol::block_on(async {
            let conn = match &self.conn {
                Some(c) => c.clone(),
                None => {
                    let c = zbus::Connection::system().await.map_err(|e| e.to_string())?;
                    self.conn = Some(c.clone());
                    c
                }
            };
            let proxy = zbus::Proxy::new(&conn, "org.freedesktop.login1", "/org/freedesktop/login1/session/auto", "org.freedesktop.login1.Session")
                .await
                .map_err(|e| e.to_string())?;
            let r: zbus::Result<()> = proxy.call("SetBrightness", &("backlight", device, raw)).await;
            r.map_err(|e| {
                self.conn = None;
                format!("logind SetBrightness: {e}")
            })
        })
    }
}

// ── DDC/CI displays ──────────────────────────────────────────────────────────────────────

/// Where the kernel exposes things (injectable for tests).
#[derive(Clone, Debug)]
pub struct Sysfs {
    /// `/sys/class/drm`
    pub drm: PathBuf,
    /// `/dev`
    pub dev: PathBuf,
}

impl Default for Sysfs {
    fn default() -> Self {
        Self { drm: "/sys/class/drm".into(), dev: "/dev".into() }
    }
}

/// A connected external connector with an EDID and a DDC bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdcCandidate {
    /// `card1-DP-1`
    pub connector: String,
    pub name: String,
    /// `/dev/i2c-N`
    pub bus: PathBuf,
}

/// Scan `/sys/class/drm/*`: connected, not an internal panel, EDID present, `ddc` symlink
/// to an `i2c-N` adapter. Pure reads of sysfs — nothing is sent to any display.
pub fn scan_ddc_candidates(sys: &Sysfs) -> Vec<DdcCandidate> {
    let Ok(entries) = std::fs::read_dir(&sys.drm) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let connector = entry.file_name().to_string_lossy().into_owned();
        // `card1-DP-1`, not `card1` / `renderD128` / `version`.
        let Some((_card, kind)) = connector.split_once('-') else { continue };
        if kind.starts_with("eDP") || kind.starts_with("LVDS") || kind.starts_with("DSI") || kind.starts_with("Writeback") {
            continue;
        }
        if std::fs::read_to_string(dir.join("status")).map(|s| s.trim() != "connected").unwrap_or(true) {
            continue;
        }
        let Ok(edid_bytes) = std::fs::read(dir.join("edid")) else { continue };
        let Some(name) = edid::display_name(&edid_bytes) else { continue };
        let Ok(link) = std::fs::read_link(dir.join("ddc")) else { continue };
        let Some(n) = link.file_name().and_then(|f| f.to_str()).and_then(|f| f.strip_prefix("i2c-")).and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        out.push(DdcCandidate { connector, name, bus: sys.dev.join(format!("i2c-{n}")) });
    }
    out.sort_by(|a, b| a.connector.cmp(&b.connector));
    out
}

/// Opens the bus of a candidate; tests return fakes.
pub type OpenBus = Box<dyn FnMut(&DdcCandidate) -> std::io::Result<Box<dyn I2cBus>> + Send>;

struct DdcDisplay {
    candidate: DdcCandidate,
    ddc: DdcCi<Box<dyn I2cBus>>,
    max: u16,
    value: f32,
}

/// External monitors, found through sysfs and driven with DDC/CI.
pub struct DdcBackend {
    sys: Sysfs,
    open: OpenBus,
    timing: Timing,
    displays: HashMap<String, DdcDisplay>,
    /// Candidates that failed to answer once: not probed again until they are unplugged
    /// (a monitor without DDC/CI should not be poked on every rescan).
    rejected: Vec<String>,
}

impl DdcBackend {
    pub fn new(sys: Sysfs, open: OpenBus, timing: Timing) -> Self {
        Self { sys, open, timing, displays: HashMap::new(), rejected: Vec::new() }
    }

    /// The real thing: `/sys/class/drm` and `/dev/i2c-*`.
    pub fn system() -> Self {
        Self::new(
            Sysfs::default(),
            Box::new(|c| DevI2c::open(&c.bus).map(|b| Box::new(b) as Box<dyn I2cBus>)),
            Timing::REAL,
        )
    }

    fn id(connector: &str) -> String {
        format!("ddc:{connector}")
    }
}

impl Backend for DdcBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Ddc
    }

    fn scan(&mut self) -> Vec<Found> {
        let candidates = scan_ddc_candidates(&self.sys);
        // Forget what was unplugged (and give a re-plugged monitor a fresh chance).
        self.displays.retain(|_, d| candidates.iter().any(|c| c.connector == d.candidate.connector));
        self.rejected.retain(|r| candidates.iter().any(|c| &c.connector == r));
        for c in candidates {
            let id = Self::id(&c.connector);
            if self.displays.contains_key(&id) || self.rejected.contains(&c.connector) {
                continue;
            }
            let bus = match (self.open)(&c) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!("brightness: {}: {}: {e}", c.connector, c.bus.display());
                    self.rejected.push(c.connector.clone());
                    continue;
                }
            };
            let mut ddc = DdcCi::new(bus, self.timing);
            match ddc.brightness() {
                Ok(v) if v.max > 0 => {
                    self.displays.insert(id, DdcDisplay { candidate: c, ddc, max: v.max, value: v.fraction() });
                }
                Ok(_) => self.rejected.push(c.connector.clone()),
                Err(e) => {
                    tracing::debug!("brightness: {} does not answer DDC/CI brightness: {e}", c.connector);
                    self.rejected.push(c.connector.clone());
                }
            }
        }
        let mut out: Vec<_> =
            self.displays.iter().map(|(id, d)| Found { id: id.clone(), name: d.candidate.name.clone(), value: d.value }).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn read(&mut self, id: &str) -> Result<f32, String> {
        let d = self.displays.get_mut(id).ok_or_else(|| format!("no display {id}"))?;
        let v = d.ddc.brightness().map_err(|e| e.to_string())?;
        d.max = v.max.max(1);
        d.value = v.fraction();
        Ok(d.value)
    }

    fn set(&mut self, id: &str, value: f32) -> Result<(), String> {
        let d = self.displays.get_mut(id).ok_or_else(|| format!("no display {id}"))?;
        d.ddc.set_brightness(d.max, value).map_err(|e| e.to_string())?;
        d.value = value.clamp(0.0, 1.0);
        Ok(())
    }

    fn min_interval(&self) -> Duration {
        // DDC/CI is slow (a set keeps the monitor busy for ~50 ms, a get needs 40 ms): never
        // flood the bus while a slider is dragged.
        Duration::from_millis(150)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        os::unix::fs::symlink,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::{ddc::tests::FakeMonitor, edid::tests::sample};

    // ── sysfs fixtures ──

    pub(crate) fn backlight_fixture(root: &Path, name: &str, kind: &str, max: u32, cur: u32) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("type"), format!("{kind}\n")).unwrap();
        std::fs::write(d.join("max_brightness"), format!("{max}\n")).unwrap();
        std::fs::write(d.join("brightness"), format!("{cur}\n")).unwrap();
    }

    fn drm_fixture(sys: &Sysfs, connector: &str, status: &str, edid: Option<&[u8]>, i2c: Option<u32>) {
        let d = sys.drm.join(connector);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("status"), format!("{status}\n")).unwrap();
        if let Some(e) = edid {
            std::fs::write(d.join("edid"), e).unwrap();
        }
        if let Some(n) = i2c {
            let adapter = sys.drm.join(format!("../adapters/i2c-{n}"));
            std::fs::create_dir_all(&adapter).unwrap();
            symlink(format!("../adapters/i2c-{n}"), d.join("ddc")).unwrap();
        }
    }

    fn sysfs(dir: &Path) -> Sysfs {
        std::fs::create_dir_all(dir.join("class/drm")).unwrap();
        Sysfs { drm: dir.join("class/drm"), dev: dir.join("dev") }
    }

    // ── backlight ──

    #[derive(Clone, Default)]
    struct RecordingWriter(Arc<Mutex<Vec<(String, u32)>>>);
    impl BacklightWriter for RecordingWriter {
        fn write(&mut self, device: &str, raw: u32) -> Result<(), String> {
            self.0.lock().unwrap().push((device.into(), raw));
            Ok(())
        }
    }

    #[test]
    fn the_best_backlight_wins_by_type_then_range() {
        let dir = tempfile::tempdir().unwrap();
        backlight_fixture(dir.path(), "acpi_video0", "firmware", 100, 50);
        backlight_fixture(dir.path(), "intel_backlight", "raw", 96000, 48000);
        backlight_fixture(dir.path(), "nvidia_0", "platform", 255, 10);
        let dev = scan_backlight(dir.path()).unwrap();
        assert_eq!(dev, BacklightDevice { name: "intel_backlight".into(), max: 96000, current: 48000 });
        assert!(scan_backlight(&dir.path().join("missing")).is_none());
        let empty = tempfile::tempdir().unwrap();
        assert!(scan_backlight(empty.path()).is_none());
    }

    #[test]
    fn backlight_reads_sysfs_and_writes_through_the_writer_never_zero() {
        let dir = tempfile::tempdir().unwrap();
        backlight_fixture(dir.path(), "intel_backlight", "raw", 1000, 250);
        let w = RecordingWriter::default();
        let mut b = Backlight::new(dir.path().to_path_buf(), Box::new(w.clone()));
        let found = b.scan();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "backlight:intel_backlight");
        assert!((found[0].value - 0.25).abs() < 1e-6);
        b.set("backlight:intel_backlight", 0.5).unwrap();
        b.set("backlight:intel_backlight", 0.0).unwrap();
        b.set("backlight:intel_backlight", 3.0).unwrap();
        assert_eq!(*w.0.lock().unwrap(), vec![("intel_backlight".into(), 500), ("intel_backlight".into(), 1), ("intel_backlight".into(), 1000)]);
        assert!(b.set("backlight:other", 0.5).is_err());
        // The kernel's value is read back from sysfs.
        std::fs::write(dir.path().join("intel_backlight/brightness"), "750\n").unwrap();
        assert!((b.read("backlight:intel_backlight").unwrap() - 0.75).abs() < 1e-6);
    }

    // ── DDC discovery ──

    #[test]
    fn candidates_are_connected_external_connectors_with_edid_and_a_bus() {
        let dir = tempfile::tempdir().unwrap();
        let sys = sysfs(dir.path());
        let edid = sample(Some("DELL U2723QE"));
        drm_fixture(&sys, "card1-DP-1", "connected", Some(&edid), Some(7));
        drm_fixture(&sys, "card1-HDMI-A-1", "disconnected", Some(&edid), Some(8));
        drm_fixture(&sys, "card1-DP-2", "connected", None, Some(9));
        drm_fixture(&sys, "card1-DP-3", "connected", Some(&edid), None);
        drm_fixture(&sys, "card1-eDP-1", "connected", Some(&edid), Some(3));
        drm_fixture(&sys, "card1-HDMI-A-2", "connected", Some(&sample(None)), Some(5));
        std::fs::create_dir_all(sys.drm.join("card1")).unwrap();
        std::fs::create_dir_all(sys.drm.join("renderD128")).unwrap();
        let found = scan_ddc_candidates(&sys);
        assert_eq!(
            found,
            vec![
                DdcCandidate { connector: "card1-DP-1".into(), name: "DELL U2723QE".into(), bus: sys.dev.join("i2c-7") },
                DdcCandidate { connector: "card1-HDMI-A-2".into(), name: "DEL 41A8".into(), bus: sys.dev.join("i2c-5") },
            ]
        );
    }

    // ── DDC backend over fake monitors ──

    fn ddc_backend(sys: Sysfs, monitors: Arc<Mutex<HashMap<String, FakeMonitor>>>, opened: Arc<Mutex<Vec<String>>>) -> DdcBackend {
        DdcBackend::new(
            sys,
            Box::new(move |c| {
                opened.lock().unwrap().push(c.connector.clone());
                monitors
                    .lock()
                    .unwrap()
                    .get(&c.connector)
                    .cloned()
                    .map(|m| Box::new(m) as Box<dyn I2cBus>)
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::PermissionDenied, "not in the i2c group"))
            }),
            Timing::INSTANT,
        )
    }

    #[test]
    fn ddc_backend_lists_probes_once_sets_and_forgets_unplugged_monitors() {
        let dir = tempfile::tempdir().unwrap();
        let sys = sysfs(dir.path());
        let edid = sample(Some("DELL U2723QE"));
        drm_fixture(&sys, "card1-DP-1", "connected", Some(&edid), Some(7));
        let mon = FakeMonitor::new(30, 100);
        let monitors = Arc::new(Mutex::new(HashMap::from([("card1-DP-1".to_string(), mon.clone())])));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let mut b = ddc_backend(sys.clone(), monitors, opened.clone());

        let found = b.scan();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ddc:card1-DP-1");
        assert_eq!(found[0].name, "DELL U2723QE");
        assert!((found[0].value - 0.30).abs() < 1e-6);
        let wire_after_first = mon.shared.lock().unwrap().wire.len();
        assert_eq!(wire_after_first, 1, "one Get VCP to learn the value");
        // Rescans use the cache: no more traffic.
        b.scan();
        b.scan();
        assert_eq!(mon.shared.lock().unwrap().wire.len(), 1);

        b.set("ddc:card1-DP-1", 0.8).unwrap();
        assert_eq!(mon.shared.lock().unwrap().brightness, 80);
        assert!((b.read("ddc:card1-DP-1").unwrap() - 0.80).abs() < 1e-6);
        assert!(b.set("ddc:nope", 0.5).is_err());
        assert_eq!(b.min_interval(), Duration::from_millis(150));

        // Unplugged: gone from the list; re-plugged: probed again.
        std::fs::write(sys.drm.join("card1-DP-1/status"), "disconnected\n").unwrap();
        assert!(b.scan().is_empty());
        std::fs::write(sys.drm.join("card1-DP-1/status"), "connected\n").unwrap();
        assert_eq!(b.scan().len(), 1);
        assert_eq!(opened.lock().unwrap().len(), 2);
    }

    #[test]
    fn monitors_that_cannot_be_opened_or_do_not_answer_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let sys = sysfs(dir.path());
        let edid = sample(Some("MUTE"));
        drm_fixture(&sys, "card1-DP-1", "connected", Some(&edid), Some(7)); // no permission
        drm_fixture(&sys, "card1-DP-2", "connected", Some(&edid), Some(8)); // never answers
        let mute = FakeMonitor::new(0, 0);
        mute.shared.lock().unwrap().garble = 99;
        let monitors = Arc::new(Mutex::new(HashMap::from([("card1-DP-2".to_string(), mute.clone())])));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let mut b = ddc_backend(sys, monitors, opened.clone());
        assert!(b.scan().is_empty());
        let wire = mute.shared.lock().unwrap().wire.len();
        assert_eq!(wire, 3, "one probe with its three attempts");
        // Not probed again on later scans.
        assert!(b.scan().is_empty());
        assert_eq!(mute.shared.lock().unwrap().wire.len(), wire);
        assert_eq!(opened.lock().unwrap().len(), 2, "each connector opened once");
    }

    #[test]
    fn fake_backend_behaves() {
        let mut f = Fake::new(2);
        let found = f.scan();
        assert_eq!(found.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Fake display 1", "Fake display 2"]);
        f.set("fake:1", 0.9).unwrap();
        assert_eq!(f.read("fake:1").unwrap(), 0.9);
        assert!(f.set("fake:9", 0.1).is_err());
        assert_eq!(f.kind(), BackendKind::Fake);
    }
}
