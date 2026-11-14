//! logind: `org.freedesktop.login1` (system bus, zbus 5) — lock/unlock signals for the
//! current session, sleep preparation, capabilities, inhibitors and power actions.
//! No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<LogindState>)`.
//! One-shot signals are exposed as counters (`lock_seq`, `unlock_seq`): react when the
//! value changes (the shell spawns `ashd-lock` on a new `lock_seq`). Owner: quick
//! settings.
//!
//! Capabilities come from `CanSuspend` & co. (`"yes"`/`"challenge"` → true): on a host with
//! sleep disabled `can_suspend` / `can_hibernate` are false and the UI hides the entries.
//! `Handle::refresh_inhibitors()` fills `LogindState::inhibitors` (`ListInhibitors`) so the
//! countdown can name what blocks a shutdown.
//!
//! **`ASHD_FAKE_POWER=1`** (always compiled in): suspend / hibernate / reboot / power-off /
//! logout / lock-session are *logged* (`tracing` warn, target `ashd_svc_logind::fake`)
//! instead of being sent to logind; reading state is unaffected. Use [`fake_power`] in
//! panels to do the same for actions that do not go through this service (niri quit).
//! Every nested-session run must set it.
//!
//! Testing: never call the power actions from automated tests against the real bus —
//! `tests/mock_login1.rs` serves a mock `org.freedesktop.login1` over a peer-to-peer
//! connection.

mod service;

use std::time::Duration;

use async_channel::{Receiver, Sender};

/// An active inhibitor lock (`ListInhibitors`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inhibitor {
    /// Colon-separated: `shutdown:sleep:idle:handle-power-key…`.
    pub what: String,
    /// Who holds it ("Octium").
    pub who: String,
    /// Why ("Saving browser state").
    pub why: String,
    /// `block` or `delay`.
    pub mode: String,
    pub uid: u32,
    pub pid: u32,
}

impl Inhibitor {
    /// Does this lock hold back `what` (`"shutdown"`, `"sleep"`)? Both modes are
    /// relevant to the user: `block` stops the action, `delay` postpones it.
    pub fn affects(&self, what: &str) -> bool {
        self.what.split(':').any(|w| w == what)
    }
    pub fn blocks(&self) -> bool {
        self.mode == "block"
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogindState {
    /// Connected to login1 and the session object was found.
    pub available: bool,
    /// Session id (`$XDG_SESSION_ID` or from `GetSessionByPID`).
    pub session_id: Option<String>,
    /// `LockedHint` of the session.
    pub locked: bool,
    pub idle_hint: bool,
    /// Between `PrepareForSleep(true)` and `PrepareForSleep(false)`.
    pub preparing_for_sleep: bool,
    /// Incremented on every `Lock` signal for this session.
    pub lock_seq: u64,
    /// Incremented on every `Unlock` signal for this session.
    pub unlock_seq: u64,
    pub can_suspend: bool,
    pub can_hibernate: bool,
    pub can_reboot: bool,
    pub can_power_off: bool,
    /// Result of the last `refresh_inhibitors()`.
    pub inhibitors: Vec<Inhibitor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `Session.Lock()` — logind emits `Lock`, which the shell answers by locking.
    LockSession,
    /// `Session.SetLockedHint(bool)` — `ashd-lock` reports its state.
    SetLockedHint(bool),
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
    /// `Session.Terminate()`. The session panel prefers niri's `Quit` action (it ends the
    /// compositor cleanly); this is the fallback for sessions without niri.
    Logout,
    /// Re-read `ListInhibitors` into `LogindState::inhibitors`.
    RefreshInhibitors,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.try_send(cmd);
    }
    pub fn lock_session(&self) {
        self.send(Command::LockSession);
    }
    pub fn suspend(&self) {
        self.send(Command::Suspend);
    }
    pub fn hibernate(&self) {
        self.send(Command::Hibernate);
    }
    pub fn reboot(&self) {
        self.send(Command::Reboot);
    }
    pub fn power_off(&self) {
        self.send(Command::PowerOff);
    }
    pub fn logout(&self) {
        self.send(Command::Logout);
    }
    pub fn refresh_inhibitors(&self) {
        self.send(Command::RefreshInhibitors);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// `ASHD_FAKE_POWER` is set to something other than `""` / `0`: power actions are logged,
/// not performed.
pub fn fake_power() -> bool {
    std::env::var_os("ASHD_FAKE_POWER").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Spawn the `ashd-svc-logind` thread (system bus, reconnects with back-off).
pub fn start() -> (Handle, Receiver<LogindState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    let fake = fake_power();
    std::thread::Builder::new()
        .name("ashd-svc-logind".into())
        .spawn(move || {
            smol::block_on(service::run(
                rx,
                state_tx,
                service::Config { session_id: std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty()), fake_power: fake },
                || async { zbus::Connection::system().await },
            ))
        })
        .expect("spawn ashd-svc-logind");
    (Handle { tx }, state_rx)
}

/// Like [`start`] over an existing connection (a peer-to-peer one to a mock, in tests).
/// No reconnection: when the connection drops, the state becomes unavailable for good.
pub fn start_on(conn: zbus::Connection, session_id: Option<String>, fake_power: bool) -> (Handle, Receiver<LogindState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-logind".into())
        .spawn(move || {
            let mut once = Some(conn);
            smol::block_on(service::run(rx, state_tx, service::Config { session_id, fake_power }, move || {
                let c = once.take();
                async move { c.ok_or_else(|| zbus::Error::Failure("no more connections".into())) }
            }))
        })
        .expect("spawn ashd-svc-logind");
    (Handle { tx }, state_rx)
}

pub(crate) const RETRY_MIN: Duration = Duration::from_millis(500);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(10);
