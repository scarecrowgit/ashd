//! Write coalescing and rate limiting. Pure: the caller passes `Instant`s.
//!
//! A slider drag produces dozens of values per second; DDC/CI can take two or three per
//! second without upsetting the monitor. Per display, the first value goes out at once
//! (leading edge), later ones are held and only the newest is written once the display's
//! minimum interval has passed (trailing edge), so the final position always arrives.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Entry {
    pending: Option<f32>,
    last_write: Option<Instant>,
    interval: Duration,
}

#[derive(Default)]
pub struct WriteQueue {
    entries: HashMap<String, Entry>,
}

impl WriteQueue {
    /// Remember `value` as the one to write next for `id`.
    pub fn push(&mut self, id: &str, value: f32, interval: Duration) {
        let e = self.entries.entry(id.to_string()).or_insert(Entry { pending: None, last_write: None, interval });
        e.interval = interval;
        e.pending = Some(value);
    }

    /// The writes that may happen at `now`; they count as written from here on.
    pub fn due(&mut self, now: Instant) -> Vec<(String, f32)> {
        let mut out = Vec::new();
        for (id, e) in &mut self.entries {
            let Some(value) = e.pending else { continue };
            if e.last_write.is_none_or(|t| now >= t + e.interval) {
                e.pending = None;
                e.last_write = Some(now);
                out.push((id.clone(), value));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// When the earliest held value becomes writable (may be in the past).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.entries.values().filter(|e| e.pending.is_some()).map(|e| e.last_write.map_or_else(Instant::now, |t| t + e.interval)).min()
    }

    pub fn has_pending(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|e| e.pending.is_some())
    }

    pub fn forget(&mut self, id: &str) {
        self.entries.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn the_first_write_is_immediate_and_a_flood_collapses_to_the_newest() {
        let t0 = Instant::now();
        let mut q = WriteQueue::default();
        q.push("d", 0.10, MS(150));
        assert_eq!(q.due(t0), vec![("d".to_string(), 0.10)]);
        // 60 slider updates over the next 120 ms
        for i in 0..60 {
            q.push("d", 0.1 + i as f32 / 100.0, MS(150));
            assert!(q.due(t0 + MS(2 * i + 1)).is_empty(), "held back inside the interval");
        }
        assert_eq!(q.next_deadline(), Some(t0 + MS(150)));
        assert!(q.due(t0 + MS(149)).is_empty());
        let w = q.due(t0 + MS(150));
        assert_eq!(w.len(), 1);
        assert!((w[0].1 - 0.69).abs() < 1e-6, "only the newest value: {w:?}");
        assert!(q.due(t0 + MS(400)).is_empty());
        assert_eq!(q.next_deadline(), None);
    }

    #[test]
    fn displays_are_limited_independently() {
        let t0 = Instant::now();
        let mut q = WriteQueue::default();
        q.push("slow", 0.5, MS(150));
        q.push("fast", 0.5, MS(0));
        assert_eq!(q.due(t0).len(), 2);
        q.push("slow", 0.6, MS(150));
        q.push("fast", 0.6, MS(0));
        let due = q.due(t0 + MS(10));
        assert_eq!(due, vec![("fast".to_string(), 0.6)]);
        assert!(q.has_pending("slow") && !q.has_pending("fast"));
        assert_eq!(q.due(t0 + MS(151)), vec![("slow".to_string(), 0.6)]);
    }

    #[test]
    fn the_writes_over_a_two_second_drag_stay_under_the_rate_limit() {
        let t0 = Instant::now();
        let mut q = WriteQueue::default();
        let mut writes = 0;
        for ms in 0..2000u64 {
            q.push("d", ms as f32 / 2000.0, MS(150));
            writes += q.due(t0 + MS(ms)).len();
        }
        assert!(writes <= 14, "{writes} writes in 2 s at a 150 ms interval");
        assert!(writes >= 13);
        // and the last value still goes out
        let last = q.due(t0 + MS(2200));
        assert_eq!(last.len(), 1);
    }

    #[test]
    fn forgetting_drops_pending_values() {
        let t0 = Instant::now();
        let mut q = WriteQueue::default();
        q.push("d", 0.3, MS(100));
        q.forget("d");
        assert!(q.due(t0).is_empty());
        assert_eq!(q.next_deadline(), None);
    }
}
