//! Persisted bookkeeping in the ashd state dir (`network.json`): bytes moved today per
//! interface and, per interface, when the current carrier period began (so "connected ·
//! 4d 02h" survives a shell restart).

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::model::Totals;

#[derive(Serialize, Deserialize, Default, Debug)]
struct Entry {
    rx: u64,
    tx: u64,
    last_rx: u64,
    last_tx: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct LinkRec {
    /// `carrier_up_count` when `since` was recorded.
    count: u64,
    /// Unix seconds.
    since: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct File {
    /// Local date (`2026-11-14`) the `ifaces` totals belong to.
    date: String,
    ifaces: BTreeMap<String, Entry>,
    links: BTreeMap<String, LinkRec>,
}

/// Daily totals + link-up bookkeeping with lazy, rate-limited persistence.
pub struct Usage {
    path: Option<PathBuf>,
    file: File,
    dirty: bool,
    last_write: Instant,
}

/// How often a dirty file is written.
const FLUSH_EVERY: Duration = Duration::from_secs(60);

impl Usage {
    /// `None` = in-memory only (tests, mock mode).
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, file, dirty: false, last_write: Instant::now() }
    }

    /// Feed the raw counters of `ifname`; returns today's totals. Bytes moved while the
    /// shell was not running count too (the kernel counters keep going), unless the
    /// counter went backwards (reboot / interface re-created): then the new value is the
    /// delta.
    pub fn update(&mut self, date: &str, ifname: &str, rx: u64, tx: u64) -> Totals {
        if self.file.date != date {
            self.file.date = date.to_string();
            // Keep the baselines, drop yesterday's totals.
            for e in self.file.ifaces.values_mut() {
                e.rx = 0;
                e.tx = 0;
            }
            self.dirty = true;
        }
        let e = self.file.ifaces.entry(ifname.to_string()).or_insert_with(|| Entry { last_rx: rx, last_tx: tx, ..Default::default() });
        let drx = if rx >= e.last_rx { rx - e.last_rx } else { rx };
        let dtx = if tx >= e.last_tx { tx - e.last_tx } else { tx };
        if drx > 0 || dtx > 0 || e.last_rx != rx || e.last_tx != tx {
            e.rx = e.rx.saturating_add(drx);
            e.tx = e.tx.saturating_add(dtx);
            e.last_rx = rx;
            e.last_tx = tx;
            self.dirty = true;
        }
        Totals { rx: e.rx, tx: e.tx }
    }

    /// Seconds the carrier of `ifname` has been up. Tracked per `carrier_up_count`; a link
    /// seen for the first time that has come up exactly once since boot is dated to boot.
    pub fn link_age(&mut self, ifname: &str, carrier: bool, count: Option<u64>, now_unix: u64, uptime_secs: Option<u64>) -> Option<u64> {
        if !carrier {
            if self.file.links.remove(ifname).is_some() {
                self.dirty = true;
            }
            return None;
        }
        let count = count.unwrap_or(0);
        let fresh = match self.file.links.get(ifname) {
            Some(r) => r.count != count,
            None => true,
        };
        if fresh {
            let since = match (count, uptime_secs) {
                (1, Some(up)) => now_unix.saturating_sub(up),
                _ => now_unix,
            };
            self.file.links.insert(ifname.to_string(), LinkRec { count, since });
            self.dirty = true;
        }
        self.file.links.get(ifname).map(|r| now_unix.saturating_sub(r.since))
    }

    /// Write if dirty and the last write is older than a minute.
    pub fn flush_if_due(&mut self, now: Instant) {
        if self.dirty && now.duration_since(self.last_write) >= FLUSH_EVERY {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        self.last_write = Instant::now();
        let Some(path) = self.path.clone() else { return };
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Ok(text) = serde_json::to_string(&self.file) else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, &path)).is_err() {
            tracing::debug!("network: cannot persist {}", path.display());
        }
    }
}

impl Drop for Usage {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_accumulate_and_reset_each_day() {
        let mut u = Usage::load(None);
        assert_eq!(u.update("d1", "eno1", 1000, 100), Totals { rx: 0, tx: 0 });
        assert_eq!(u.update("d1", "eno1", 1500, 150), Totals { rx: 500, tx: 50 });
        // Counter reset (reboot): the new value counts as moved.
        assert_eq!(u.update("d1", "eno1", 20, 5), Totals { rx: 520, tx: 55 });
        // New day: totals restart, baseline kept.
        assert_eq!(u.update("d2", "eno1", 120, 5), Totals { rx: 100, tx: 0 });
    }

    #[test]
    fn persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("network.json");
        {
            let mut u = Usage::load(Some(p.clone()));
            u.update("d1", "eno1", 100, 10);
            u.update("d1", "eno1", 400, 40);
            u.link_age("eno1", true, Some(3), 1_000, None);
        } // drop flushes
        let mut u = Usage::load(Some(p));
        assert_eq!(u.update("d1", "eno1", 500, 40), Totals { rx: 400, tx: 30 });
        // Same carrier_up_count: the original start is kept.
        assert_eq!(u.link_age("eno1", true, Some(3), 1_500, None), Some(500));
        // A new carrier period restarts the clock.
        assert_eq!(u.link_age("eno1", true, Some(4), 1_600, None), Some(0));
    }

    #[test]
    fn link_up_once_since_boot_is_dated_to_boot() {
        let mut u = Usage::load(None);
        assert_eq!(u.link_age("eno1", true, Some(1), 100_000, Some(5_000)), Some(5_000));
        assert_eq!(u.link_age("eno1", false, Some(1), 100_001, Some(5_001)), None);
        // Unknown count: starts counting now.
        assert_eq!(u.link_age("eno1", true, None, 100_002, Some(5_002)), Some(0));
    }
}
