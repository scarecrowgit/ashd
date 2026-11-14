//! Audio: PulseAudio / pipewire-pulse sinks, sources, volume and mute. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<AudioState>)`,
//! newest-snapshot channel. Commands are fire-and-forget; the resulting change comes
//! back as a new `AudioState` (the model is updated optimistically, so repeated
//! `step_volume` calls compose before the server answers). Owner: quick settings.
//!
//! Layout: `model` is the pure core (commands → actions, volume maths; unit-tested),
//! `pulse` the libpulse backend (threaded mainloop; its callbacks only post events).
//! The service reconnects with a back-off when the server goes away and publishes
//! `available: false` meanwhile. It never autospawns a pulse daemon.
//!
//! Testing: the nested session may share the user's pulse server — reading is fine, but
//! never send mutating commands to it from automated tests. `tests/private_server.rs`
//! drives a throw-away `pulseaudio` with null sinks instead (opt-in, see there).

mod model;
mod pulse;

use std::time::Duration;

use async_channel::{Receiver, Sender};
use libpulse_binding::context::State as PaState;
use smol::future::or;

use crate::{
    model::Model,
    pulse::{Event, Pulse},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    #[default]
    Other,
    Speakers,
    Headphones,
    Headset,
    Hdmi,
    Bluetooth,
    Microphone,
}

/// A sink (output) or source (input). Monitor sources are not listed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    /// Pulse index (changes when the device is re-added).
    pub index: u32,
    /// Pulse name (stable, used for `set_default`).
    pub name: String,
    /// Human description ("Built-in Audio Analog Stereo").
    pub description: String,
    /// Average channel volume; 1.0 = 100 %, may exceed 1.0 (up to [`MAX_VOLUME`]).
    pub volume: f32,
    pub muted: bool,
    pub is_default: bool,
    pub kind: DeviceKind,
    /// Description of the active port ("Headphones", "HDMI / DisplayPort 3"), if any.
    pub port: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioState {
    /// Connected to the pulse server.
    pub available: bool,
    pub sinks: Vec<Device>,
    pub sources: Vec<Device>,
}

impl AudioState {
    pub fn default_sink(&self) -> Option<&Device> {
        self.sinks.iter().find(|d| d.is_default)
    }
    pub fn default_source(&self) -> Option<&Device> {
        self.sources.iter().find(|d| d.is_default)
    }
}

/// Upper bound for `set_volume` / `step_volume` (150 %).
pub const MAX_VOLUME: f32 = 1.5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    DefaultSink,
    DefaultSource,
    Sink(String),
    Source(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetVolume(Target, f32),
    /// Relative change in percent points, clamped to 0..=100 % unless already above.
    StepVolume(Target, i32),
    SetMute(Target, bool),
    ToggleMute(Target),
    /// Make the sink/source with this pulse name the default.
    SetDefault(Target),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        if self.tx.try_send(cmd).is_err() {
            tracing::debug!("audio service is not running");
        }
    }
    pub fn set_volume(&self, target: Target, volume: f32) {
        self.send(Command::SetVolume(target, volume.clamp(0.0, MAX_VOLUME)));
    }
    pub fn step_volume(&self, target: Target, percent: i32) {
        self.send(Command::StepVolume(target, percent));
    }
    pub fn set_mute(&self, target: Target, muted: bool) {
        self.send(Command::SetMute(target, muted));
    }
    pub fn toggle_mute(&self, target: Target) {
        self.send(Command::ToggleMute(target));
    }
    pub fn set_default(&self, target: Target) {
        self.send(Command::SetDefault(target));
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Spawn the `ashd-svc-audio` thread (libpulse threaded mainloop + command loop). The
/// server is the default one (`$PULSE_SERVER`, else the user's runtime dir).
pub fn start() -> (Handle, Receiver<AudioState>) {
    start_with_server(None)
}

/// Like [`start`], for an explicit server address (`unix:/path/native`, tests).
pub fn start_with_server(server: Option<String>) -> (Handle, Receiver<AudioState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-audio".into())
        .spawn(move || smol::block_on(run(rx, state_tx, server)))
        .expect("spawn ashd-svc-audio");
    (Handle { tx }, state_rx)
}

const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(5);

enum End {
    /// Every `Handle` is gone: stop the service.
    Closed,
    /// The server went away (or never answered): reconnect.
    Lost,
}

async fn run(commands: Receiver<Command>, state: Sender<AudioState>, server: Option<String>) {
    let mut model = Model::default();
    let _ = state.force_send(model.state());
    let mut backoff = RETRY_MIN;
    loop {
        // Commands queued while there is no server are meaningless afterwards.
        while commands.try_recv().is_ok() {}
        match session(&commands, &state, &mut model, server.as_deref()).await {
            End::Closed => return,
            End::Lost => {}
        }
        if model.available {
            backoff = RETRY_MIN;
        }
        model = Model::default();
        let _ = state.force_send(model.state());
        // Wait before reconnecting, but notice when the last handle is dropped.
        let closed = or(
            async {
                smol::Timer::after(backoff).await;
                false
            },
            async {
                loop {
                    if commands.recv().await.is_err() {
                        return true;
                    }
                }
            },
        )
        .await;
        if closed {
            return;
        }
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

enum Wake {
    Command(Result<Command, async_channel::RecvError>),
    Event(Result<Event, async_channel::RecvError>),
}

/// One connection's lifetime.
async fn session(commands: &Receiver<Command>, state: &Sender<AudioState>, model: &mut Model, server: Option<&str>) -> End {
    let (ev_tx, ev_rx) = async_channel::unbounded::<Event>();
    let mut pulse = match Pulse::connect(server, ev_tx.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("audio: {e}");
            return End::Lost;
        }
    };
    let (mut got_sinks, mut got_sources, mut got_defaults) = (false, false, false);
    loop {
        let wake = or(async { Wake::Command(commands.recv().await) }, async { Wake::Event(ev_rx.recv().await) }).await;
        match wake {
            Wake::Command(Err(_)) => return End::Closed,
            Wake::Command(Ok(cmd)) => {
                let actions = model.apply(cmd);
                if !actions.is_empty() {
                    for a in actions {
                        pulse.exec(a);
                    }
                    let _ = state.force_send(model.state());
                }
            }
            Wake::Event(Err(_)) => return End::Lost,
            Wake::Event(Ok(ev)) => {
                match ev {
                    Event::StateChanged => match pulse.state() {
                        PaState::Ready => {
                            tracing::debug!("audio: connected");
                            pulse.subscribe();
                            pulse.refresh(&ev_tx);
                        }
                        PaState::Failed | PaState::Terminated => {
                            tracing::debug!("audio: connection lost");
                            return End::Lost;
                        }
                        _ => {}
                    },
                    Event::Changed => {
                        pulse.refresh(&ev_tx);
                    }
                    Event::Sinks(v) => {
                        model.sinks = v;
                        got_sinks = true;
                    }
                    Event::Sources(v) => {
                        model.sources = v;
                        got_sources = true;
                    }
                    Event::Defaults { sink, source } => {
                        model.default_sink = sink;
                        model.default_source = source;
                        got_defaults = true;
                    }
                }
                // `available` only once the first full picture is in.
                if got_sinks && got_sources && got_defaults {
                    model.available = true;
                }
                if model.available {
                    let _ = state.force_send(model.state());
                }
            }
        }
    }
}
