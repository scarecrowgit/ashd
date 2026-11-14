//! Time-based animation helpers on top of `ashd_tokens::motion` (no state of their own
//! beyond a start instant, so a frame can be re-drawn for any `now`).

use std::time::{Duration, Instant};

use ashd_tokens::motion::{Bezier, Spring};

/// A tween from 0 to 1 over `duration` with a cubic-bezier curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    pub start: Instant,
    pub duration: Duration,
    pub curve: Bezier,
}

impl Tween {
    pub fn new(start: Instant, duration_ms: u64, curve: Bezier) -> Self {
        Self { start, duration: Duration::from_millis(duration_ms), curve }
    }

    /// Linear time fraction 0..=1.
    pub fn t(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Eased progress 0..=1.
    pub fn value(&self, now: Instant) -> f32 {
        self.curve.eval(self.t(now))
    }

    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.duration
    }

    pub fn end(&self) -> Instant {
        self.start + self.duration
    }
}

/// A spring released from 0 towards 1 (`Spring::value`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringAnim {
    pub start: Instant,
    pub spring: Spring,
    pub v0: f32,
}

impl SpringAnim {
    pub fn new(start: Instant, spring: Spring) -> Self {
        Self { start, spring, v0: 0.0 }
    }

    pub fn value(&self, now: Instant) -> f32 {
        self.spring.value(now.saturating_duration_since(self.start).as_secs_f32(), self.v0)
    }

    pub fn settled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.spring.settle_duration()
    }

    pub fn end(&self) -> Instant {
        self.start + self.spring.settle_duration()
    }
}

/// A shake: the spring's impulse response (starts at rest at 0 with a kick, oscillates
/// around 0), scaled so the first excursion is `-amplitude` px. With `SPRING_SHAKE`
/// (damping 0.4, stiffness 900) that is ≈ −10, +2.5, −0.6 px over ~0.5 s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shake {
    pub start: Instant,
    pub spring: Spring,
    pub amplitude: f32,
}

impl Shake {
    pub fn new(start: Instant, spring: Spring, amplitude: f32) -> Self {
        Self { start, spring, amplitude }
    }

    /// Unscaled impulse response x(t) = e^{-ζωt} sin(ω_d t) (under-damped springs only;
    /// a critically/over-damped spring gives t·e^{-ωt}).
    fn impulse(spring: Spring, t: f32) -> f32 {
        let w = spring.omega();
        let z = spring.damping;
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            (-z * w * t).exp() * (wd * t).sin()
        } else {
            w * t * (-w * t).exp()
        }
    }

    fn first_peak(spring: Spring) -> f32 {
        let w = spring.omega().max(1e-3);
        let z = spring.damping;
        let tp = if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            (wd / (z * w).max(1e-6)).atan() / wd
        } else {
            1.0 / w
        };
        Self::impulse(spring, tp).abs().max(1e-6)
    }

    /// Horizontal offset in px at `now`.
    pub fn offset(&self, now: Instant) -> f32 {
        if self.settled(now) {
            return 0.0;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        -self.amplitude * Self::impulse(self.spring, t) / Self::first_peak(self.spring)
    }

    /// Settled when the envelope is below a quarter pixel.
    pub fn settled(&self, now: Instant) -> bool {
        now >= self.end()
    }

    pub fn end(&self) -> Instant {
        let w = self.spring.omega().max(1e-3);
        let decay = (self.spring.damping.min(1.0) * w).max(1e-3);
        let peak = Self::first_peak(self.spring);
        // amplitude/peak · e^{-decay t} < 0.25
        let t = ((self.amplitude / peak) / 0.25).max(1.0).ln() / decay;
        self.start + Duration::from_secs_f32(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ashd_tokens::motion::{EASE_OUT_QUAD, SPRING_SHAKE};

    #[test]
    fn tween_runs_0_to_1() {
        let t0 = Instant::now();
        let tw = Tween::new(t0, 300, EASE_OUT_QUAD);
        assert_eq!(tw.value(t0), 0.0);
        assert!((tw.value(t0 + Duration::from_millis(300)) - 1.0).abs() < 1e-3);
        assert!(tw.done(t0 + Duration::from_millis(301)));
    }

    #[test]
    fn shake_peaks_alternate_and_decay() {
        let t0 = Instant::now();
        let s = Shake::new(t0, SPRING_SHAKE, 10.0);
        let samples: Vec<f32> = (0..800).map(|ms| s.offset(t0 + Duration::from_millis(ms))).collect();
        let min = samples.iter().copied().fold(f32::MAX, f32::min);
        assert!((min + 10.0).abs() < 0.2, "first excursion ≈ −10, got {min}");
        let max = samples.iter().copied().fold(f32::MIN, f32::max);
        assert!(max > 1.0 && max < 10.0, "under-damped overshoot, got {max}");
        assert_eq!(s.offset(s.end() + Duration::from_millis(1)), 0.0);
        assert!(s.end() - t0 < Duration::from_millis(800));
    }
}
