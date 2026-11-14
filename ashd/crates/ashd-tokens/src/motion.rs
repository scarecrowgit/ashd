//! Motion constants (docs/design/TOKENS.md §9). "Anything spatial is a spring, anything
//! that appears is a tween; exits are a third faster than entrances; no bounce."
//!
//! The math lives here (no GPUI) so the lock screen can use it too; `ashd_ui::anim`
//! wraps it for GPUI elements.

use std::time::Duration;

// ── durations (ms) ────────────────────────────────────────────────────────────

pub const DUR_INSTANT: u64 = 80;
pub const DUR_FAST: u64 = 120;
pub const DUR_FLYOUT: u64 = 160;
pub const DUR_BASE: u64 = 220;
pub const DUR_SLOW: u64 = 300;

/// Per-animation timings from the sys-06 live demos.
pub mod timing {
    /// Rail submenu: 120 in / 100 out.
    pub const SUBMENU_IN: u64 = 120;
    pub const SUBMENU_OUT: u64 = 100;
    /// Toast: 180 in, linear timer.
    pub const TOAST_IN: u64 = 180;
    pub const TOAST_OUT: u64 = 120;
    /// Volume OSD: 100 in, hold 1200, 200 out.
    pub const OSD_IN: u64 = 100;
    pub const OSD_HOLD: u64 = 1200;
    pub const OSD_OUT: u64 = 200;
    /// Dock indicator: 160 standard.
    pub const DOCK_INDICATOR: u64 = 160;
    /// Launcher: 140 out-expo, scale .98 → 1.
    pub const LAUNCHER_IN: u64 = 140;
    pub const LAUNCHER_SCALE_FROM: f32 = 0.98;
    /// Scroll ↔ Canvas: 280 out-cubic.
    pub const MODE_SWITCH: u64 = 280;
    /// Theme switch: 300 ms crossfade of all surfaces.
    pub const THEME_CROSSFADE: u64 = 300;
    /// Modal scrim fade.
    pub const SCRIM: u64 = 160;
    /// Reduce motion: movement is 0 ms, only 80 ms fades remain.
    pub const REDUCED_FADE: u64 = 80;
}

pub const fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

// ── cubic-bezier curves ───────────────────────────────────────────────────────

/// CSS `cubic-bezier(x1, y1, x2, y2)`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Bezier(pub f32, pub f32, pub f32, pub f32);

/// Hover, focus, flyouts.
pub const EASE_STANDARD: Bezier = Bezier(0.2, 0.8, 0.2, 1.0);
/// Window open, launcher (out-expo).
pub const EASE_OUT_EXPO: Bezier = Bezier(0.16, 1.0, 0.3, 1.0);
/// Close, dismiss (out-quad).
pub const EASE_OUT_QUAD: Bezier = Bezier(0.5, 1.0, 0.89, 1.0);
/// Toasts, Scroll ↔ Canvas (out-cubic).
pub const EASE_OUT_CUBIC: Bezier = Bezier(0.33, 1.0, 0.68, 1.0);

impl Bezier {
    pub const LINEAR: Bezier = Bezier(0.0, 0.0, 1.0, 1.0);

    /// Progress at time fraction `t` in 0..=1 (solves x(s) = t, returns y(s)).
    pub fn eval(self, t: f32) -> f32 {
        let Bezier(x1, y1, x2, y2) = self;
        let t = t.clamp(0.0, 1.0);
        if self == Self::LINEAR {
            return t;
        }
        let bez = |a: f32, b: f32, s: f32| {
            let inv = 1.0 - s;
            3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
        };
        let (mut lo, mut hi) = (0f32, 1f32);
        let mut s = t;
        for _ in 0..32 {
            let x = bez(x1, x2, s);
            if (x - t).abs() < 1e-5 {
                break;
            }
            if x < t {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.0;
        }
        bez(y1, y2, s)
    }
}

// ── springs ───────────────────────────────────────────────────────────────────

/// niri-style spring: damping ratio (1.0 = critically damped) and stiffness, unit mass.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Spring {
    pub damping: f32,
    pub stiffness: f32,
}

