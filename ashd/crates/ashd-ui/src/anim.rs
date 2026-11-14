//! Motion for GPUI views. "Anything spatial is a spring, anything that appears is a
//! tween" (TOKENS.md §9).
//!
//! Tweens: GPUI's `with_animation` + an easing from here:
//! ```ignore
//! el.with_animation("osd-in", tween(timing::OSD_IN, EASE_STANDARD), |el, t| el.opacity(t))
//! ```
//! Springs: keep a [`SpringValue`] in your state, retarget it, read it in render and keep
//! frames coming while it moves:
//! ```ignore
//! let x = self.indicator.value_now();
//! if !self.indicator.is_settled_now() { window.request_animation_frame(); }
//! ```

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use gpui::{Animation, App};
pub use ashd_tokens::motion::{
    self, Bezier, EASE_OUT_CUBIC, EASE_OUT_EXPO, EASE_OUT_QUAD, EASE_STANDARD, SPRING_CANVAS_PAN,
    SPRING_CANVAS_ZOOM, SPRING_SCRATCHPAD, SPRING_SHAKE, SPRING_VIEW, SPRING_WORKSPACE, Spring, timing,
};

pub fn ease_standard(t: f32) -> f32 {
    EASE_STANDARD.eval(t)
}
pub fn ease_out_expo(t: f32) -> f32 {
    EASE_OUT_EXPO.eval(t)
}
pub fn ease_out_quad(t: f32) -> f32 {
    EASE_OUT_QUAD.eval(t)
}
pub fn ease_out_cubic(t: f32) -> f32 {
    EASE_OUT_CUBIC.eval(t)
}

// ── reduced motion ────────────────────────────────────────────────────────────

static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Reduce motion (TOKENS.md §9): movement happens at once, only 80 ms fades remain.
/// Affects [`tween`], [`fade`], [`move_t`], [`SpringValue`] and the theme crossfade.
/// Initialised from `ASHD_REDUCE_MOTION=1` by [`init_from_env`] (called by `ashd_ui::init`).
pub fn set_reduced_motion(on: bool, cx: &mut App) {
    if REDUCED_MOTION.swap(on, Ordering::Relaxed) != on {
        cx.refresh_windows();
    }
}

/// Is the global reduced-motion switch on?
pub fn reduced_motion() -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed)
}

/// Read `ASHD_REDUCE_MOTION` (`1`/`true`/`yes`) into the switch.
pub fn init_from_env() {
    let on = std::env::var("ASHD_REDUCE_MOTION")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    REDUCED_MOTION.store(on, Ordering::Relaxed);
}

/// `ms`, or the 80 ms fade under reduced motion.
pub fn duration_ms(ms: u64) -> u64 {
    if reduced_motion() { timing::REDUCED_FADE.min(ms.max(1)) } else { ms }
}

/// Progress for *movement* (position, scale, size) driven by an animation's `t`: the real
/// `t` normally, the end state immediately under reduced motion. Opacity keeps using `t`.
pub fn move_t(t: f32) -> f32 {
    if reduced_motion() { 1.0 } else { t }
}

/// Linear interpolation helper for animator closures.
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

// ── tweens ────────────────────────────────────────────────────────────────────

/// A one-shot GPUI animation of `ms` milliseconds on `curve` (80 ms under reduced motion).
pub fn tween(ms: u64, curve: Bezier) -> Animation {
    Animation::new(Duration::from_millis(duration_ms(ms))).with_easing(move |t| curve.eval(t))
}

/// An appearing tween (opacity): same as [`tween`], named for intent.
pub fn fade(ms: u64, curve: Bezier) -> Animation {
    tween(ms, curve)
}

/// Hover / focus / flyout tween: `ms` on the standard curve.
pub fn tween_standard(ms: u64) -> Animation {
    tween(ms, EASE_STANDARD)
}

/// Window open / launcher: out-expo.
pub fn tween_enter(ms: u64) -> Animation {
    tween(ms, EASE_OUT_EXPO)
}

/// Close / dismiss: out-quad. Exits are about a third faster than entrances.
pub fn tween_exit(ms: u64) -> Animation {
    tween(ms, EASE_OUT_QUAD)
}

/// Scroll <-> Canvas, toasts: out-cubic.
pub fn tween_out_cubic(ms: u64) -> Animation {
    tween(ms, EASE_OUT_CUBIC)
}

/// Toast timer / progress: linear.
pub fn tween_linear(ms: u64) -> Animation {
    Animation::new(Duration::from_millis(duration_ms(ms)))
}

/// Wrong-password shake: horizontal offset (px) `t` seconds after the shake started, with
/// `amplitude` px of initial displacement, on the under-damped [`SPRING_SHAKE`]. Starts at
/// `amplitude`, swings through zero a few times and is below 0.5 px after
/// `SPRING_SHAKE.settle_time()`.
pub fn shake_offset(t: f32, amplitude: f32) -> f32 {
    if reduced_motion() {
        return 0.0;
    }
    amplitude * (1.0 - SPRING_SHAKE.value(t, 0.0))
}

