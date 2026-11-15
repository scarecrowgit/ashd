//! When the minimap is shown: the idle-hide state machine. Plain data and `Instant`s, no
//! GPUI, so it is unit-tested without a display.
//!
//! The minimap is visible while the view is changing and for [`HOLD`] afterwards, then fades
//! out over [`FADE_OUT_MS`]; any new change fades it back in over [`FADE_IN_MS`]. It stays
//! while the pointer is over it (or while a drag is in progress). The caller:
//!
//! 1. calls [`Visibility::poke`] on every change (view, windows, workspace switch),
//!    [`Visibility::set_hovered`] / [`Visibility::set_held`] on pointer state, and notifies
//!    its view;
//! 2. calls [`Visibility::frame`] while rendering and draws at `opacity`; while `animating`
//!    it asks GPUI for the next frame, otherwise it arms one timer for `wake_at`.
//!
//! Once fully hidden `frame` reports neither: nothing re-renders until the next change.

use std::time::{Duration, Instant};

/// How long the minimap stays fully visible after the last change.
pub const HOLD: Duration = Duration::from_millis(1500);
/// Fade-out duration (full opacity to hidden).
pub const FADE_OUT_MS: u64 = 250;
/// Fade-in duration (hidden to full opacity); a partial fade takes proportionally less.
pub const FADE_IN_MS: u64 = 120;
/// Below this opacity the card is treated as gone: it takes no input (empty input region).
pub const INTERACTIVE_MIN: f32 = 0.3;

/// The three durations of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    pub hold: Duration,
    pub fade_in: Duration,
    pub fade_out: Duration,
}

impl Timings {
    /// The constants above.
    pub const DEFAULT: Timings = Timings {
        hold: HOLD,
        fade_in: Duration::from_millis(FADE_IN_MS),
        fade_out: Duration::from_millis(FADE_OUT_MS),
    };

    /// [`Self::DEFAULT`] with the fades shortened to the shell's reduce-motion fade when
    /// that switch is on (`ashd_ui::anim::duration_ms`); the hold is unchanged.
    pub fn current() -> Timings {
        Timings {
            hold: HOLD,
            fade_in: Duration::from_millis(ashd_ui::anim::duration_ms(FADE_IN_MS)),
            fade_out: Duration::from_millis(ashd_ui::anim::duration_ms(FADE_OUT_MS)),
        }
    }
}

/// What to draw and what to schedule at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// 0.0 hidden .. 1.0 fully visible.
    pub opacity: f32,
    /// The opacity is still moving: request the next frame.
    pub animating: bool,
    /// Fully visible and idle: the instant to render again (the hold runs out). `None`
    /// when animating, hidden, or held visible by hover / drag.
    pub wake_at: Option<Instant>,
}

impl Frame {
    /// Draw the card at all.
    pub fn drawn(&self) -> bool {
        self.opacity > 0.0
    }

    /// The card takes pointer input (it is in the input region).
    pub fn interactive(&self) -> bool {
        self.opacity >= INTERACTIVE_MIN
    }
}

#[derive(Clone, Copy, Debug)]
struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
}

impl Tween {
    fn value(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return self.to;
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32()).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * t
    }

    fn done(&self, now: Instant) -> bool {
        self.dur.is_zero() || now.saturating_duration_since(self.start) >= self.dur
    }
}

#[derive(Clone, Debug)]
pub struct Visibility {
    timings: Timings,
    last_change: Option<Instant>,
    hovered: bool,
    held: bool,
    tween: Tween,
}

impl Visibility {
    /// Starts hidden: the first [`Self::poke`] shows it.
    pub fn new(timings: Timings, now: Instant) -> Self {
        Self { timings, last_change: None, hovered: false, held: false, tween: Tween { from: 0.0, to: 0.0, start: now, dur: Duration::ZERO } }
    }

    pub fn set_timings(&mut self, timings: Timings) {
        self.timings = timings;
    }

    /// Something changed (the view moved, a window opened, the workspace was switched to).
    pub fn poke(&mut self, now: Instant) {
        self.frame(now);
        self.bump(now);
        // Retarget at the event, not at whenever the next frame happens to be rendered.
        self.frame(now);
    }

