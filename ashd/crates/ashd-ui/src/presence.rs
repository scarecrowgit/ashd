//! Time-based state machines for things that appear, change and leave. Plain data (no GPUI,
//! `Instant` passed in), so the transitions are unit-tested without a window.
//!
//! - [`Tween`]: a scalar moving to a retargetable value on a cubic-bezier curve. Retargeting
//!   mid-flight starts from the current *value* (no jump); a leg that covers only part of
//!   the distance takes proportionally less time.
//! - [`Presence`]: a [`Tween`] between 0 (absent) and 1 (present) with separate enter / exit
//!   timings: the model of every popup, overlay, row and badge. Closing keeps the thing
//!   alive until [`Presence::is_gone`]; re-opening while it leaves reverses from where it
//!   is (no restart, no pop).
//! - [`PresenceList`]: a keyed list whose removed items stay (leaving) until their exit
//!   animation is over, and whose new items enter.
//!
//! Everything is clock-driven: nothing depends on how many frames were drawn, so the
//! motion is the same at 60 and 180 Hz. Reduced motion ([`crate::anim::set_reduced_motion`])
//! shortens every leg to the 80 ms fade; use [`Presence::movement`] for position / scale so
//! those jump to the end state.

use std::time::{Duration, Instant};

use ashd_tokens::motion::Bezier;

use crate::anim;

/// Duration and curve of one direction of a transition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub ms: u64,
    pub curve: Bezier,
}

impl Timing {
    pub const fn new(ms: u64, curve: Bezier) -> Self {
        Self { ms, curve }
    }
}

/// A scalar easing towards a target.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    /// Effective duration of the current leg (reduced motion and the distance applied).
    dur: Duration,
    curve: Bezier,
}

impl Tween {
    /// Settled at `value`.
    pub fn new(value: f32, now: Instant) -> Self {
        Self { from: value, to: value, start: now, dur: Duration::ZERO, curve: Bezier::LINEAR }
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    /// Jump to `value`, no animation.
    pub fn snap(&mut self, value: f32, now: Instant) {
        *self = Self::new(value, now);
    }

    /// Ease to `to` with `timing`. `full` is the distance a full-length leg covers (1 for
    /// presence, the value range for a meter): shorter legs are shorter in time. A no-op
    /// when `to` is already the target.
    pub fn go(&mut self, to: f32, timing: Timing, full: f32, now: Instant) {
        if to == self.to {
            return;
        }
        let here = self.value(now);
        let frac = if full > 0.0 { ((to - here).abs() / full).clamp(0.0, 1.0) } else { 1.0 };
        let ms = (anim::duration_ms(timing.ms) as f32 * frac).round() as u64;
        self.from = here;
        self.to = to;
        self.start = now;
        self.dur = Duration::from_millis(ms);
        self.curve = timing.curve;
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32();
        if t >= 1.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * self.curve.eval(t)
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        !self.dur.is_zero() && now.saturating_duration_since(self.start) < self.dur
    }

    /// Time left on the current leg.
    pub fn remaining(&self, now: Instant) -> Duration {
        self.dur.saturating_sub(now.saturating_duration_since(self.start))
    }
}

/// A thing's presence: 0 = not there, 1 = fully there.
#[derive(Clone, Copy, Debug)]
pub struct Presence {
    tween: Tween,
    enter: Timing,
    exit: Timing,
}

impl Presence {
    /// Fully present, no animation (the thing was there before we looked).
    pub fn present(enter: Timing, exit: Timing, now: Instant) -> Self {
        Self { tween: Tween::new(1.0, now), enter, exit }
    }

    /// Absent; call [`Self::set_present`] to bring it in.
    pub fn absent(enter: Timing, exit: Timing, now: Instant) -> Self {
        Self { tween: Tween::new(0.0, now), enter, exit }
    }

    /// Starts entering right now.
    pub fn entering(enter: Timing, exit: Timing, now: Instant) -> Self {
        let mut p = Self::absent(enter, exit, now);
        p.set_present(true, now);
        p
    }

    /// Heading to (or at) present.
    pub fn is_present(&self) -> bool {
        self.tween.target() >= 1.0
    }

    pub fn set_present(&mut self, present: bool, now: Instant) {
        if present {
            self.tween.go(1.0, self.enter, 1.0, now);
        } else {
            self.tween.go(0.0, self.exit, 1.0, now);
        }
    }

