//! Night light: colour temperature through `zwlr_gamma_control_manager_v1`. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<NightlightState>)`,
//! newest-snapshot channel. Owner: idle / night light / brightness.
//!
//! * **Maths** (`ramp`): a blackbody fit normalised at 6500 K, so 6500 K is exactly the
//!   identity ramp; glides interpolate in mired. `anim` is the glide, `solar` NOAA's
//!   sunrise / sunset formulas, `schedule` the modes, `settings` defaults + `shell.json` +
//!   the state file. All pure and unit-tested.
//! * **Modes** ([`Mode`]): `off`, `on` (manual, until switched off), `fixed` (default
//!   21:00-07:00, may wrap midnight), `sun` (sunset to sunrise at the configured
//!   latitude / longitude; without coordinates it behaves like `fixed`).
//! * **Transitions**: ~45 s (30-60 s, configurable) when a schedule flips, 1 s when
//!   toggled by hand, 250 ms for temperature adjustments.
//! * **Outputs**: one gamma control per `wl_output`, created and dropped with hotplug. A
//!   control the compositor refuses (`failed`, e.g. gammastep holds it) marks that output
//!   unavailable (`OutputStatus::available`); the service retries every 30 s.
//!   `NightlightState::available` is "at least one output works".
//! * **Shutdown / crash**: the compositor restores the original table when a control is
//!   destroyed or the connection closes; dropping the service does the former.
//! * **Persistence**: `<state dir>/nightlight.json` (`mode`, `temperature`, `lastMode`),
//!   written (debounced) after every command; see `settings.rs` for the `shell.json`
//!   `"nightlight"` section.
//!
//! Testing: the service loop is driven against a fake [`Gamma`] (`tests` below); the real
//! protocol only ever talks to the compositor named by `$WAYLAND_DISPLAY`, i.e. the
//! nested niri in the verification session.

mod anim;
mod fake;
mod ramp;
mod schedule;
mod settings;
mod solar;
mod wl;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anim::Animator;
use async_channel::{Receiver, Sender};
use chrono::{DateTime, Local, TimeZone};
pub use ramp::{MIN_K, NEUTRAL_K, build_ramp, kelvin_to_rgb, lerp_kelvin};
pub use schedule::{Decision, Mode, Schedule, TimeOfDay};
pub use settings::{
    ADJUST_TRANSITION, DEFAULT_MANUAL_TRANSITION, DEFAULT_SCHEDULE_TRANSITION, DEFAULT_TEMPERATURE, Persisted, Settings, clamp_temperature,
};
use smol::future::or;
pub use solar::{SunDay, elevation_deg, sun_day};

/// Gamma control of one output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputStatus {
    /// Connector name (`DP-1`) once the compositor said it.
    pub name: String,
    /// The compositor granted a working gamma control.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NightlightState {
    /// At least one output has a working gamma control.
    pub available: bool,
    /// The mode is not `off`.
    pub enabled: bool,
    /// The tint is on right now (manual `on`, or inside the schedule's window).
    pub active: bool,
    /// The configured night temperature in Kelvin.
    pub temperature: u32,
    /// What is applied right now (differs from `temperature` while gliding).
    pub current: u32,
    /// The next moment a scheduled mode flips (end of the window while `active`, start of
    /// the next one otherwise). `None` for `off` / `on` and polar day / night.
    pub until: Option<TimeOfDay>,
    pub mode: Mode,
    /// Fixed window (also the fallback of `sun` without coordinates).
    pub start: TimeOfDay,
    pub end: TimeOfDay,
    /// `sun` mode has a latitude and longitude to work with.
    pub has_location: bool,
    pub outputs: Vec<OutputStatus>,
}

