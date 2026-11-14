//! A colour temperature that glides to its target. Pure: the caller passes `Instant`s.

use std::time::{Duration, Instant};

use crate::ramp::lerp_kelvin;

#[derive(Clone, Copy, Debug)]
struct Glide {
    from: f32,
    start: Instant,
    duration: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct Animator {
    current: f32,
    target: f32,
    glide: Option<Glide>,
}

impl Animator {
    pub fn new(kelvin: f32) -> Self {
        Self { current: kelvin, target: kelvin, glide: None }
    }

    #[cfg(test)]
    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn is_animating(&self) -> bool {
        self.glide.is_some()
    }

    /// Head for `to`, starting from wherever the glide is at `now`. A zero `over` jumps.
    pub fn retarget(&mut self, now: Instant, to: f32, over: Duration) {
        self.advance(now);
        self.target = to;
        if over.is_zero() || (self.current - to).abs() < 0.5 {
            self.current = to;
            self.glide = None;
        } else {
            self.glide = Some(Glide { from: self.current, start: now, duration: over });
        }
    }

    /// Move to `now` and return the temperature there.
    pub fn advance(&mut self, now: Instant) -> f32 {
        if let Some(g) = self.glide {
            let t = now.saturating_duration_since(g.start).as_secs_f32() / g.duration.as_secs_f32();
            if t >= 1.0 {
                self.current = self.target;
                self.glide = None;
            } else {
                // smoothstep: gentle at both ends so a 45 s ramp has no visible onset
                let eased = t * t * (3.0 - 2.0 * t);
                self.current = lerp_kelvin(g.from, self.target, eased);
            }
        }
        self.current
    }

    /// When the glide ends (`None` if idle).
    pub fn ends_at(&self) -> Option<Instant> {
        self.glide.map(|g| g.start + g.duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glides_from_a_to_b_and_stops() {
        let t0 = Instant::now();
        let mut a = Animator::new(6500.0);
        a.retarget(t0, 3400.0, Duration::from_secs(10));
        assert!(a.is_animating());
        assert_eq!(a.advance(t0), 6500.0);
        let mid = a.advance(t0 + Duration::from_secs(5));
        assert!(mid < 6500.0 && mid > 3400.0);
        let late = a.advance(t0 + Duration::from_secs(8));
        assert!(late < mid);
        assert_eq!(a.advance(t0 + Duration::from_secs(10)), 3400.0);
        assert!(!a.is_animating());
        assert_eq!(a.advance(t0 + Duration::from_secs(99)), 3400.0);
    }

    #[test]
    fn retarget_midway_continues_from_the_current_value() {
        let t0 = Instant::now();
        let mut a = Animator::new(6500.0);
        a.retarget(t0, 3400.0, Duration::from_secs(10));
        let at5 = t0 + Duration::from_secs(5);
        let mid = a.advance(at5);
        a.retarget(at5, 6500.0, Duration::from_secs(1));
        assert!((a.advance(at5) - mid).abs() < 0.5, "no jump when reversing");
        assert_eq!(a.advance(at5 + Duration::from_secs(1)), 6500.0);
    }

    #[test]
    fn zero_duration_jumps() {
        let t0 = Instant::now();
        let mut a = Animator::new(6500.0);
        a.retarget(t0, 3000.0, Duration::ZERO);
        assert_eq!((a.current(), a.is_animating(), a.ends_at()), (3000.0, false, None));
    }

    #[test]
    fn the_glide_is_eased_at_both_ends() {
        let t0 = Instant::now();
        let mut a = Animator::new(6500.0);
        a.retarget(t0, 3400.0, Duration::from_secs(100));
        let first = 6500.0 - a.advance(t0 + Duration::from_secs(1));
        let middle = a.advance(t0 + Duration::from_secs(50)) - a.advance(t0 + Duration::from_secs(51));
        assert!(first < middle, "slow start ({first}) then faster ({middle})");
    }
}