    /// Opacity-like progress in 0..=1 (curve of the direction it is heading).
    pub fn value(&self, now: Instant) -> f32 {
        self.tween.value(now)
    }

    /// Progress for *movement* (offset, scale): under reduced motion the end state at once.
    pub fn movement(&self, now: Instant) -> f32 {
        if anim::reduced_motion() { self.tween.target() } else { self.tween.value(now) }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.tween.is_animating(now)
    }

    /// Left and settled at 0: safe to destroy.
    pub fn is_gone(&self, now: Instant) -> bool {
        !self.is_present() && !self.is_animating(now)
    }

    /// Time until this settles (for the timer that destroys a surface).
    pub fn remaining(&self, now: Instant) -> Duration {
        self.tween.remaining(now)
    }
}

/// One entry of a [`PresenceList`].
#[derive(Clone, Debug)]
pub struct Entry<K, T> {
    pub key: K,
    pub value: T,
    pub presence: Presence,
}

impl<K, T> Entry<K, T> {
    /// Still wanted by the data (not leaving).
    pub fn is_present(&self) -> bool {
        self.presence.is_present()
    }
}

/// A keyed list that animates additions and removals: removed items stay, with their last
/// value, until their exit animation is over.
#[derive(Clone, Debug)]
pub struct PresenceList<K, T> {
    entries: Vec<Entry<K, T>>,
    enter: Timing,
    exit: Timing,
    primed: bool,
}

impl<K: Clone + PartialEq, T: Clone> PresenceList<K, T> {
    pub fn new(enter: Timing, exit: Timing) -> Self {
        Self { entries: Vec::new(), enter, exit, primed: false }
    }

    /// Animate what the very first [`Self::sync`] finds too (a list that only exists because
    /// its first item just arrived: the HUD's passive surface).
    pub fn animate_first(mut self) -> Self {
        self.primed = true;
        self
    }

    /// Reconcile with the data's current items (in order). The first call only fills the
    /// list (what was already there does not animate in); afterwards new keys enter,
    /// missing keys leave (keeping their place: right after the entry that preceded them)
    /// and known keys get their value replaced. A key that comes back while leaving
    /// re-enters from where it is.
    pub fn sync(&mut self, now: Instant, current: impl IntoIterator<Item = (K, T)>) {
        let animate = self.primed;
        self.primed = true;
        let old_order: Vec<K> = self.entries.iter().map(|e| e.key.clone()).collect();
        let mut old = std::mem::take(&mut self.entries);
        let mut out: Vec<Entry<K, T>> = Vec::with_capacity(old.len());
        for (key, value) in current {
            let mut entry = match old.iter().position(|e| e.key == key) {
                Some(i) => old.remove(i),
                None => {
                    let presence = if animate {
                        Presence::entering(self.enter, self.exit, now)
                    } else {
                        Presence::present(self.enter, self.exit, now)
                    };
                    Entry { key, value: value.clone(), presence }
                }
            };
            entry.value = value;
            if !entry.is_present() {
                entry.presence.set_present(true, now);
            }
            out.push(entry);
        }
        // `old` now holds what left the data, in old order.
        for mut gone in old {
            if gone.is_present() {
                gone.presence.set_present(false, now);
            }
            if gone.presence.is_gone(now) {
                continue;
            }
            let ix = old_order.iter().position(|k| *k == gone.key).unwrap_or(0);
            let anchor = old_order[..ix].iter().rev().find_map(|k| out.iter().position(|e| e.key == *k));
            out.insert(anchor.map_or(0, |a| a + 1), gone);
        }
        self.entries = out;
    }

    /// Entries to draw now (drops the ones whose exit is over).
    pub fn entries(&mut self, now: Instant) -> &[Entry<K, T>] {
        self.entries.retain(|e| !e.presence.is_gone(now));
        &self.entries
    }

    /// Anything entering or leaving: the view must keep asking for frames.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.entries.iter().any(|e| e.presence.is_animating(now))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries still wanted by the data.
    pub fn present_len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_present()).count()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use ashd_tokens::motion::{EASE_OUT_EXPO, EASE_OUT_QUAD};

    use super::*;

