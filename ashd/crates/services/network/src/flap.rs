//! Link flap debouncing: turns raw carrier / status observations into [`LinkEvent`]s
//! (det-12: "unplug or replug fires a toast; a link that flaps within 5 s shows one toast,
//! not two").

use std::{collections::HashMap, time::Instant};

use crate::model::{FLAP_WINDOW, LinkEventKind, LinkStatus};

struct Track {
    raw_up: bool,
    down_at: Option<Instant>,
    /// What the user was last told.
    reported: LinkStatus,
    /// The carrier bounced inside the flap window after having been reported up.
    flapped: bool,
}

#[derive(Default)]
pub struct Flap {
    tracks: HashMap<String, Track>,
}

impl Flap {
    /// Observe one interface. `status` is the status as derived from the same sample.
    /// Returns an event kind when something worth a toast happened:
    ///
    /// * down for [`FLAP_WINDOW`] → `Unplugged` (a shorter bounce reports nothing yet);
    /// * back up and settled (`Connected` / `Limited`) → `Connected`, `Limited`, or — if it
    ///   bounced while reported up — one `Reconnected` instead of an unplug/replug pair.
    ///
    /// The first observation only records the baseline.
    pub fn observe(&mut self, now: Instant, ifname: &str, raw_up: bool, status: LinkStatus) -> Option<LinkEventKind> {
        let Some(t) = self.tracks.get_mut(ifname) else {
            let reported = if raw_up { status } else { LinkStatus::Unplugged };
            self.tracks.insert(ifname.to_string(), Track { raw_up, down_at: (!raw_up).then_some(now), reported, flapped: false });
            return None;
        };
        if raw_up != t.raw_up {
            if !raw_up {
                t.down_at = Some(now);
            } else if let Some(d) = t.down_at
                && t.reported != LinkStatus::Unplugged
                && now.duration_since(d) < FLAP_WINDOW
            {
                t.flapped = true;
            }
            t.raw_up = raw_up;
        }
        if !raw_up {
            let down_for = t.down_at.map(|d| now.duration_since(d)).unwrap_or_default();
            if t.reported != LinkStatus::Unplugged && down_for >= FLAP_WINDOW {
                t.reported = LinkStatus::Unplugged;
                t.flapped = false;
                return Some(LinkEventKind::Unplugged);
            }
            return None;
        }
        match status {
            LinkStatus::Connecting | LinkStatus::Unknown | LinkStatus::Unplugged | LinkStatus::Disconnected => None,
            LinkStatus::Connected | LinkStatus::Limited => {
                let flapped = std::mem::take(&mut t.flapped);
                let changed = status != t.reported;
                t.reported = status;
                match (status, changed, flapped) {
                    (LinkStatus::Connected, _, true) => Some(LinkEventKind::Reconnected),
                    (LinkStatus::Connected, true, false) => Some(LinkEventKind::Connected),
                    (LinkStatus::Limited, true, _) => Some(LinkEventKind::Limited),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(base: Instant, s: f32) -> Instant {
        base + Duration::from_secs_f32(s)
    }

    #[test]
    fn startup_is_silent_and_unplug_waits_for_the_window() {
        let t0 = Instant::now();
        let mut f = Flap::default();
        assert_eq!(f.observe(t0, "eno1", true, LinkStatus::Connected), None);
        assert_eq!(f.observe(at(t0, 1.0), "eno1", true, LinkStatus::Connected), None);
        assert_eq!(f.observe(at(t0, 2.0), "eno1", false, LinkStatus::Unplugged), None);
        assert_eq!(f.observe(at(t0, 5.0), "eno1", false, LinkStatus::Unplugged), None);
        assert_eq!(f.observe(at(t0, 7.5), "eno1", false, LinkStatus::Unplugged), Some(LinkEventKind::Unplugged));
        // Not reported twice.
        assert_eq!(f.observe(at(t0, 12.5), "eno1", false, LinkStatus::Unplugged), None);
        // Replug: wait until it settled, then one `Connected`.
        assert_eq!(f.observe(at(t0, 20.0), "eno1", true, LinkStatus::Connecting), None);
        assert_eq!(f.observe(at(t0, 22.0), "eno1", true, LinkStatus::Connected), Some(LinkEventKind::Connected));
        assert_eq!(f.observe(at(t0, 23.0), "eno1", true, LinkStatus::Connected), None);
    }

    #[test]
    fn a_flap_inside_five_seconds_is_one_event() {
        let t0 = Instant::now();
        let mut f = Flap::default();
        f.observe(t0, "eno1", true, LinkStatus::Connected);
        assert_eq!(f.observe(at(t0, 1.0), "eno1", false, LinkStatus::Unplugged), None);
        assert_eq!(f.observe(at(t0, 3.0), "eno1", true, LinkStatus::Connecting), None);
        assert_eq!(f.observe(at(t0, 4.0), "eno1", true, LinkStatus::Connected), Some(LinkEventKind::Reconnected));
        assert_eq!(f.observe(at(t0, 9.0), "eno1", true, LinkStatus::Connected), None);
    }

    #[test]
    fn flap_that_returns_to_connected_instantly_reports_reconnected_once() {
        let t0 = Instant::now();
        let mut f = Flap::default();
        f.observe(t0, "eno1", true, LinkStatus::Connected);
        f.observe(at(t0, 1.0), "eno1", false, LinkStatus::Unplugged);
        assert_eq!(f.observe(at(t0, 2.0), "eno1", true, LinkStatus::Connected), Some(LinkEventKind::Reconnected));
        assert_eq!(f.observe(at(t0, 2.5), "eno1", true, LinkStatus::Connected), None);
    }

    #[test]
    fn limited_is_reported_once_and_recovery_is_a_connect() {
        let t0 = Instant::now();
        let mut f = Flap::default();
        f.observe(t0, "eno1", true, LinkStatus::Connected);
        assert_eq!(f.observe(at(t0, 1.0), "eno1", true, LinkStatus::Limited), Some(LinkEventKind::Limited));
        assert_eq!(f.observe(at(t0, 2.0), "eno1", true, LinkStatus::Limited), None);
        assert_eq!(f.observe(at(t0, 3.0), "eno1", true, LinkStatus::Connected), Some(LinkEventKind::Connected));
    }
}