impl Default for NightlightState {
    fn default() -> Self {
        let s = Settings::default();
        Self {
            available: false,
            enabled: false,
            active: false,
            temperature: s.temperature,
            current: NEUTRAL_K,
            until: None,
            mode: Mode::Off,
            start: s.schedule.start,
            end: s.schedule.end,
            has_location: false,
            outputs: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// On <-> off (switching on returns to the last non-off mode).
    Toggle,
    SetEnabled(bool),
    /// The night temperature in Kelvin (clamped to 1000-6500).
    SetTemperature(u32),
    SetMode(Mode),
    /// Re-read `shell.json` and the state file.
    ReloadConfig,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        if self.tx.try_send(cmd).is_err() {
            tracing::debug!("nightlight service is not running");
        }
    }
    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.send(Command::SetEnabled(enabled));
    }
    pub fn set_temperature(&self, kelvin: u32) {
        self.send(Command::SetTemperature(kelvin));
    }
    pub fn set_mode(&self, mode: Mode) {
        self.send(Command::SetMode(mode));
    }
    pub fn reload_config(&self) {
        self.send(Command::ReloadConfig);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

// ── the pure controller ───────────────────────────────────────────────────────────────────

/// Why the controller is looking at the clock (decides how long the glide is).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Startup,
    /// A timer woke us: a schedule edge is a long glide.
    Clock,
    Manual,
    Adjust,
}

pub(crate) struct Eval<Tz: TimeZone> {
    pub active: bool,
    pub next_change: Option<DateTime<Tz>>,
}

pub(crate) struct Core {
    pub settings: Settings,
    anim: Animator,
}

impl Core {
    pub(crate) fn new(settings: Settings) -> Self {
        Self { settings, anim: Animator::new(NEUTRAL_K as f32) }
    }

    /// Decide whether the tint should be on at `wall`, and start a glide if that changed.
    pub(crate) fn evaluate<Tz: TimeZone>(&mut self, now: Instant, wall: DateTime<Tz>, reason: Reason) -> Eval<Tz> {
        let d = self.settings.schedule.decide(wall);
        let target = if d.active { self.settings.temperature } else { NEUTRAL_K } as f32;
        if (target - self.anim.target()).abs() > 0.5 {
            let over = match reason {
                Reason::Clock if self.settings.mode().is_scheduled() => self.settings.schedule_transition,
                Reason::Adjust => ADJUST_TRANSITION,
                _ => self.settings.manual_transition,
            };
            self.anim.retarget(now, target, over);
        }
        Eval { active: d.active, next_change: d.next_change }
    }

    pub(crate) fn advance(&mut self, now: Instant) -> f32 {
        self.anim.advance(now)
    }

    pub(crate) fn animating(&self) -> bool {
        self.anim.is_animating()
    }

    pub(crate) fn glide_ends_at(&self) -> Option<Instant> {
        self.anim.ends_at()
    }

