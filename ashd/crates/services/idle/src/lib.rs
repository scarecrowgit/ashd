//! Idle: `ext-idle-notify-v1` stages, inhibitors, lock request, monitor power. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<IdleState>)`,
//! newest-snapshot channel. Owner: idle / night light / brightness.
//!
//! * **Stages** (configurable, see [`IdleConfig`] and `config.rs` for the `shell.json`
//!   `"idle"` section): after `lock_after` (default 10 min) without input
//!   [`IdleState::lock_seq`] is incremented — the shell turns a new value into
//!   `Request::Lock` (`ashd-shell-core`'s power bridge does); after `screen_off_after`
//!   (default 15 min) the service sends niri `PowerOffMonitors`. Any input powers the
//!   monitors back on: niri does that itself on the first input event; the service also
//!   sends `PowerOnMonitors` when the compositor reports `resumed` (idempotent). It never
//!   suspends — this host has sleep disabled, and nothing here talks to logind's power
//!   actions.
//! * **Inhibitors**: the compositor already stops idle notifications while a Wayland
//!   `idle-inhibit` client (a video player, a browser in fullscreen) is active. On top of
//!   that the service skips a due stage while a logind `idle` inhibitor is held (checked
//!   when a stage is due, then every 10 s while one is deferred) or while
//!   [`Handle::set_inhibited`]`(true)` was called — the shell bridge calls it with "any
//!   MPRIS player is Playing". A deferred stage fires as soon as the inhibitors are gone,
//!   if the user is still away.
//! * **Sleep**: nothing here. Locking before sleep (logind `PrepareForSleep`) is done by
//!   the same shell bridge from `LogindState::preparing_for_sleep`.
//! * `ASHD_FAKE_POWER=1`: the monitor actions are logged (target `ashd_svc_idle::fake`)
//!   instead of sent to niri (the lock request is only a counter; the bridge logs it).
//!   `ASHD_NESTED=1`: no logind inhibitor lookups.
//!
//! Layout: `model` is the pure state machine (unit-tested), `wl` the Wayland binding,
//! this file the service loop (tested against a fake backend, fake actions and fake
//! inhibitor source).

mod config;
mod inhibit;
mod model;
mod wl;

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use async_channel::{Receiver, Sender};
pub use config::{DEFAULT_LOCK_AFTER, DEFAULT_SCREEN_OFF_AFTER, IdleConfig, MIN_AFTER};
use model::{Effect, Kind, Machine};
use smol::future::or;

pub use crate::inhibit::InhibitSource;