    /// Record a change time; never moves it back.
    fn bump(&mut self, now: Instant) {
        self.last_change = Some(self.last_change.map_or(now, |t| t.max(now)));
    }

    /// The pointer entered / left the card. Leaving starts a new hold.
    pub fn set_hovered(&mut self, on: bool, now: Instant) {
        self.frame(now);
        if self.hovered && !on {
            self.bump(now);
        }
        self.hovered = on;
        self.frame(now);
    }

    /// Keep the card shown regardless of the hold (a drag in progress). Releasing starts a
    /// new hold.
    pub fn set_held(&mut self, on: bool, now: Instant) {
        self.frame(now);
        if self.held && !on {
            self.bump(now);
        }
        self.held = on;
        self.frame(now);
    }

    fn wanted(&self, now: Instant) -> bool {
        self.hovered || self.held || self.last_change.is_some_and(|t| now < t + self.timings.hold)
    }

    /// The opacity at `now` without retargeting (a pure read).
    pub fn opacity(&self, now: Instant) -> f32 {
        self.tween.value(now)
    }

    /// Advance to `now`: retarget the fade if the wanted state flipped, and report what to
    /// draw and schedule. Call once per render.
    pub fn frame(&mut self, now: Instant) -> Frame {
        let want = self.wanted(now);
        let to = if want { 1.0 } else { 0.0 };
        if self.tween.to != to {
            // A fade-out begins when the hold ran out, not when a (possibly late) frame
            // noticed; every other retarget begins now.
            let start = match self.last_change {
                Some(t) if !want => (t + self.timings.hold).min(now),
                _ => now,
            };
            let from = self.tween.value(start);
            let full = if want { self.timings.fade_in } else { self.timings.fade_out };
            // A fade that is part-way takes only the rest of its time, so a quick change
            // right after the start of a fade-out does not wait a full fade-in.
            let dur = full.mul_f32((to - from).abs());
            self.tween = Tween { from, to, start, dur };
        }
        let opacity = self.tween.value(now);
        let animating = !self.tween.done(now);
        let wake_at = if !animating && want && !self.hovered && !self.held {
            self.last_change.map(|t| t + self.timings.hold)
        } else {
            None
        };
        Frame { opacity: if animating { opacity } else { self.tween.to }, animating, wake_at }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timings = Timings::DEFAULT;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// Hidden machine at `t0`, poked at `t0`, rendered once.
    fn shown() -> (Visibility, Instant) {
        let t0 = Instant::now();
        let mut v = Visibility::new(T, t0);
        v.poke(t0);
        v.frame(t0);
        (v, t0)
    }

    #[test]
    fn starts_hidden_and_idle() {
        let t0 = Instant::now();
        let mut v = Visibility::new(T, t0);
        let f = v.frame(t0 + ms(10_000));
        assert_eq!(f, Frame { opacity: 0.0, animating: false, wake_at: None });
        assert!(!f.drawn() && !f.interactive());
    }

    #[test]
    fn a_change_fades_in_then_holds_then_fades_out() {
        let (mut v, t0) = shown();
        // Fade in over FADE_IN_MS.
        let f = v.frame(t0 + ms(60));
        assert!(f.animating && near(f.opacity, 0.5), "{f:?}");
        assert_eq!(f.wake_at, None, "animating: frames come from the animation, no timer");
        let f = v.frame(t0 + ms(120));
        assert_eq!(f.opacity, 1.0);
        assert!(!f.animating);
        // Fully visible: one wake-up at the end of the hold, nothing before.
        assert_eq!(f.wake_at, Some(t0 + HOLD));
        let f = v.frame(t0 + ms(1499));
        assert_eq!((f.opacity, f.animating, f.wake_at), (1.0, false, Some(t0 + HOLD)));
        // The hold is over: fade out over FADE_OUT_MS.
        let f = v.frame(t0 + HOLD);
        assert!(f.animating && near(f.opacity, 1.0), "{f:?}");
        let f = v.frame(t0 + HOLD + ms(125));
        assert!(f.animating && near(f.opacity, 0.5), "{f:?}");
        let f = v.frame(t0 + HOLD + ms(FADE_OUT_MS));
        assert_eq!(f, Frame { opacity: 0.0, animating: false, wake_at: None }, "fully hidden: no frames, no timer");
        // And it stays that way.
        assert_eq!(v.frame(t0 + ms(60_000)), Frame { opacity: 0.0, animating: false, wake_at: None });
    }

    #[test]
    fn a_change_while_visible_extends_the_hold() {
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        v.poke(t0 + ms(1400));
        let f = v.frame(t0 + ms(1600)); // past the first hold, inside the second
        assert_eq!((f.opacity, f.animating), (1.0, false));
        assert_eq!(f.wake_at, Some(t0 + ms(1400) + HOLD));
        let f = v.frame(t0 + ms(2899));
        assert_eq!((f.opacity, f.animating), (1.0, false));
        assert!(v.frame(t0 + ms(3000)).animating, "fades out only at 1400 + 1500");
    }

    #[test]
    fn a_change_during_fade_out_returns_from_where_it_was() {
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        let start = t0 + HOLD;
        let f = v.frame(start + ms(125));
        assert!(near(f.opacity, 0.5));
        v.poke(start + ms(125));
        // Retargets to visible from 0.5: half of the fade-in time, not from zero.
        let f = v.frame(start + ms(125));
        assert!(f.animating && near(f.opacity, 0.5), "continuous at the turn: {f:?}");
        let f = v.frame(start + ms(125) + ms(30));
        assert!(near(f.opacity, 0.75), "{f:?}");
        let f = v.frame(start + ms(125) + ms(60));
        assert_eq!((f.opacity, f.animating), (1.0, false));
        assert_eq!(f.wake_at, Some(start + ms(125) + HOLD));
    }

    #[test]
    fn a_late_frame_resumes_the_fade_where_it_should_be() {
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        // Nothing rendered between the end of the hold and 100 ms into the fade-out.
        let f = v.frame(t0 + HOLD + ms(100));
        assert!(f.animating && near(f.opacity, 0.6), "{f:?}");
        // Nothing rendered for a minute: it is simply gone.
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        assert_eq!(v.frame(t0 + ms(60_000)), Frame { opacity: 0.0, animating: false, wake_at: None });
    }

    #[test]
    fn rapid_changes_stay_visible_and_never_flicker() {
        let (mut v, t0) = shown();
        let mut t = t0;
        let mut last = 0.0;
        for _ in 0..200 {
            t += ms(16);
            v.poke(t);
            let f = v.frame(t);
            assert!(f.opacity >= last - 1e-6, "no dip while changes keep coming");
            last = f.opacity;
        }
        let f = v.frame(t + ms(1));
        assert_eq!((f.opacity, f.animating), (1.0, false));
        // Only one hold after the last change.
        assert_eq!(f.wake_at, Some(t + HOLD));
        assert!(v.frame(t + HOLD + ms(FADE_OUT_MS)).opacity == 0.0);
    }

    #[test]
    fn changes_at_the_same_instant_collapse() {
        let (mut v, t0) = shown();
        v.poke(t0 + ms(500));
        v.poke(t0 + ms(500));
        // An out-of-order (older) poke never shortens the hold.
        v.poke(t0 + ms(100));
        assert_eq!(v.frame(t0 + ms(600)).wake_at, Some(t0 + ms(500) + HOLD));
    }

    #[test]
    fn hover_keeps_it_visible_and_leaving_starts_a_new_hold() {
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        v.set_hovered(true, t0 + ms(300));
        // Long past the hold: still fully visible, and nothing to schedule.
        let f = v.frame(t0 + ms(10_000));
        assert_eq!(f, Frame { opacity: 1.0, animating: false, wake_at: None });
        v.set_hovered(false, t0 + ms(10_000));
        let f = v.frame(t0 + ms(10_000));
        assert_eq!((f.opacity, f.animating), (1.0, false));
        assert_eq!(f.wake_at, Some(t0 + ms(10_000) + HOLD));
        let f = v.frame(t0 + ms(10_000) + HOLD + ms(FADE_OUT_MS));
        assert_eq!(f.opacity, 0.0);
    }

    #[test]
    fn hover_during_the_fade_out_brings_it_back() {
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        let t = t0 + HOLD + ms(100); // fading out, 0.6 left
        let f = v.frame(t);
        assert!(f.animating && f.opacity < 1.0);
        v.set_hovered(true, t);
        let f = v.frame(t + ms(200));
        assert_eq!((f.opacity, f.animating), (1.0, false));
        assert_eq!(f.wake_at, None);
    }

    #[test]
    fn events_retarget_at_the_event_not_at_the_next_frame() {
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        // Hover arrives 100 ms into the fade-out (0.6 left) but is only rendered much later.
        v.set_hovered(true, t0 + HOLD + ms(100));
        let f = v.frame(t0 + HOLD + ms(5000));
        assert_eq!((f.opacity, f.animating), (1.0, false), "never went fully transparent");
        // Same for a change.
        let (mut v, t0) = shown();
        v.frame(t0 + ms(200));
        v.poke(t0 + HOLD + ms(100));
        assert!(v.opacity(t0 + HOLD + ms(100)) > 0.5, "the fade-out is cut at the event: {}", v.opacity(t0 + HOLD + ms(100)));
        assert!(v.frame(t0 + HOLD + ms(600)).opacity == 1.0);
    }

    #[test]
    fn hovering_a_hidden_card_is_a_no_op_leave() {
        // A stray leave without an enter must not show anything.
        let t0 = Instant::now();
        let mut v = Visibility::new(T, t0);
        v.set_hovered(false, t0);
        assert_eq!(v.frame(t0).opacity, 0.0);
    }

    #[test]
    fn a_drag_holds_it_like_hover() {
        let (mut v, t0) = shown();
        v.set_held(true, t0);
        assert_eq!(v.frame(t0 + ms(30_000)), Frame { opacity: 1.0, animating: false, wake_at: None });
        v.set_held(false, t0 + ms(30_000));
        assert_eq!(v.frame(t0 + ms(30_000)).wake_at, Some(t0 + ms(30_000) + HOLD));
    }

    #[test]
    fn input_follows_the_opacity() {
        let (mut v, t0) = shown();
        assert!(!v.frame(t0).interactive(), "just poked: still transparent, no input yet");
        assert!(v.frame(t0 + ms(60)).interactive());
        v.frame(t0 + ms(200));
        // Fading out: input goes away before the card is gone.
        assert!(v.frame(t0 + HOLD + ms(100)).interactive());
        let f = v.frame(t0 + HOLD + ms(200));
        assert!(!f.interactive() && f.drawn());
        assert!(!v.frame(t0 + HOLD + ms(FADE_OUT_MS)).drawn());
    }

    #[test]
    fn reduced_motion_shortens_only_the_fades() {
        let r = Timings { fade_in: ms(80), fade_out: ms(80), ..T };
        let t0 = Instant::now();
        let mut v = Visibility::new(r, t0);
        v.poke(t0);
        v.frame(t0);
        assert_eq!(v.frame(t0 + ms(80)).opacity, 1.0);
        assert_eq!(v.frame(t0 + HOLD).wake_at, None);
        assert_eq!(v.frame(t0 + HOLD + ms(80)).opacity, 0.0);
        // Zero-length fades (never configured, but the machine must not divide by zero).
        let z = Timings { fade_in: Duration::ZERO, fade_out: Duration::ZERO, hold: ms(10) };
        let mut v = Visibility::new(z, t0);
        v.poke(t0);
        assert_eq!(v.frame(t0), Frame { opacity: 1.0, animating: false, wake_at: Some(t0 + ms(10)) });
        assert_eq!(v.frame(t0 + ms(10)), Frame { opacity: 0.0, animating: false, wake_at: None });
    }

    #[test]
    fn the_constants_are_the_specified_ones() {
        assert_eq!(HOLD, ms(1500));
        assert_eq!((FADE_IN_MS, FADE_OUT_MS), (120, 250));
        assert_eq!(Timings::DEFAULT.hold, HOLD);
    }
}