    /// Reduced motion is a process-global flag: tests that read durations run one at a time.
    fn guard() -> MutexGuard<'static, ()> {
        let g = crate::anim::test_support::guard();
        crate::anim::test_support::set_reduced(false);
        g
    }

    const IN: Timing = Timing::new(140, EASE_OUT_EXPO);
    const OUT: Timing = Timing::new(100, EASE_OUT_QUAD);

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn enter_runs_to_one_and_settles_exactly() {
        let _g = guard();
        let t0 = Instant::now();
        let p = Presence::entering(IN, OUT, t0);
        assert_eq!(p.value(t0), 0.0);
        let mid = p.value(ms(t0, 40));
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert!(p.is_animating(ms(t0, 100)));
        assert_eq!(p.value(ms(t0, 140)), 1.0);
        assert!(!p.is_animating(ms(t0, 140)));
        assert!(p.is_present() && !p.is_gone(ms(t0, 500)));
        // monotonic
        let mut prev = 0.0;
        for t in 0..=140 {
            let v = p.value(ms(t0, t));
            assert!(v >= prev - 1e-6, "{t} {v} {prev}");
            prev = v;
        }
    }

    #[test]
    fn exit_is_shorter_and_ends_gone() {
        let _g = guard();
        let t0 = Instant::now();
        let mut p = Presence::present(IN, OUT, t0);
        p.set_present(false, t0);
        assert!(!p.is_present());
        assert!(!p.is_gone(ms(t0, 50)), "still on screen while it leaves");
        assert_eq!(p.value(ms(t0, 100)), 0.0);
        assert!(p.is_gone(ms(t0, 100)));
        assert_eq!(p.remaining(t0), Duration::from_millis(100));
    }

    #[test]
    fn open_close_open_reverses_without_a_jump_and_never_sticks() {
        let _g = guard();
        let t0 = Instant::now();
        let mut p = Presence::entering(IN, OUT, t0);
        let at_close = p.value(ms(t0, 30));
        p.set_present(false, ms(t0, 30));
        // continuous at the reversal
        assert!((p.value(ms(t0, 30)) - at_close).abs() < 1e-6);
        let mid = p.value(ms(t0, 60));
        assert!(mid < at_close, "heads down");
        p.set_present(true, ms(t0, 60));
        assert!((p.value(ms(t0, 60)) - mid).abs() < 1e-6);
        // ends exactly present, not half-transparent
        assert_eq!(p.value(ms(t0, 60 + 140)), 1.0);
        assert!(!p.is_animating(ms(t0, 60 + 140)));
        // and the other way: closes fully
        p.set_present(false, ms(t0, 400));
        p.set_present(true, ms(t0, 420));
        p.set_present(false, ms(t0, 430));
        assert_eq!(p.value(ms(t0, 430 + 100)), 0.0);
        assert!(p.is_gone(ms(t0, 430 + 100)));
    }

    #[test]
    fn a_short_leg_takes_proportionally_less_time() {
        let _g = guard();
        let t0 = Instant::now();
        let p = Presence::entering(IN, OUT, t0);
        assert_eq!(p.value(ms(t0, 140)), 1.0);
        let mut p = Presence::present(IN, OUT, t0);
        p.tween.go(0.3, OUT, 1.0, t0); // now at 0.3 going nowhere fast
        p.tween.snap(0.3, t0);
        p.set_present(false, t0);
        assert_eq!(p.remaining(t0), Duration::from_millis(30));
    }

    #[test]
    fn movement_jumps_under_reduced_motion_but_opacity_still_fades() {
        let _g = guard();
        let t0 = Instant::now();
        crate::anim::test_support::set_reduced(true);
        let p = Presence::entering(IN, OUT, t0);
        assert_eq!(p.movement(t0), 1.0, "movement is at its end state at once");
        assert!(p.value(t0) < 1.0, "the fade remains");
        assert_eq!(p.value(ms(t0, 80)), 1.0, "and lasts 80 ms, not 140");
        let mut q = Presence::present(IN, OUT, t0);
        q.set_present(false, t0);
        assert_eq!(q.movement(t0), 0.0);
        assert!(q.is_gone(ms(t0, 80)));
        crate::anim::test_support::set_reduced(false);
    }

    #[test]
    fn tween_retargets_from_the_current_value() {
        let _g = guard();
        let t0 = Instant::now();
        let mut v = Tween::new(0.2, t0);
        let std = Timing::new(160, ashd_tokens::motion::EASE_STANDARD);
        v.go(0.8, std, 1.0, t0);
        let mid = v.value(ms(t0, 50));
        assert!(mid > 0.2 && mid < 0.8);
        v.go(0.4, std, 1.0, ms(t0, 50));
        assert!((v.value(ms(t0, 50)) - mid).abs() < 1e-6);
        assert_eq!(v.value(ms(t0, 50 + 160)), 0.4);
        // same target: nothing restarts
        let before = v.remaining(ms(t0, 100));
        v.go(0.4, std, 1.0, ms(t0, 100));
        assert_eq!(v.remaining(ms(t0, 100)), before);
    }

    fn keys(l: &mut PresenceList<u32, &'static str>, now: Instant) -> Vec<(u32, bool)> {
        l.entries(now).iter().map(|e| (e.key, e.is_present())).collect()
    }

    #[test]
    fn list_first_sync_does_not_animate_then_additions_enter_and_removals_leave_in_place() {
        let _g = guard();
        let t0 = Instant::now();
        let mut l: PresenceList<u32, &str> = PresenceList::new(IN, OUT);
        l.sync(t0, [(1, "a"), (2, "b"), (3, "c")]);
        assert!(!l.is_animating(t0), "initial content is just there");
        assert_eq!(l.entries(t0)[0].presence.value(t0), 1.0);

        // remove the middle one, add one at the end
        l.sync(ms(t0, 10), [(1, "a"), (3, "c"), (4, "d")]);
        assert!(l.is_animating(ms(t0, 10)));
        assert_eq!(keys(&mut l, ms(t0, 20)), vec![(1, true), (2, false), (3, true), (4, true)], "2 keeps its slot");
        let e = l.entries(ms(t0, 10)).iter().find(|e| e.key == 4).unwrap().presence;
        assert_eq!(e.value(ms(t0, 10)), 0.0, "new item enters from 0");
        // after its exit it is gone, the others settled
        assert_eq!(keys(&mut l, ms(t0, 10 + 200)), vec![(1, true), (3, true), (4, true)]);
        assert!(!l.is_animating(ms(t0, 10 + 200)));
    }

    #[test]
    fn list_can_animate_what_it_finds_first() {
        let _g = guard();
        let t0 = Instant::now();
        let mut l: PresenceList<u32, &str> = PresenceList::new(IN, OUT).animate_first();
        l.sync(t0, [(1, "a")]);
        assert!(l.is_animating(t0));
        assert_eq!(l.entries(t0)[0].presence.value(t0), 0.0);
    }

    #[test]
    fn list_item_that_returns_while_leaving_reenters_and_values_update() {
        let _g = guard();
        let t0 = Instant::now();
        let mut l: PresenceList<u32, &str> = PresenceList::new(IN, OUT);
        l.sync(t0, [(1, "a"), (2, "b")]);
        l.sync(ms(t0, 10), [(1, "a")]);
        let mid = l.entries(ms(t0, 40)).iter().find(|e| e.key == 2).unwrap().presence.value(ms(t0, 40));
        assert!(mid < 1.0);
        l.sync(ms(t0, 40), [(1, "A"), (2, "B")]);
        let e = l.entries(ms(t0, 40)).iter().find(|e| e.key == 2).unwrap().clone();
        assert!(e.is_present());
        assert_eq!(e.value, "B");
        assert!((e.presence.value(ms(t0, 40)) - mid).abs() < 1e-6);
        assert_eq!(e.presence.value(ms(t0, 40 + 140)), 1.0);
        assert_eq!(l.present_len(), 2);
    }

    #[test]
    fn list_order_of_several_leaving_entries_is_stable() {
        let _g = guard();
        let t0 = Instant::now();
        let mut l: PresenceList<u32, &str> = PresenceList::new(IN, OUT);
        l.sync(t0, [(1, "a"), (2, "b"), (3, "c"), (4, "d")]);
        l.sync(ms(t0, 5), [(1, "a"), (4, "d")]);
        assert_eq!(keys(&mut l, ms(t0, 6)), vec![(1, true), (2, false), (3, false), (4, true)]);
        l.sync(ms(t0, 7), []);
        assert_eq!(keys(&mut l, ms(t0, 8)), vec![(1, false), (2, false), (3, false), (4, false)]);
        assert!(keys(&mut l, ms(t0, 8 + 100)).is_empty());
        assert!(l.is_empty());
    }
}
