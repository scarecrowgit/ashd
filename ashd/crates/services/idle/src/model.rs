//! The pure core: idle notifications in, stage + effects out. No I/O, no clock of its own.

use std::time::{Duration, SystemTime};

use crate::{IdleConfig, IdleState, Stage};

/// Which compositor timeout fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Lock,
    ScreenOff,
}

/// Something the service must do outside the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    ScreenOff,
    ScreenOn,
}

#[derive(Debug, Default)]
pub struct Machine {
    pub cfg: IdleConfig,
    /// The compositor said "idle for `lock_after`" and the user has not come back.
    lock_due: bool,
    off_due: bool,
    lock_fired: bool,
    off_fired: bool,
    idle_since: Option<SystemTime>,
    /// `Handle::set_inhibited` (media playing, ...).
    external: bool,
    /// A logind `idle` inhibitor is held.
    logind: bool,
    lock_seq: u64,
}

impl Machine {
    pub fn new(cfg: IdleConfig) -> Self {
        Self { cfg, ..Self::default() }
    }

    pub fn inhibited(&self) -> bool {
        self.external || self.logind
    }

    /// A stage is due but held back by an inhibitor (the service re-checks the logind
    /// inhibitors periodically while this is true).
    pub fn deferred(&self) -> bool {
        self.inhibited() && ((self.lock_due && !self.lock_fired) || (self.off_due && !self.off_fired))
    }