pub const SPRING_VIEW: Spring = Spring { damping: 1.0, stiffness: 800.0 };
pub const SPRING_WORKSPACE: Spring = Spring { damping: 1.0, stiffness: 1000.0 };
pub const SPRING_CANVAS_ZOOM: Spring = Spring { damping: 1.0, stiffness: 600.0 };
pub const SPRING_CANVAS_PAN: Spring = Spring { damping: 1.0, stiffness: 500.0 };
pub const SPRING_SCRATCHPAD: Spring = Spring { damping: 1.0, stiffness: 700.0 };
/// Wrong-password shake: the one under-damped spring.
pub const SPRING_SHAKE: Spring = Spring { damping: 0.4, stiffness: 900.0 };

/// Settling threshold, like niri's `epsilon=0.0001` (fraction of the distance).
pub const SPRING_EPSILON: f32 = 0.0001;

impl Spring {
    /// Angular frequency ω0 = √k (unit mass).
    pub fn omega(self) -> f32 {
        self.stiffness.max(0.0).sqrt()
    }

    /// Normalised position 0 → 1 at time `t` seconds for a spring released from 0 towards
    /// 1 with initial velocity `v0` (in units of distance per second, normalised).
    pub fn value(self, t: f32, v0: f32) -> f32 {
        let w = self.omega();
        let z = self.damping;
        let t = t.max(0.0);
        // x(t) is the displacement from the target (starts at -1).
        let x0 = -1.0f32;
        let x = if (z - 1.0).abs() < 1e-4 {
            // critically damped
            (x0 + (v0 + w * x0) * t) * (-w * t).exp()
        } else if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let a = x0;
            let b = (v0 + z * w * x0) / wd;
            (-z * w * t).exp() * (a * (wd * t).cos() + b * (wd * t).sin())
        } else {
            let s = (z * z - 1.0).sqrt();
            let r1 = -w * (z - s);
            let r2 = -w * (z + s);
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            c1 * (r1 * t).exp() + c2 * (r2 * t).exp()
        };
        1.0 + x
    }

    /// Time until |displacement| stays below `SPRING_EPSILON` (upper bound, seconds).
    pub fn settle_time(self) -> f32 {
        let w = self.omega().max(1e-3);
        let decay = (self.damping.min(1.0) * w).max(1e-3);
        // envelope (1 + w t) e^{-w t} for critical damping; a little slack covers it.
        let mut t = (1.0 / SPRING_EPSILON).ln() / decay;
        if (self.damping - 1.0).abs() < 1e-4 {
            t *= 1.3;
        }
        t
    }

    pub fn settle_duration(self) -> Duration {
        Duration::from_secs_f32(self.settle_time())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints() {
        for b in [EASE_STANDARD, EASE_OUT_EXPO, EASE_OUT_QUAD, EASE_OUT_CUBIC, Bezier::LINEAR] {
            assert!(b.eval(0.0).abs() < 1e-3);
            assert!((b.eval(1.0) - 1.0).abs() < 1e-3);
            assert!(b.eval(0.5) >= 0.5 - 1e-3, "ease-out curves are ahead at t=.5");
        }
    }

    #[test]
    fn critically_damped_never_overshoots() {
        let s = SPRING_VIEW;
        let mut prev = 0.0;
        for i in 0..2000 {
            let v = s.value(i as f32 / 1000.0, 0.0);
            assert!(v <= 1.0 + 1e-5 && v + 1e-6 >= prev);
            prev = v;
        }
        assert!((s.value(s.settle_time(), 0.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn shake_overshoots() {
        let max = (0..1000).map(|i| SPRING_SHAKE.value(i as f32 / 1000.0, 0.0)).fold(0.0, f32::max);
        assert!(max > 1.1);
    }
}
