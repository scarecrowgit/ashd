//! The power bridge: three small hooks between services that have no GPUI and no
//! knowledge of each other.
//!
//! * **idle -> lock**: a new `IdleState::lock_seq` (the idle service's lock stage fired)
//!   becomes `Request::Lock`.
//! * **logind -> lock**: `loginctl lock-session` (a new `LogindState::lock_seq`) and
//!   `PrepareForSleep(true)` (lock before sleep) become `Request::Lock` too. This host has
//!   sleep disabled, but a suspend triggered from elsewhere must not leave the session open.
//! * **mpris -> idle**: while any MPRIS player is `Playing` the idle stages are held back
//!   (`ashd_svc_idle::Handle::set_inhibited`).
//!
//! A lock is not requested while the session already reports `LockedHint`, nor twice within
//! [`LOCK_COOLDOWN`] (`ashd-lock` needs a moment to take the lock and set the hint). With
//! `ASHD_FAKE_POWER=1` the request is logged instead (nested sessions must never lock).

use std::{cell::Cell, rc::Rc, time::{Duration, Instant}};

use gpui::{App, WeakEntity};
use ashd_ipc::Request;

use crate::{Services, Shell};

/// No second lock request this soon after one.
const LOCK_COOLDOWN: Duration = Duration::from_secs(5);

/// Is `now` a new value of a monotonically increasing counter (a reset to 0 after a
/// service reconnect is not a new event)?
pub(crate) fn advanced(seen: u64, now: u64) -> bool {
    now > seen
}

/// Rising edge of a flag.
pub(crate) fn rose(was: bool, now: bool) -> bool {
    !was && now
}

pub(crate) fn wire(services: &Services, shell: WeakEntity<Shell>, cx: &mut App) {
    let gate: Rc<Cell<Option<Instant>>> = Rc::default();

    // idle -> lock
    let mut seen = services.idle.state.read(cx).lock_seq;
    let (services_c, shell_c, gate_c) = (services.clone(), shell.clone(), gate.clone());
    cx.observe(&services.idle.state, move |state, cx| {
        let seq = state.read(cx).lock_seq;
        if advanced(seen, seq) {
            request_lock("idle", &services_c, &shell_c, &gate_c, cx);
        }
        seen = seq;
    })
    .detach();

    // logind -> lock (lock-session, lock before sleep)
    let initial = services.logind.state.read(cx);
    let (mut seen, mut preparing) = (initial.lock_seq, initial.preparing_for_sleep);
    let (services_c, shell_c, gate_c) = (services.clone(), shell, gate);
    cx.observe(&services.logind.state, move |state, cx| {
        let (seq, sleeping) = {
            let s = state.read(cx);
            (s.lock_seq, s.preparing_for_sleep)
        };
        if advanced(seen, seq) {
            request_lock("logind lock signal", &services_c, &shell_c, &gate_c, cx);
        }
        if rose(preparing, sleeping) {
            request_lock("sleep", &services_c, &shell_c, &gate_c, cx);
        }
        (seen, preparing) = (seq, sleeping);
    })
    .detach();

    // mpris -> idle inhibit
    let handle = services.idle.handle.clone();
    let mut playing = false;
    cx.observe(&services.mpris.state, move |state, cx| {
        let now = state.read(cx).players.iter().any(|p| p.is_playing());
        if now != playing {
            playing = now;
            handle.set_inhibited(now);
        }
    })
    .detach();
}

fn request_lock(reason: &str, services: &Services, shell: &WeakEntity<Shell>, gate: &Cell<Option<Instant>>, cx: &mut App) {
    if services.logind.state.read(cx).locked {
        tracing::debug!("lock requested by {reason}: already locked");
        return;
    }
    if gate.get().is_some_and(|t| t.elapsed() < LOCK_COOLDOWN) {
        tracing::debug!("lock requested by {reason}: one is already on its way");
        return;
    }
    gate.set(Some(Instant::now()));
    if ashd_svc_logind::fake_power() {
        tracing::warn!(target: "ashd::power", "ASHD_FAKE_POWER: lock requested by {reason} (nothing is done)");
        return;
    }
    tracing::info!("locking the session ({reason})");
    crate::dispatch(shell, &Request::Lock, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_only_count_forward() {
        assert!(advanced(0, 1));
        assert!(advanced(3, 4));
        assert!(!advanced(3, 3));
        assert!(!advanced(3, 0), "a service restart resets the counter: not a lock");
    }

    #[test]
    fn flags_fire_on_the_rising_edge_only() {
        assert!(rose(false, true));
        assert!(!rose(true, true));
        assert!(!rose(true, false));
        assert!(!rose(false, false));
    }
}