    /// Is the user idle as far as the compositor is concerned?
    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.lock_due || self.off_due
    }

    pub fn stage(&self) -> Stage {
        if self.off_fired {
            Stage::ScreenOff
        } else if self.lock_fired {
            Stage::Lock
        } else {
            Stage::Active
        }
    }

    #[cfg(test)]
    pub fn lock_seq(&self) -> u64 {
        self.lock_seq
    }

    /// The compositor's idle notification of `kind` fired at `now`.
    pub fn idled(&mut self, kind: Kind, now: SystemTime) -> Vec<Effect> {
        let after = match kind {
            Kind::Lock => self.cfg.lock_after,
            Kind::ScreenOff => self.cfg.screen_off_after,
        };
        let since = now.checked_sub(after.unwrap_or(Duration::ZERO)).unwrap_or(now);
        self.idle_since = Some(self.idle_since.map_or(since, |old| old.min(since)));
        match kind {
            Kind::Lock => self.lock_due = true,
            Kind::ScreenOff => self.off_due = true,
        }
        self.settle()
    }

    /// Input happened (`resumed`, or `simulate_activity`).
    pub fn resumed(&mut self) -> Vec<Effect> {
        let was_off = self.off_fired;
        self.lock_due = false;
        self.off_due = false;
        self.lock_fired = false;
        self.off_fired = false;
        self.idle_since = None;
        if was_off { vec![Effect::ScreenOn] } else { Vec::new() }
    }

    pub fn set_inhibited(&mut self, v: bool) -> Vec<Effect> {
        self.external = v;
        self.settle()
    }

    pub fn set_logind_inhibited(&mut self, v: bool) -> Vec<Effect> {
        self.logind = v;
        self.settle()
    }

    /// New timeouts: the compositor objects are recreated, so start from "active".
    pub fn set_config(&mut self, cfg: IdleConfig) -> Vec<Effect> {
        self.cfg = cfg;
        self.resumed()
    }

    /// Fire every stage that is due and not held back.
    fn settle(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.inhibited() {
            return out;
        }
        if self.lock_due && !self.lock_fired {
            self.lock_fired = true;
            self.lock_seq += 1;
        }
        if self.off_due && !self.off_fired {
            self.off_fired = true;
            out.push(Effect::ScreenOff);
        }
        out
    }

    pub fn snapshot(&self, available: bool) -> IdleState {
        IdleState {
            available,
            idle_since: self.idle_since,
            stage: self.stage(),
            inhibited: self.inhibited(),
            config: self.cfg,
            lock_seq: self.lock_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    fn short() -> Machine {
        Machine::new(IdleConfig::new(Some(Duration::from_secs(5)), Some(Duration::from_secs(8))))
    }

    #[test]
    fn stages_follow_the_notifications_in_order() {
        let mut m = short();
        assert_eq!(m.stage(), Stage::Active);
        assert!(m.idled(Kind::Lock, t(5)).is_empty());
        assert_eq!((m.stage(), m.lock_seq()), (Stage::Lock, 1));
        assert_eq!(m.idled(Kind::ScreenOff, t(8)), vec![Effect::ScreenOff]);
        assert_eq!((m.stage(), m.lock_seq()), (Stage::ScreenOff, 1));
        // idle_since is when the user stopped, not when the stage fired
        assert_eq!(m.snapshot(true).idle_since, Some(t(0)));
    }

    #[test]
    fn activity_resets_everything_and_powers_monitors_back_on() {
        let mut m = short();
        m.idled(Kind::Lock, t(5));
        assert!(m.resumed().is_empty(), "monitors were never off");
        assert_eq!((m.stage(), m.snapshot(true).idle_since), (Stage::Active, None));
        m.idled(Kind::Lock, t(100));
        m.idled(Kind::ScreenOff, t(103));
        assert_eq!(m.resumed(), vec![Effect::ScreenOn]);
        assert_eq!(m.stage(), Stage::Active);
        // a new idle period locks again
        m.idled(Kind::Lock, t(200));
        assert_eq!(m.lock_seq(), 3);
    }

    #[test]
    fn screen_off_before_lock_still_locks_later() {
        let mut m = Machine::new(IdleConfig::new(Some(Duration::from_secs(20)), Some(Duration::from_secs(8))));
        assert_eq!(m.idled(Kind::ScreenOff, t(8)), vec![Effect::ScreenOff]);
        assert_eq!(m.lock_seq(), 0);
        m.idled(Kind::Lock, t(20));
        assert_eq!(m.lock_seq(), 1);
        assert_eq!(m.stage(), Stage::ScreenOff);
    }

    #[test]
    fn an_inhibitor_defers_and_release_fires_while_still_idle() {
        let mut m = short();
        m.set_inhibited(true);
        assert!(m.idled(Kind::Lock, t(5)).is_empty());
        assert!(m.idled(Kind::ScreenOff, t(8)).is_empty());
        assert_eq!((m.stage(), m.lock_seq()), (Stage::Active, 0));
        assert!(m.deferred() && m.is_idle());
        assert_eq!(m.set_inhibited(false), vec![Effect::ScreenOff]);
        assert_eq!((m.stage(), m.lock_seq(), m.deferred()), (Stage::ScreenOff, 1, false));
    }

    #[test]
    fn release_after_the_user_came_back_does_nothing() {
        let mut m = short();
        m.set_inhibited(true);
        m.idled(Kind::Lock, t(5));
        m.resumed();
        assert!(m.set_inhibited(false).is_empty());
        assert_eq!(m.lock_seq(), 0);
    }

    #[test]
    fn logind_and_external_inhibitors_combine() {
        let mut m = short();
        m.set_logind_inhibited(true);
        m.set_inhibited(true);
        m.idled(Kind::Lock, t(5));
        m.set_inhibited(false);
        assert_eq!(m.lock_seq(), 0, "logind still holds");
        assert!(m.snapshot(true).inhibited);
        m.set_logind_inhibited(false);
        assert_eq!(m.lock_seq(), 1);
    }

    #[test]
    fn a_stage_that_already_fired_is_not_undone_by_an_inhibitor() {
        let mut m = short();
        m.idled(Kind::Lock, t(5));
        m.set_inhibited(true);
        assert_eq!(m.stage(), Stage::Lock);
        assert_eq!(m.lock_seq(), 1);
    }

    #[test]
    fn changing_the_config_starts_over() {
        let mut m = short();
        m.idled(Kind::Lock, t(5));
        m.idled(Kind::ScreenOff, t(8));
        let fx = m.set_config(IdleConfig::DISABLED);
        assert_eq!(fx, vec![Effect::ScreenOn]);
        assert_eq!(m.stage(), Stage::Active);
        assert_eq!(m.cfg, IdleConfig::DISABLED);
    }
}
