//! The toast queue, without GPUI: which notifications are on screen, which wait, when each
//! timer runs out. All time comes in as an `Instant` so the logic is unit-testable.

use std::time::{Duration, Instant};

use ashd_svc_notifications::{Notification, NotificationsState, Urgency};
use ashd_tokens::motion::timing::TOAST_OUT;

/// Cards on screen at once; the rest wait their turn.
pub const MAX_VISIBLE: usize = 4;

/// How long a card keeps fading out before it is removed.
pub const EXIT: Duration = Duration::from_millis(TOAST_OUT);

#[derive(Clone, Debug)]
pub struct Toast {
    pub n: Notification,
    /// Full lifetime; `None` = no timer (critical, `expire_timeout` 0).
    pub total: Option<Duration>,
    /// Time left as of `running_since` (or right now while paused / not started).
    pub remaining: Option<Duration>,
    /// The timer is running since this instant (`None`: queued, hovered or untimed).
    pub running_since: Option<Instant>,
    pub hovered: bool,
    pub exiting_since: Option<Instant>,
    /// Bumped whenever the timer (re)starts: keys the timer-bar animation.
    pub generation: u32,
}

impl Toast {
    fn new(n: Notification) -> Self {
        let total = n.timeout();
        Self { n, total, remaining: total, running_since: None, hovered: false, exiting_since: None, generation: 0 }
    }

    pub fn exiting(&self) -> bool {
        self.exiting_since.is_some()
    }

    /// Time left right now.
    pub fn remaining_at(&self, now: Instant) -> Option<Duration> {
        let rem = self.remaining?;
        Some(match self.running_since {
            Some(since) => rem.saturating_sub(now.saturating_duration_since(since)),
            None => rem,
        })
    }

    /// Fraction of the timer bar still filled at the moment the timer (re)started.
    pub fn bar_fraction(&self) -> Option<f32> {
        let (total, rem) = (self.total?, self.remaining?);
        Some((rem.as_secs_f32() / total.as_secs_f32().max(f32::EPSILON)).clamp(0.0, 1.0))
    }
}

#[derive(Default)]
pub struct Queue {
    pub toasts: Vec<Toast>,
    /// Highest `seq` handled; only newer notifications pop up.
    pub seen_seq: u64,
}

impl Queue {
    pub fn new(seen_seq: u64) -> Self {
        Self { toasts: Vec::new(), seen_seq }
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// Reconcile with the service snapshot. Returns whether anything visible may have changed.
    pub fn sync(&mut self, st: &NotificationsState, now: Instant) {
        for t in self.toasts.iter_mut().filter(|t| !t.exiting()) {
            match st.notifications.iter().find(|n| n.id == t.n.id) {
                None => t.exiting_since = Some(now),
                Some(n) if !n.toast_allowed(st.dnd) => t.exiting_since = Some(now),
                Some(n) if n.seq != t.n.seq => {
                    // Replaced in place: new content, timer starts over.
                    *t = Toast { exiting_since: None, hovered: t.hovered, generation: t.generation + 1, ..Toast::new(n.clone()) };
                }
                Some(_) => {}
            }
        }
        for n in st.notifications.iter().filter(|n| n.seq > self.seen_seq && n.toast_allowed(st.dnd)) {
            if self.toasts.iter().any(|t| t.n.id == n.id && !t.exiting()) {
                continue;
            }
            self.toasts.retain(|t| t.n.id != n.id);
            self.toasts.push(Toast::new(n.clone()));
        }
        self.seen_seq = self.seen_seq.max(st.last_seq);
        self.settle(now);
    }

    /// Ids of the cards to draw, top to bottom (newest first). Exiting cards stay until
    /// their fade is over; waiting ones are not included.
    pub fn display(&self) -> Vec<&Toast> {
        let mut shown: Vec<&Toast> = self.toasts.iter().filter(|t| self.is_shown(t)).collect();
        shown.sort_by_key(|t| std::cmp::Reverse(t.n.seq));
        shown
    }

    /// Notifications waiting for a free slot.
    pub fn queued(&self) -> usize {
        self.toasts.iter().filter(|t| !self.is_shown(t)).count()
    }

    fn is_shown(&self, t: &Toast) -> bool {
        if t.exiting() {
            return true;
        }
        let exiting = self.toasts.iter().filter(|t| t.exiting()).count();
        let slots = MAX_VISIBLE.saturating_sub(exiting);
        // Critical first, then arrival order.
        let mut waiting: Vec<&Toast> = self.toasts.iter().filter(|t| !t.exiting()).collect();
        waiting.sort_by_key(|t| (t.n.urgency != Urgency::Critical, t.n.seq));
        waiting.iter().take(slots).any(|w| w.n.id == t.n.id)
    }

    /// Start timers of cards that just became visible, expire / remove what is due.
    /// Returns the ids whose time ran out (tell the service: `Handle::expire`).
    pub fn tick(&mut self, now: Instant) -> Vec<u32> {
        let mut expired = Vec::new();
        for t in &mut self.toasts {
            if !t.exiting() && t.running_since.is_some() && t.remaining_at(now) == Some(Duration::ZERO) {
                t.exiting_since = Some(now);
                expired.push(t.n.id);
            }
        }
        self.toasts.retain(|t| t.exiting_since.is_none_or(|s| now.saturating_duration_since(s) < EXIT));
        self.settle(now);
        expired
    }

    /// Hover pauses the timer; leaving resumes it with the time that was left.
    pub fn set_hover(&mut self, id: u32, hovered: bool, now: Instant) {
        if let Some(t) = self.toasts.iter_mut().find(|t| t.n.id == id && !t.exiting()) {
            if t.hovered == hovered {
                return;
            }
            t.hovered = hovered;
            if hovered {
                t.remaining = t.remaining_at(now);
                t.running_since = None;
            }
        }
        self.settle(now);
    }

    pub fn is_hovered(&self, id: u32) -> bool {
        self.toasts.iter().any(|t| t.n.id == id && t.hovered)
    }

    /// The pointer left the surface: nothing is hovered any more. Returns whether it changed.
    pub fn clear_hover(&mut self, now: Instant) -> bool {
        let any = self.toasts.iter().any(|t| t.hovered);
        if any {
            for t in &mut self.toasts {
                t.hovered = false;
            }
            self.settle(now);
        }
        any
    }

    /// Fade a card out now (the user closed it / acted on it).
    pub fn begin_exit(&mut self, id: u32, now: Instant) {
        if let Some(t) = self.toasts.iter_mut().find(|t| t.n.id == id && !t.exiting()) {
            t.exiting_since = Some(now);
        }
        self.settle(now);
    }

    /// Start the timers that should run.
    fn settle(&mut self, now: Instant) {
        let shown: Vec<u32> = self.toasts.iter().filter(|t| self.is_shown(t) && !t.exiting()).map(|t| t.n.id).collect();
        for t in &mut self.toasts {
            let run = shown.contains(&t.n.id) && !t.hovered && t.total.is_some() && !t.exiting();
            if run && t.running_since.is_none() {
                t.running_since = Some(now);
                t.generation += 1;
            } else if !run && t.running_since.is_some() {
                t.remaining = t.remaining_at(now);
                t.running_since = None;
            }
        }
    }

    /// When [`Queue::tick`] next has work to do.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.toasts
            .iter()
            .filter_map(|t| match t.exiting_since {
                Some(s) => Some(s + EXIT),
                None => t.running_since.map(|_| now + t.remaining_at(now).unwrap_or_default()),
            })
            .min()
    }
}