    /// Apply a command to the settings; returns the reason to re-evaluate with.
    pub(crate) fn command(&mut self, cmd: &Command) -> Reason {
        match cmd {
            Command::Toggle => {
                let next = if self.settings.enabled() { Mode::Off } else { self.settings.last_mode };
                self.settings.set_mode(next);
                Reason::Manual
            }
            Command::SetEnabled(on) => {
                let next = if *on { self.settings.last_mode } else { Mode::Off };
                self.settings.set_mode(next);
                Reason::Manual
            }
            Command::SetMode(mode) => {
                self.settings.set_mode(*mode);
                Reason::Manual
            }
            Command::SetTemperature(k) => {
                self.settings.temperature = clamp_temperature(*k);
                Reason::Adjust
            }
            Command::ReloadConfig => Reason::Manual,
        }
    }
}

// ── the service ───────────────────────────────────────────────────────────────────────────

/// Compositor-side events into the service loop.
#[derive(Debug)]
pub(crate) enum Input {
    /// An output appeared, went away, got a name, a gamma size, or a control failed.
    OutputsChanged,
    Lost,
}

/// The gamma controls of all outputs.
pub(crate) trait Gamma {
    /// Set every working output to this per-channel multiplier.
    fn apply(&mut self, rgb: [f32; 3]);
    fn outputs(&self) -> Vec<OutputStatus>;
    /// Ask again for the controls that failed; `true` if there were any.
    fn retry_failed(&mut self) -> bool;
    fn has_failed(&self) -> bool;
}

type Connect = Box<dyn FnMut(Sender<Input>) -> Result<Box<dyn Gamma>, String> + Send>;

pub(crate) struct Env {
    pub connect: Connect,
    pub settings: Settings,
    /// Where to persist UI changes; `None` in tests.
    pub state_file: Option<PathBuf>,
}

/// Spawn the `ashd-svc-nightlight` thread. Settings: `shell.json` + the state file.
pub fn start() -> (Handle, Receiver<NightlightState>) {
    let state_file = settings::state_file();
    start_with_settings(Settings::load(&state_file), Some(state_file))
}

/// Like [`start`] with explicit settings (tests, the nested session).
pub fn start_with_settings(settings: Settings, state_file: Option<PathBuf>) -> (Handle, Receiver<NightlightState>) {
    // The nested niri has no gamma control: `ASHD_FAKE_NIGHTLIGHT=1` (only honoured with
    // `ASHD_NESTED=1`) swaps in a logging stand-in so the UI and the glide can be exercised.
    let fake = env_flag("ASHD_NESTED") && env_flag("ASHD_FAKE_NIGHTLIGHT");
    let connect: Connect = if fake {
        Box::new(|_tx| Ok(Box::new(fake::LogGamma::new()) as Box<dyn Gamma>))
    } else {
        Box::new(|tx| wl::WlGamma::connect(tx).map(|g| Box::new(g) as Box<dyn Gamma>))
    };
    start_env(Env { connect, settings, state_file })
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

pub(crate) fn start_env(env: Env) -> (Handle, Receiver<NightlightState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-nightlight".into())
        .spawn(move || smol::block_on(run(rx, state_tx, env)))
        .expect("spawn ashd-svc-nightlight");
    (Handle { tx }, state_rx)
}

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// How often a failed gamma control is requested again.
const GAMMA_RETRY: Duration = Duration::from_secs(30);
/// Longest sleep between schedule checks (clock jumps, suspend / resume).
const MAX_SLEEP: Duration = Duration::from_secs(60);
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);

enum Wake {
    Command(Result<Command, async_channel::RecvError>),
    Input(Result<Input, async_channel::RecvError>),
    Timeout,
}

async fn run(commands: Receiver<Command>, state_tx: Sender<NightlightState>, mut env: Env) {
    let (in_tx, in_rx) = async_channel::unbounded::<Input>();
    let mut core = Core::new(env.settings);
    let mut gamma: Option<Box<dyn Gamma>> = None;
    let mut backoff = RETRY_MIN;
    let mut retry_connect_at = Instant::now();
    let mut retry_gamma_at: Option<Instant> = None;
    let mut persist_at: Option<Instant> = None;
    let mut applied: Option<[f32; 3]> = None;
    let mut last_published: Option<NightlightState> = None;

    let wall = Local::now();
    let mut eval = core.evaluate(Instant::now(), wall, Reason::Startup);

    loop {
        let now = Instant::now();
        if gamma.is_none() && now >= retry_connect_at {
            match (env.connect)(in_tx.clone()) {
                Ok(g) => {
                    tracing::debug!("nightlight: connected");
                    gamma = Some(g);
                    backoff = RETRY_MIN;
                    applied = None;
                }
                Err(e) => {
                    tracing::debug!("nightlight: {e}");
                    retry_connect_at = now + backoff;
                    backoff = (backoff * 2).min(RETRY_MAX);
                }
            }
        }

        // Apply whatever the glide says now.
        let kelvin = core.advance(now);
        let rgb = kelvin_to_rgb(kelvin);
        if let Some(g) = gamma.as_mut()
            && applied != Some(rgb)
        {
            g.apply(rgb);
            applied = Some(rgb);
        }
        if gamma.as_ref().is_some_and(|g| g.has_failed()) {
            retry_gamma_at.get_or_insert(now + GAMMA_RETRY);
        } else {
            retry_gamma_at = None;
        }

        let snapshot = snapshot(&core, &eval, kelvin, gamma.as_deref());
        if last_published.as_ref() != Some(&snapshot) {
            if last_published.as_ref().is_none_or(|l| l.outputs != snapshot.outputs || l.active != snapshot.active || l.mode != snapshot.mode) {
                tracing::info!(
                    "nightlight: mode {} active {} outputs {:?}",
                    snapshot.mode.label(),
                    snapshot.active,
                    snapshot.outputs.iter().map(|o| format!("{}{}", o.name, if o.available { "" } else { " (unavailable)" })).collect::<Vec<_>>()
                );
            }
            let _ = state_tx.force_send(snapshot.clone());
            last_published = Some(snapshot);
        }

        // Next wake-up.
        let mut deadline = None::<Instant>;
        let mut sooner = |t: Instant| deadline = Some(deadline.map_or(t, |d| d.min(t)));
        if core.animating() {
            let remaining = core.glide_ends_at().map_or(Duration::ZERO, |t| t.saturating_duration_since(now));
            let step = if remaining > Duration::from_secs(3) { Duration::from_secs(1) } else { Duration::from_millis(40) };
            sooner(now + step.min(remaining.max(Duration::from_millis(1))));
        } else if let Some(next) = &eval.next_change {
            let wait = (*next - Local::now()).to_std().unwrap_or(Duration::ZERO) + Duration::from_millis(50);
            sooner(now + wait.min(MAX_SLEEP));
        } else {
            sooner(now + MAX_SLEEP);
        }
        if gamma.is_none() {
            sooner(retry_connect_at);
        }
        if let Some(t) = retry_gamma_at {
            sooner(t);
        }
        if let Some(t) = persist_at {
            sooner(t);
        }
        let wait = deadline.map_or(MAX_SLEEP, |d| d.saturating_duration_since(Instant::now()));

        let wake = or(
            async { Wake::Command(commands.recv().await) },
            or(async { Wake::Input(in_rx.recv().await) }, async {
                smol::Timer::after(wait).await;
                Wake::Timeout
            }),
        )
        .await;

        match wake {
            Wake::Command(Err(_)) | Wake::Input(Err(_)) => break,
            Wake::Command(Ok(cmd)) => {
                if cmd == Command::ReloadConfig
                    && let Some(path) = &env.state_file
                {
                    core.settings = Settings::load(path);
                }
                let reason = core.command(&cmd);
                eval = core.evaluate(Instant::now(), Local::now(), reason);
                if env.state_file.is_some() && cmd != Command::ReloadConfig {
                    persist_at = Some(Instant::now() + PERSIST_DEBOUNCE);
                }
            }
            Wake::Input(Ok(Input::OutputsChanged)) => {
                // A new output needs the current table; a name or size changes the state.
                applied = None;
            }
            Wake::Input(Ok(Input::Lost)) => {
                tracing::debug!("nightlight: wayland connection lost");
                gamma = None;
                retry_connect_at = Instant::now() + backoff;
            }
            Wake::Timeout => {
                let now = Instant::now();
                if persist_at.is_some_and(|t| now >= t) {
                    persist_at = None;
                    if let Some(path) = &env.state_file
                        && let Err(e) = settings::save(path, &core.settings)
                    {
                        tracing::warn!("nightlight: cannot save {}: {e}", path.display());
                    }
                }
                if retry_gamma_at.is_some_and(|t| now >= t) {
                    retry_gamma_at = None;
                    if let Some(g) = gamma.as_mut() {
                        g.retry_failed();
                    }
                }
                if !core.animating() {
                    eval = core.evaluate(now, Local::now(), Reason::Clock);
                }
            }
        }
    }

    // Last handle gone: flush a pending save and release the gamma controls (restores
    // the original tables).
    if persist_at.is_some()
        && let Some(path) = &env.state_file
    {
        let _ = settings::save(path, &core.settings);
    }
    drop(gamma);
}

fn snapshot<Tz: TimeZone>(core: &Core, eval: &Eval<Tz>, kelvin: f32, gamma: Option<&dyn Gamma>) -> NightlightState {
    let s = &core.settings;
    let outputs = gamma.map(|g| g.outputs()).unwrap_or_default();
    NightlightState {
        available: outputs.iter().any(|o| o.available),
        enabled: s.enabled(),
        active: eval.active,
        temperature: s.temperature,
        current: kelvin.round() as u32,
        until: if s.mode().is_scheduled() { eval.next_change.as_ref().map(TimeOfDay::of) } else { None },
        mode: s.mode(),
        start: s.schedule.start,
        end: s.schedule.end,
        has_location: s.schedule.latitude.is_some() && s.schedule.longitude.is_some(),
        outputs,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

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

    use chrono::{FixedOffset, TimeZone as _};

    use super::*;

    // ── Core ──

    fn t(h: u32, m: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0).unwrap().with_ymd_and_hms(2024, 11, 14, h, m, 0).unwrap()
    }

    #[test]
    fn a_schedule_edge_glides_for_the_long_transition_and_manual_for_one_second() {
        let t0 = Instant::now();
        let mut core = Core::new(Settings::default());
        // 12:00: outside the default window, nothing to do.
        core.evaluate(t0, t(12, 0), Reason::Startup);
        assert!(!core.animating());
        assert_eq!(core.advance(t0), 6500.0);
        // 21:00 reached by the clock: 45 s glide.
        let ev = core.evaluate(t0, t(21, 0), Reason::Clock);
        assert!(ev.active);
        assert!(core.animating());
        assert_eq!(core.glide_ends_at(), Some(t0 + DEFAULT_SCHEDULE_TRANSITION));
        let mid = core.advance(t0 + Duration::from_secs(22));
        assert!(mid < 6500.0 && mid > 3400.0, "{mid}");
        assert_eq!(core.advance(t0 + Duration::from_secs(46)), 3400.0);
        // A hand toggle off: 1 s.
        let t1 = t0 + Duration::from_secs(60);
        core.command(&Command::Toggle);
        core.evaluate(t1, t(21, 5), Reason::Manual);
        assert_eq!(core.glide_ends_at(), Some(t1 + DEFAULT_MANUAL_TRANSITION));
        assert_eq!(core.advance(t1 + Duration::from_secs(2)), 6500.0);
    }

    #[test]
    fn toggle_remembers_the_mode() {
        let mut core = Core::new(Settings::default());
        core.command(&Command::SetMode(Mode::Sun));
        assert!(core.settings.enabled());
        core.command(&Command::Toggle);
        assert_eq!(core.settings.mode(), Mode::Off);
        core.command(&Command::Toggle);
        assert_eq!(core.settings.mode(), Mode::Sun, "back to the last non-off mode");
        core.command(&Command::SetEnabled(false));
        core.command(&Command::SetEnabled(false));
        assert_eq!(core.settings.mode(), Mode::Off);
        core.command(&Command::SetEnabled(true));
        assert_eq!(core.settings.mode(), Mode::Sun);
        assert_eq!(core.command(&Command::SetTemperature(100)), Reason::Adjust);
        assert_eq!(core.settings.temperature, MIN_K);
        core.command(&Command::SetTemperature(9000));
        assert_eq!(core.settings.temperature, NEUTRAL_K);
    }

    #[test]
    fn adjusting_the_temperature_while_tinted_retargets_quickly() {
        let t0 = Instant::now();
        let mut s = Settings::default();
        s.set_mode(Mode::On);
        let mut core = Core::new(s);
        core.evaluate(t0, t(12, 0), Reason::Startup);
        core.advance(t0 + Duration::from_secs(5));
        let t1 = t0 + Duration::from_secs(10);
        core.command(&Command::SetTemperature(2500));
        core.evaluate(t1, t(12, 0), Reason::Adjust);
        assert_eq!(core.glide_ends_at(), Some(t1 + ADJUST_TRANSITION));
        assert_eq!(core.advance(t1 + Duration::from_secs(1)), 2500.0);
    }

    // ── the service against a fake compositor ──

    #[derive(Default)]
    struct Fake {
        applied: Vec<[f32; 3]>,
        outputs: Vec<OutputStatus>,
        failed: bool,
        retries: u32,
        input: Option<Sender<Input>>,
        dropped: bool,
    }

    struct FakeGamma(Arc<Mutex<Fake>>);
    impl Gamma for FakeGamma {
        fn apply(&mut self, rgb: [f32; 3]) {
            self.0.lock().unwrap().applied.push(rgb);
        }
        fn outputs(&self) -> Vec<OutputStatus> {
            self.0.lock().unwrap().outputs.clone()
        }
        fn retry_failed(&mut self) -> bool {
            self.0.lock().unwrap().retries += 1;
            true
        }
        fn has_failed(&self) -> bool {
            self.0.lock().unwrap().failed
        }
    }
    impl Drop for FakeGamma {
        fn drop(&mut self) {
            self.0.lock().unwrap().dropped = true;
        }
    }

    fn out(name: &str, available: bool) -> OutputStatus {
        OutputStatus { name: name.into(), available }
    }

    fn fake_service(settings: Settings, state_file: Option<PathBuf>, outputs: Vec<OutputStatus>) -> (Handle, Watch<NightlightState>, Arc<Mutex<Fake>>) {
        let shared = Arc::new(Mutex::new(Fake { outputs, ..Fake::default() }));
        let s = shared.clone();
        let (h, rx) = start_env(Env {
            connect: Box::new(move |tx| {
                s.lock().unwrap().input = Some(tx);
                Ok(Box::new(FakeGamma(s.clone())))
            }),
            settings,
            state_file,
        });
        (h, Watch::new(rx), shared)
    }

    fn fast() -> Settings {
        Settings { manual_transition: Duration::from_millis(120), ..Settings::default() }
    }


    #[test]
    fn toggling_glides_the_ramp_to_the_night_temperature_and_back() {
        let mut s = fast();
        s.last_mode = Mode::On; // independent of the time of day
        s.set_mode(Mode::Off);
        s.last_mode = Mode::On;
        let (h, mut rx, fake) = fake_service(s, None, vec![out("DP-1", true)]);
        let st = rx.until("connected", |s| s.available);
        assert!(!st.enabled && !st.active);
        assert_eq!(st.current, NEUTRAL_K);

        h.toggle();
        rx.until("on and settled", |s| s.active && s.current == 3400);
        {
            let f = fake.lock().unwrap();
            assert_eq!(f.applied.first(), Some(&[1.0, 1.0, 1.0]));
            assert_eq!(f.applied.last(), Some(&kelvin_to_rgb(3400.0)));
            assert!(f.applied.len() >= 3, "intermediate steps were applied: {}", f.applied.len());
            let blues: Vec<f32> = f.applied.iter().map(|c| c[2]).collect();
            assert!(blues.windows(2).all(|w| w[1] <= w[0] + 1e-6), "monotonic glide {blues:?}");
        }
        let st = rx.until("state", |s| s.mode == Mode::On);
        assert!(st.enabled && st.active && st.until.is_none());

        h.toggle();
        let st = rx.until("off again", |s| !s.enabled && s.current == NEUTRAL_K);
        assert_eq!(st.until, None);
        assert_eq!(fake.lock().unwrap().applied.last(), Some(&[1.0, 1.0, 1.0]));
    }

    #[test]
    fn manual_on_has_no_until_and_scheduled_modes_do() {
        let mut s = fast();
        s.set_mode(Mode::On);
        let (h, mut rx, _fake) = fake_service(s, None, vec![out("DP-1", true)]);
        let st = rx.until("tinted", |s| s.active && s.current == 3400);
        assert_eq!((st.mode, st.until), (Mode::On, None));
        h.set_mode(Mode::Fixed);
        let st = rx.until("fixed", |s| s.mode == Mode::Fixed);
        assert!(st.until.is_some_and(|t| t == st.start || t == st.end), "{st:?}");
    }

    #[test]
    fn temperature_changes_reach_the_outputs() {
        let mut s = fast();
        s.set_mode(Mode::On);
        let (h, mut rx, fake) = fake_service(s, None, vec![out("DP-1", true)]);
        rx.until("tinted", |s| s.current == 3400);
        h.set_temperature(2500);
        rx.until("2500 K", |s| s.current == 2500 && s.temperature == 2500);
        assert_eq!(fake.lock().unwrap().applied.last(), Some(&kelvin_to_rgb(2500.0)));
    }

    #[test]
    fn a_refused_output_is_unavailable_and_retried_later() {
        let (_h, mut rx, fake) = fake_service(fast(), None, vec![out("DP-1", false)]);
        let st = rx.until("first snapshot", |_| true);
        assert!(!st.available);
        assert_eq!(st.outputs, vec![out("DP-1", false)]);
        // The compositor later grants it.
        {
            let mut f = fake.lock().unwrap();
            f.outputs = vec![out("DP-1", true)];
            f.input.as_ref().unwrap().try_send(Input::OutputsChanged).unwrap();
        }
        rx.until("available after hotplug event", |s| s.available);
        // The new table was applied on that event.
        assert!(!fake.lock().unwrap().applied.is_empty());
    }

    #[test]
    fn hotplug_reapplies_the_current_table() {
        let mut s = fast();
        s.set_mode(Mode::On);
        let (_h, mut rx, fake) = fake_service(s, None, vec![out("DP-1", true)]);
        rx.until("tinted", |s| s.current == 3400);
        let before = fake.lock().unwrap().applied.len();
        {
            let mut f = fake.lock().unwrap();
            f.outputs.push(out("HDMI-A-1", true));
            f.input.as_ref().unwrap().try_send(Input::OutputsChanged).unwrap();
        }
        rx.until("second output listed", |s| s.outputs.len() == 2);
        let f = fake.lock().unwrap();
        assert!(f.applied.len() > before);
        assert_eq!(f.applied.last(), Some(&kelvin_to_rgb(3400.0)));
    }

    #[test]
    fn dropping_the_last_handle_releases_the_gamma_controls_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nightlight.json");
        let mut s = fast();
        s.set_mode(Mode::Off);
        let (h, mut rx, fake) = fake_service(s, Some(path.clone()), vec![out("DP-1", true)]);
        rx.until("connected", |s| s.available);
        h.set_temperature(2800);
        h.set_mode(Mode::On);
        rx.until("applied", |s| s.mode == Mode::On && s.temperature == 2800);
        drop(h);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fake.lock().unwrap().dropped && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(fake.lock().unwrap().dropped, "gamma controls destroyed on shutdown");
        let back = Settings::load_from(&[] as &[PathBuf], &path);
        assert_eq!((back.mode(), back.temperature), (Mode::On, 2800), "pending changes are flushed on shutdown");
    }

    #[test]
    fn a_lost_connection_reconnects_and_reapplies() {
        let mut s = fast();
        s.set_mode(Mode::On);
        let (_h, mut rx, fake) = fake_service(s, None, vec![out("DP-1", true)]);
        rx.until("tinted", |s| s.current == 3400 && s.available);
        let tx = fake.lock().unwrap().input.clone().unwrap();
        tx.try_send(Input::Lost).unwrap();
        rx.until("unavailable", |s| !s.available);
        rx.until("reconnected", |s| s.available);
        assert_eq!(fake.lock().unwrap().applied.last(), Some(&kelvin_to_rgb(3400.0)));
    }
}