/// A scalar animated by a spring. Retargeting mid-flight keeps the current velocity, so
/// motion stays continuous (niri-like).
#[derive(Clone, Copy, Debug)]
pub struct SpringValue {
    pub spring: Spring,
    from: f32,
    to: f32,
    v0: f32,
    start: Instant,
}

impl SpringValue {
    pub fn new(value: f32, spring: Spring) -> Self {
        Self { spring, from: value, to: value, v0: 0.0, start: Instant::now() }
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    /// Jump to `value` without animating.
    pub fn snap(&mut self, value: f32) {
        *self = Self::new(value, self.spring);
    }

    pub fn set_target(&mut self, to: f32) {
        self.set_target_at(to, Instant::now());
    }

    pub fn set_target_at(&mut self, to: f32, now: Instant) {
        if to == self.to {
            return;
        }
        if reduced_motion() {
            let spring = self.spring;
            *self = Self { spring, from: to, to, v0: 0.0, start: now };
            return;
        }
        let (x, v) = self.state_at(now);
        *self = Self { spring: self.spring, from: x, to, v0: v, start: now };
    }

    pub fn value_now(&self) -> f32 {
        self.value_at(Instant::now())
    }

    pub fn value_at(&self, now: Instant) -> f32 {
        self.state_at(now).0
    }

    pub fn is_settled_now(&self) -> bool {
        self.is_settled_at(Instant::now())
    }

    pub fn is_settled_at(&self, now: Instant) -> bool {
        let (x, v) = self.state_at(now);
        let span = (self.to - self.from).abs().max(1.0);
        (x - self.to).abs() <= span * motion::SPRING_EPSILON * 10.0 && v.abs() < span * 0.01
    }

    /// (position, velocity) at `now`.
    fn state_at(&self, now: Instant) -> (f32, f32) {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let d0 = self.from - self.to;
        if d0 == 0.0 && self.v0 == 0.0 {
            return (self.to, 0.0);
        }
        (self.to + displacement(self.spring, d0, self.v0, t), velocity(self.spring, d0, self.v0, t))
    }

    /// Current velocity in units per second.
    pub fn velocity_now(&self) -> f32 {
        self.state_at(Instant::now()).1
    }
}

/// Damped harmonic oscillator displacement from the target at time `t` (unit mass).
fn displacement(s: Spring, d0: f32, v0: f32, t: f32) -> f32 {
    let w = s.omega();
    let z = s.damping;
    if (z - 1.0).abs() < 1e-4 {
        (d0 + (v0 + w * d0) * t) * (-w * t).exp()
    } else if z < 1.0 {
        let wd = w * (1.0 - z * z).sqrt();
        let b = (v0 + z * w * d0) / wd;
        (-z * w * t).exp() * (d0 * (wd * t).cos() + b * (wd * t).sin())
    } else {
        let r = (z * z - 1.0).sqrt();
        let (r1, r2) = (-w * (z - r), -w * (z + r));
        let c2 = (v0 - r1 * d0) / (r2 - r1);
        (d0 - c2) * (r1 * t).exp() + c2 * (r2 * t).exp()
    }
}

/// d/dt of [`displacement`].
fn velocity(s: Spring, d0: f32, v0: f32, t: f32) -> f32 {
    let w = s.omega();
    let z = s.damping;
    if (z - 1.0).abs() < 1e-4 {
        let c = v0 + w * d0;
        (c - w * (d0 + c * t)) * (-w * t).exp()
    } else if z < 1.0 {
        let wd = w * (1.0 - z * z).sqrt();
        let b = (v0 + z * w * d0) / wd;
        let e = (-z * w * t).exp();
        let (sin, cos) = (wd * t).sin_cos();
        e * (-z * w * (d0 * cos + b * sin) + (-d0 * wd * sin + b * wd * cos))
    } else {
        let r = (z * z - 1.0).sqrt();
        let (r1, r2) = (-w * (z - r), -w * (z + r));
        let c2 = (v0 - r1 * d0) / (r2 - r1);
        (d0 - c2) * r1 * (r1 * t).exp() + c2 * r2 * (r2 * t).exp()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// The reduced-motion flag is process-global; tests that depend on it run one at a time.
    fn guard() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn spring_value_settles_and_retargets() {
        let _g = guard();
        let t0 = Instant::now();
        let mut s = SpringValue::new(0.0, SPRING_VIEW);
        s.set_target_at(100.0, t0);
        let mid = s.value_at(t0 + Duration::from_millis(50));
        assert!(mid > 0.0 && mid < 100.0);
        s.set_target_at(200.0, t0 + Duration::from_millis(50));
        let after = s.value_at(t0 + Duration::from_millis(60));
        assert!(after > mid, "velocity is kept across retargets");
        assert!(s.is_settled_at(t0 + Duration::from_secs(2)));
        assert!((s.value_at(t0 + Duration::from_secs(2)) - 200.0).abs() < 0.05);
    }

    fn sample(spring: Spring, from: f32, to: f32, ms: u64) -> f32 {
        let t0 = Instant::now();
        let mut s = SpringValue::new(from, spring);
        s.set_target_at(to, t0);
        s.value_at(t0 + Duration::from_millis(ms))
    }

    #[test]
    fn critically_damped_never_overshoots_and_settles() {
        let _g = guard();
        for spring in [SPRING_VIEW, SPRING_WORKSPACE, SPRING_CANVAS_ZOOM, SPRING_CANVAS_PAN, SPRING_SCRATCHPAD] {
            assert_eq!(spring.damping, 1.0);
            let mut prev = 0.0;
            for ms in 0..1500 {
                let v = sample(spring, 0.0, 100.0, ms);
                assert!(v <= 100.0 + 1e-3 && v >= prev - 1e-3, "{spring:?} {ms}ms {v}");
                prev = v;
            }
            let t0 = Instant::now();
            let mut s = SpringValue::new(0.0, spring);
            s.set_target_at(100.0, t0);
            assert!(!s.is_settled_at(t0 + Duration::from_millis(20)));
            assert!(s.is_settled_at(t0 + spring.settle_duration()));
            // niri view spring: ~ done within 0.3 s
            assert!(sample(SPRING_VIEW, 0.0, 100.0, 300) > 99.0);
        }
    }

    #[test]
    fn analytic_velocity_matches_finite_difference() {
        for spring in [SPRING_VIEW, SPRING_SHAKE, Spring { damping: 2.0, stiffness: 400.0 }] {
            for t in [0.0f32, 0.013, 0.05, 0.2] {
                let h = 1e-4;
                let num = (displacement(spring, -1.0, 0.3, t + h) - displacement(spring, -1.0, 0.3, t - h.min(t))) / (h + h.min(t));
                let ana = velocity(spring, -1.0, 0.3, t);
                assert!((num - ana).abs() < 5e-2 * (1.0 + ana.abs()), "{spring:?} t={t}: {num} vs {ana}");
            }
        }
    }

    #[test]
    fn shake_swings_and_decays() {
        let _g = guard();
        // starts displaced, crosses zero (under-damped), ends at rest
        assert!((shake_offset(0.0, 12.0) - 12.0).abs() < 1e-3);
        let samples: Vec<f32> = (0..400).map(|i| shake_offset(i as f32 / 1000.0, 12.0)).collect();
        let sign_changes = samples.windows(2).filter(|w| w[0].signum() != w[1].signum()).count();
        assert!(sign_changes >= 2, "oscillates ({sign_changes})");
        assert!(shake_offset(SPRING_SHAKE.settle_time(), 12.0).abs() < 0.5);
        // as a SpringValue: overshoots its target
        let max = (0..300).map(|ms| sample(SPRING_SHAKE, 0.0, 1.0, ms)).fold(0.0, f32::max);
        assert!(max > 1.1 && max < 1.8, "{max}");
    }

    #[test]
    fn easing_helpers_hit_the_curves() {
        for f in [ease_standard, ease_out_expo, ease_out_quad, ease_out_cubic] {
            assert!(f(0.0).abs() < 1e-3 && (f(1.0) - 1.0).abs() < 1e-3);
            assert!(f(0.5) > 0.5, "ease-out is ahead at the midpoint");
        }
        assert!((EASE_STANDARD.eval(0.25) - ease_standard(0.25)).abs() < 1e-6);
    }

    #[test]
    fn reduced_motion_turns_movement_into_short_fades() {
        let _g = guard();
        // The flag is process-global: keep every assertion that toggles it in one test.
        REDUCED_MOTION.store(false, Ordering::Relaxed);
        assert_eq!(duration_ms(220), 220);
        assert_eq!(tween(220, EASE_STANDARD).duration, Duration::from_millis(220));
        assert_eq!(move_t(0.3), 0.3);
        REDUCED_MOTION.store(true, Ordering::Relaxed);
        assert_eq!(duration_ms(220), 80);
        assert_eq!(duration_ms(60), 60, "never longer than asked");
        assert_eq!(tween(300, EASE_OUT_EXPO).duration, Duration::from_millis(80));
        assert_eq!(tween_linear(1200).duration, Duration::from_millis(80));
        assert_eq!(move_t(0.3), 1.0);
        assert_eq!(shake_offset(0.0, 12.0), 0.0);
        let t0 = Instant::now();
        let mut s = SpringValue::new(0.0, SPRING_VIEW);
        s.set_target_at(50.0, t0);
        assert_eq!(s.value_at(t0), 50.0);
        assert!(s.is_settled_at(t0));
        REDUCED_MOTION.store(false, Ordering::Relaxed);
    }
}