/// How far the idle sequence has progressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// The user is here (or an inhibitor holds the stages back).
    #[default]
    Active,
    /// `lock_after` elapsed: a lock was requested.
    Lock,
    /// `screen_off_after` elapsed: the monitors were powered off.
    ScreenOff,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdleState {
    /// The compositor offers `ext_idle_notifier_v1` and we are connected.
    pub available: bool,
    /// When the current idle period began (now − the timeout of the first stage that
    /// fired); `None` while active.
    pub idle_since: Option<SystemTime>,
    pub stage: Stage,
    /// A logind or `set_inhibited` inhibitor holds the stages back.
    pub inhibited: bool,
    pub config: IdleConfig,
    /// Incremented every time the lock stage fires. Compare with the last value you saw.
    pub lock_seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// New stages (also recreates the compositor timers).
    SetConfig(IdleConfig),
    /// Re-read `shell.json` and apply it.
    ReloadConfig,
    SetInhibited(bool),
    /// Behave as if the user had just touched an input device (tests).
    SimulateActivity,
    /// Behave as if the compositor had reported this stage as due (tests).
    SimulateIdle(Stage),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        if self.tx.try_send(cmd).is_err() {
            tracing::debug!("idle service is not running");
        }
    }
    pub fn set_config(&self, config: IdleConfig) {
        self.send(Command::SetConfig(config));
    }
    pub fn reload_config(&self) {
        self.send(Command::ReloadConfig);
    }
    /// Hold the idle stages back (`true`) or release them. Who calls it: the shell's power
    /// bridge, with "an MPRIS player is playing".
    pub fn set_inhibited(&self, inhibited: bool) {
        self.send(Command::SetInhibited(inhibited));
    }
    pub fn simulate_activity(&self) {
        self.send(Command::SimulateActivity);
    }
    pub fn simulate_idle(&self, stage: Stage) {
        self.send(Command::SimulateIdle(stage));
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Events from the compositor side into the service loop.
#[derive(Debug)]
pub(crate) enum Input {
    Idled(Kind),
    Resumed,
    /// The Wayland connection broke.
    Lost,
}

/// Compositor timers.
pub(crate) trait Backend {
    fn set_timeouts(&mut self, lock: Option<Duration>, screen_off: Option<Duration>);
}

/// What the stages do to the machine.
pub(crate) trait Actions: Send + Sync {
    fn run(&self, effect: Effect);
}

/// Real actions: niri `PowerOffMonitors` / `PowerOnMonitors`.
struct NiriActions;

impl Actions for NiriActions {
    fn run(&self, effect: Effect) {
        use ashd_niri::niri_ipc::Action;
        let action = match effect {
            Effect::ScreenOff => Action::PowerOffMonitors {},
            Effect::ScreenOn => Action::PowerOnMonitors {},
        };
        if let Err(e) = ashd_niri::blocking::action(action) {
            tracing::warn!("idle: niri {effect:?}: {e}");
        }
    }
}

/// `ASHD_FAKE_POWER`: say what would happen.
struct LogActions;

impl Actions for LogActions {
    fn run(&self, effect: Effect) {
        tracing::warn!(target: "ashd_svc_idle::fake", "ASHD_FAKE_POWER: would run {effect:?} (nothing is done)");
    }
}

type Connect = Box<dyn FnMut(Sender<Input>) -> Result<Box<dyn Backend>, String> + Send>;

pub(crate) struct Env {
    pub connect: Connect,
    pub actions: Arc<dyn Actions>,
    pub inhibit: Option<Arc<dyn InhibitSource>>,
    pub config: IdleConfig,
}

/// `ASHD_NESTED` is set to something other than `""` / `0`.
fn nested() -> bool {
    std::env::var_os("ASHD_NESTED").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Spawn the `ashd-svc-idle` thread. Configuration: `shell.json` (see [`IdleConfig::load`]).
pub fn start() -> (Handle, Receiver<IdleState>) {
    start_with_config(IdleConfig::load())
}

/// Like [`start`] with explicit stages (tests, the nested session).
pub fn start_with_config(config: IdleConfig) -> (Handle, Receiver<IdleState>) {
    let actions: Arc<dyn Actions> = if ashd_svc_logind::fake_power() { Arc::new(LogActions) } else { Arc::new(NiriActions) };
    let inhibit: Option<Arc<dyn InhibitSource>> = if nested() { None } else { Some(Arc::new(inhibit::Logind::default())) };
    start_env(Env {
        connect: Box::new(|tx| wl::WlBackend::connect(tx).map(|b| Box::new(b) as Box<dyn Backend>)),
        actions,
        inhibit,
        config,
    })
}

pub(crate) fn start_env(env: Env) -> (Handle, Receiver<IdleState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-idle".into())
        .spawn(move || smol::block_on(run(rx, state_tx, env)))
        .expect("spawn ashd-svc-idle");
    (Handle { tx }, state_rx)
}

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// While a stage is deferred by an inhibitor: how often the logind inhibitors are re-read.
const DEFERRED_RECHECK: Duration = Duration::from_secs(10);

enum Wake {
    Command(Result<Command, async_channel::RecvError>),
    Input(Result<Input, async_channel::RecvError>),
    Timeout,
}

async fn run(commands: Receiver<Command>, state: Sender<IdleState>, mut env: Env) {
    let (in_tx, in_rx) = async_channel::unbounded::<Input>();
    let mut machine = Machine::new(env.config);
    let mut backend: Option<Box<dyn Backend>> = None;
    let mut retry_at = Instant::now();
    let mut backoff = RETRY_MIN;
    let mut last: Option<IdleState> = None;
    let mut publish = |machine: &Machine, available: bool| {
        let snap = machine.snapshot(available);
        if last.as_ref() != Some(&snap) {
            if last.as_ref().is_some_and(|l| l.stage != snap.stage || l.inhibited != snap.inhibited) {
                tracing::info!("idle: stage {:?}{}", snap.stage, if snap.inhibited { " (inhibited)" } else { "" });
            }
            let _ = state.force_send(snap.clone());
            last = Some(snap);
        }
    };
    publish(&machine, false);
    loop {
        if backend.is_none() && Instant::now() >= retry_at {
            match (env.connect)(in_tx.clone()) {
                Ok(mut b) => {
                    tracing::debug!("idle: connected");
                    b.set_timeouts(machine.cfg.lock_after, machine.cfg.screen_off_after);
                    backend = Some(b);
                    backoff = RETRY_MIN;
                }
                Err(e) => {
                    tracing::debug!("idle: {e}");
                    retry_at = Instant::now() + backoff;
                    backoff = (backoff * 2).min(RETRY_MAX);
                }
            }
            publish(&machine, backend.is_some());
        }
        let timeout = if backend.is_none() {
            Some(retry_at.saturating_duration_since(Instant::now()))
        } else if machine.deferred() {
            Some(DEFERRED_RECHECK)
        } else {
            None
        };
        let wake = or(
            async { Wake::Command(commands.recv().await) },
            or(async { Wake::Input(in_rx.recv().await) }, async {
                match timeout {
                    Some(d) => {
                        smol::Timer::after(d).await;
                        Wake::Timeout
                    }
                    None => std::future::pending().await,
                }
            }),
        )
        .await;
        let mut effects = Vec::new();
        match wake {
            Wake::Command(Err(_)) => return,
            Wake::Command(Ok(cmd)) => match cmd {
                Command::SetConfig(cfg) => effects = set_config(&mut machine, backend.as_deref_mut(), cfg),
                Command::ReloadConfig => effects = set_config(&mut machine, backend.as_deref_mut(), IdleConfig::load()),
                Command::SetInhibited(v) => effects = machine.set_inhibited(v),
                Command::SimulateActivity => effects = machine.resumed(),
                Command::SimulateIdle(stage) => {
                    for kind in [Kind::Lock, Kind::ScreenOff].into_iter().take(match stage {
                        Stage::Active => 0,
                        Stage::Lock => 1,
                        Stage::ScreenOff => 2,
                    }) {
                        effects.extend(machine.idled(kind, SystemTime::now()));
                    }
                }
            },
            Wake::Input(Err(_)) => return,
            Wake::Input(Ok(Input::Idled(kind))) => {
                effects.extend(recheck_logind(&mut machine, &env).await);
                effects.extend(machine.idled(kind, SystemTime::now()));
            }
            Wake::Input(Ok(Input::Resumed)) => effects = machine.resumed(),
            Wake::Input(Ok(Input::Lost)) => {
                tracing::debug!("idle: wayland connection lost");
                backend = None;
                retry_at = Instant::now() + backoff;
            }
            Wake::Timeout => {
                if machine.deferred() {
                    effects = recheck_logind(&mut machine, &env).await;
                }
            }
        }
        for effect in effects {
            let actions = env.actions.clone();
            smol::unblock(move || actions.run(effect)).detach();
        }
        publish(&machine, backend.is_some());
    }
}

fn set_config(machine: &mut Machine, backend: Option<&mut (dyn Backend + '_)>, cfg: IdleConfig) -> Vec<Effect> {
    let effects = machine.set_config(cfg);
    if let Some(b) = backend {
        b.set_timeouts(cfg.lock_after, cfg.screen_off_after);
    }
    effects
}

/// Read the logind `idle` inhibitors (off the service thread's executor) and feed them in.
async fn recheck_logind(machine: &mut Machine, env: &Env) -> Vec<Effect> {
    let Some(src) = env.inhibit.clone() else { return Vec::new() };
    let held = smol::unblock(move || src.idle_inhibited()).await;
    machine.set_logind_inhibited(held)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    /// The newest snapshot, kept between waits (the channel only carries changes).
    struct Watch<S> {
        rx: Receiver<S>,
        latest: Option<S>,
    }

    impl<S: Clone + std::fmt::Debug> Watch<S> {
        fn new(rx: Receiver<S>) -> Self {
            Self { rx, latest: None }
        }

        /// Every snapshot that arrived since the last call (the newest is also remembered).
        fn drain(&mut self) -> Vec<S> {
            let mut out = Vec::new();
            while let Ok(s) = self.rx.try_recv() {
                self.latest = Some(s.clone());
                out.push(s);
            }
            out
        }

        /// Wait (bounded) until the newest snapshot satisfies `pred`.
        fn until(&mut self, what: &str, pred: impl Fn(&S) -> bool) -> S {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.drain();
                if let Some(l) = self.latest.as_ref().filter(|l| pred(l)) {
                    return l.clone();
                }
                if Instant::now() >= deadline {
                    panic!("timed out waiting for {what}; last state {:?}", self.latest);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    use super::*;

    #[derive(Default)]
    struct Shared {
        timeouts: Vec<(Option<Duration>, Option<Duration>)>,
        effects: Vec<Effect>,
        logind: bool,
        input: Option<Sender<Input>>,
    }

    struct FakeBackend(Arc<Mutex<Shared>>);
    impl Backend for FakeBackend {
        fn set_timeouts(&mut self, lock: Option<Duration>, off: Option<Duration>) {
            self.0.lock().unwrap().timeouts.push((lock, off));
        }
    }
    struct FakeActions(Arc<Mutex<Shared>>);
    impl Actions for FakeActions {
        fn run(&self, effect: Effect) {
            self.0.lock().unwrap().effects.push(effect);
        }
    }
    struct FakeInhibit(Arc<Mutex<Shared>>);
    impl InhibitSource for FakeInhibit {
        fn idle_inhibited(&self) -> bool {
            self.0.lock().unwrap().logind
        }
    }

    fn fake(cfg: IdleConfig, connect_ok: bool) -> (Handle, Watch<IdleState>, Arc<Mutex<Shared>>) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let s = shared.clone();
        let (h, rx) = start_env(Env {
            connect: Box::new(move |tx| {
                if !connect_ok {
                    return Err("no compositor".into());
                }
                s.lock().unwrap().input = Some(tx);
                Ok(Box::new(FakeBackend(s.clone())))
            }),
            actions: Arc::new(FakeActions(shared.clone())),
            inhibit: Some(Arc::new(FakeInhibit(shared.clone()))),
            config: cfg,
        });
        (h, Watch::new(rx), shared)
    }


    fn cfg() -> IdleConfig {
        IdleConfig::new(Some(Duration::from_secs(5)), Some(Duration::from_secs(8)))
    }

    fn inject(shared: &Arc<Mutex<Shared>>, input: Input) {
        shared.lock().unwrap().input.as_ref().unwrap().try_send(input).unwrap();
    }

    #[test]
    fn connects_arms_the_timers_and_walks_the_stages() {
        let (h, mut rx, shared) = fake(cfg(), true);
        let s = rx.until("available", |s| s.available);
        assert_eq!(s.stage, Stage::Active);
        assert_eq!(shared.lock().unwrap().timeouts, vec![(Some(Duration::from_secs(5)), Some(Duration::from_secs(8)))]);

        inject(&shared, Input::Idled(Kind::Lock));
        let s = rx.until("lock stage", |s| s.stage == Stage::Lock);
        assert_eq!(s.lock_seq, 1);
        assert!(s.idle_since.is_some());

        inject(&shared, Input::Idled(Kind::ScreenOff));
        rx.until("screen off", |s| s.stage == Stage::ScreenOff);
        h.simulate_activity();
        let s = rx.until("back to active", |s| s.stage == Stage::Active);
        assert_eq!((s.idle_since, s.lock_seq), (None, 1));
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.lock().unwrap().effects.len() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shared.lock().unwrap().effects, vec![Effect::ScreenOff, Effect::ScreenOn]);
    }

    #[test]
    fn a_logind_idle_inhibitor_defers_the_stages_until_it_is_released() {
        let (h, mut rx, shared) = fake(cfg(), true);
        rx.until("available", |s| s.available);
        shared.lock().unwrap().logind = true;
        inject(&shared, Input::Idled(Kind::Lock));
        let s = rx.until("inhibited", |s| s.inhibited);
        assert_eq!((s.stage, s.lock_seq), (Stage::Active, 0));
        // An external release is not enough while logind still holds one.
        h.set_inhibited(false);
        std::thread::sleep(Duration::from_millis(100));
        for s in rx.drain() {
            assert_eq!(s.lock_seq, 0);
        }
        // The inhibitor goes away; the next stage notification (or the periodic re-check)
        // finds the machine free.
        shared.lock().unwrap().logind = false;
        inject(&shared, Input::Idled(Kind::ScreenOff));
        let s = rx.until("released", |s| s.stage == Stage::ScreenOff);
        assert_eq!(s.lock_seq, 1, "the deferred lock fires with it");
    }

    #[test]
    fn set_inhibited_defers_and_releases() {
        let (h, mut rx, shared) = fake(cfg(), true);
        rx.until("available", |s| s.available);
        h.set_inhibited(true);
        rx.until("inhibited", |s| s.inhibited);
        inject(&shared, Input::Idled(Kind::Lock));
        std::thread::sleep(Duration::from_millis(100));
        h.set_inhibited(false);
        let s = rx.until("lock after release", |s| s.lock_seq == 1);
        assert_eq!(s.stage, Stage::Lock);
        assert!(!s.inhibited);
    }

    #[test]
    fn set_config_rearms_the_timers() {
        let (h, mut rx, shared) = fake(cfg(), true);
        rx.until("available", |s| s.available);
        let new = IdleConfig::new(Some(Duration::from_secs(60)), None);
        h.set_config(new);
        let s = rx.until("new config", |s| s.config == new);
        assert_eq!(s.stage, Stage::Active);
        let t = shared.lock().unwrap().timeouts.clone();
        assert_eq!(t.last(), Some(&(Some(Duration::from_secs(60)), None)));
    }

    #[test]
    fn unavailable_without_a_compositor_and_recovers_after_a_lost_connection() {
        let (_h, mut rx, _shared) = fake(cfg(), false);
        let s = rx.until("first snapshot", |_| true);
        assert!(!s.available);

        let (_h, mut rx, shared) = fake(cfg(), true);
        rx.until("available", |s| s.available);
        inject(&shared, Input::Lost);
        rx.until("lost", |s| !s.available);
        // reconnect after the back-off (1 s)
        rx.until("reconnected", |s| s.available);
        assert_eq!(shared.lock().unwrap().timeouts.len(), 2, "timers armed again on the new connection");
    }

    #[test]
    fn simulate_idle_walks_to_the_requested_stage() {
        let (h, mut rx, _shared) = fake(cfg(), true);
        rx.until("available", |s| s.available);
        h.simulate_idle(Stage::Lock);
        let s = rx.until("lock", |s| s.stage == Stage::Lock);
        assert_eq!(s.lock_seq, 1);
        h.simulate_idle(Stage::ScreenOff);
        rx.until("off", |s| s.stage == Stage::ScreenOff);
    }
}