/// "now", "5m", "2h", "3d" (the age label of a card).
pub fn age_label(received: chrono::DateTime<chrono::Local>, now: chrono::DateTime<chrono::Local>) -> String {
    let secs = (now - received).num_seconds().max(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use ashd_svc_notifications::{Action, Notification};

    use super::*;

    fn notif(id: u32, seq: u64, urgency: Urgency) -> Notification {
        Notification {
            id,
            seq,
            app_name: "app".into(),
            summary: format!("n{id}"),
            body: String::new(),
            actions: vec![Action { key: "default".into(), label: "Open".into() }],
            urgency,
            image: None,
            category: None,
            desktop_entry: None,
            expire_timeout_ms: None,
            resident: false,
            transient: false,
            progress: None,
            reply_placeholder: None,
            expired: false,
            sync_tag: None,
            received: chrono::Local::now(),
        }
    }

    fn state(ns: Vec<Notification>, dnd: bool) -> NotificationsState {
        let last_seq = ns.iter().map(|n| n.seq).max().unwrap_or(0);
        NotificationsState { available: true, notifications: ns, dnd, last_seq }
    }

    fn ids(q: &Queue) -> Vec<u32> {
        q.display().iter().map(|t| t.n.id).collect()
    }

    #[test]
    fn new_notifications_pop_newest_on_top_and_start_their_timer() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal), notif(2, 2, Urgency::Normal)], false), t0);
        assert_eq!(ids(&q), [2, 1]);
        assert!(q.toasts.iter().all(|t| t.running_since == Some(t0) && t.total == Some(Duration::from_secs(6))));
        assert_eq!(q.next_deadline(t0), Some(t0 + Duration::from_secs(6)));
    }

    #[test]
    fn already_seen_notifications_do_not_pop_again() {
        let t0 = Instant::now();
        let mut q = Queue::new(5);
        q.sync(&state(vec![notif(1, 3, Urgency::Normal), notif(2, 6, Urgency::Normal)], false), t0);
        assert_eq!(ids(&q), [2]);
    }

    #[test]
    fn excess_toasts_wait_and_only_then_start_their_timers() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        let ns: Vec<_> = (1..=6).map(|i| notif(i, i as u64, Urgency::Normal)).collect();
        q.sync(&state(ns, false), t0);
        assert_eq!(ids(&q), [4, 3, 2, 1]);
        assert_eq!(q.queued(), 2);
        assert!(q.toasts.iter().filter(|t| t.running_since.is_none()).count() == 2);
        // the oldest times out: it fades, the next one takes its slot once the fade is over
        let t1 = t0 + Duration::from_secs(6);
        assert_eq!(q.tick(t1), [1, 2, 3, 4]);
        assert_eq!(q.queued(), 2);
        let t2 = t1 + EXIT;
        assert!(q.tick(t2).is_empty());
        assert_eq!(ids(&q), [6, 5]);
        let five = q.toasts.iter().find(|t| t.n.id == 5).unwrap();
        assert_eq!(five.running_since, Some(t2));
    }

    #[test]
    fn critical_jumps_the_queue_and_has_no_timer() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        let mut ns: Vec<_> = (1..=4).map(|i| notif(i, i as u64, Urgency::Normal)).collect();
        q.sync(&state(ns.clone(), false), t0);
        ns.push(notif(5, 5, Urgency::Critical));
        q.sync(&state(ns, false), t0);
        assert_eq!(ids(&q), [5, 3, 2, 1]);
        let crit = q.toasts.iter().find(|t| t.n.id == 5).unwrap();
        assert_eq!((crit.total, crit.running_since), (None, None));
        assert_eq!(q.queued(), 1);
    }

    #[test]
    fn hover_pauses_and_resumes_with_the_time_left() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal)], false), t0);
        q.set_hover(1, true, t0 + Duration::from_secs(2));
        let t = &q.toasts[0];
        assert_eq!(t.running_since, None);
        assert_eq!(t.remaining, Some(Duration::from_secs(4)));
        assert!(q.tick(t0 + Duration::from_secs(60)).is_empty());
        let t1 = t0 + Duration::from_secs(60);
        q.set_hover(1, false, t1);
        assert_eq!(q.next_deadline(t1), Some(t1 + Duration::from_secs(4)));
        assert_eq!(q.tick(t1 + Duration::from_secs(4)), [1]);
    }

    #[test]
    fn pointer_leaving_the_surface_resumes_every_timer() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal), notif(2, 2, Urgency::Normal)], false), t0);
        q.set_hover(2, true, t0 + Duration::from_secs(1));
        assert!(q.is_hovered(2) && !q.is_hovered(1));
        let t1 = t0 + Duration::from_secs(3);
        assert!(q.clear_hover(t1));
        assert!(!q.clear_hover(t1));
        let two = q.toasts.iter().find(|t| t.n.id == 2).unwrap();
        assert_eq!((two.running_since, two.remaining), (Some(t1), Some(Duration::from_secs(5))));
        // the other card has 3 s left, the resumed one 5 s
        assert_eq!(q.next_deadline(t1), Some(t1 + Duration::from_secs(3)));
    }

    #[test]
    fn replace_restarts_the_timer_in_place() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal)], false), t0);
        let t1 = t0 + Duration::from_secs(4);
        q.sync(&state(vec![notif(1, 2, Urgency::Normal)], false), t1);
        assert_eq!(q.toasts.len(), 1);
        assert_eq!(q.toasts[0].n.seq, 2);
        assert_eq!(q.next_deadline(t1), Some(t1 + Duration::from_secs(6)));
    }

    #[test]
    fn removal_expiry_and_dnd_make_cards_leave() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal), notif(2, 2, Urgency::Critical)], false), t0);
        // dismissed elsewhere
        q.sync(&state(vec![notif(2, 2, Urgency::Critical)], false), t0);
        assert!(q.toasts.iter().find(|t| t.n.id == 1).unwrap().exiting());
        // dnd keeps only critical ones
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal), notif(2, 2, Urgency::Critical)], false), t0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal), notif(2, 2, Urgency::Critical)], true), t0);
        assert!(q.toasts.iter().find(|t| t.n.id == 1).unwrap().exiting());
        assert!(!q.toasts.iter().find(|t| t.n.id == 2).unwrap().exiting());
        q.tick(t0 + EXIT);
        assert_eq!(ids(&q), [2]);
        // marked expired by the service
        let mut gone = notif(2, 2, Urgency::Critical);
        gone.expired = true;
        q.sync(&state(vec![gone], true), t0);
        q.tick(t0 + EXIT * 2);
        assert!(q.is_empty());
    }

    #[test]
    fn dnd_notifications_never_pop_later() {
        let t0 = Instant::now();
        let mut q = Queue::new(0);
        q.sync(&state(vec![notif(1, 1, Urgency::Normal)], true), t0);
        assert!(q.is_empty());
        q.sync(&state(vec![notif(1, 1, Urgency::Normal)], false), t0);
        assert!(q.is_empty());
    }

    #[test]
    fn age_labels() {
        let now = chrono::Local::now();
        let ago = |s: i64| now - chrono::Duration::seconds(s);
        assert_eq!(age_label(ago(5), now), "now");
        assert_eq!(age_label(ago(65), now), "1m");
        assert_eq!(age_label(ago(2 * 3600 + 5), now), "2h");
        assert_eq!(age_label(ago(3 * 86400), now), "3d");
        assert_eq!(age_label(now + chrono::Duration::seconds(30), now), "now");
    }
}
