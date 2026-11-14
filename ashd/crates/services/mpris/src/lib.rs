//! MPRIS: media players on the session bus (`org.mpris.MediaPlayer2.*`, zbus 5). No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<MprisState>)`,
//! newest-snapshot channel; commands are fire-and-forget and their effect comes back as
//! a new [`MprisState`]. Owner: media.
//!
//! * **Discovery**: `ListNames` once, then `NameOwnerChanged`. Each player gets a watcher
//!   task (own `PropertiesChanged` / `Seeked` / `TrackList` subscriptions on the owner's
//!   unique name), so one hung player never stalls the others. `playerctld` (the proxy
//!   that mirrors the "current" player) is ignored: it would only duplicate a real one.
//! * **Active player**: the pinned one ([`Handle::set_active`]) while it exists, else the
//!   player that most recently *started* playing, else the one that most recently
//!   changed. Commands target the active player unless a bus name is given.
//! * **Position**: MPRIS does not signal it, so while a player is `Playing` it is polled
//!   at 1 Hz (never faster) plus on `Seeked`; [`Handle::set_position_polling`] turns the
//!   polling off while nobody looks (the panel does so when it closes).
//! * **Queue**: `org.mpris.MediaPlayer2.TrackList` when the player implements it, else
//!   an empty [`Player::queue`] (and `has_track_list == false`).
//! * **Album art**: `file://` URLs become [`Track::art_path`]; `http(s)` URLs are *not*
//!   fetched (no HTTP client in the workspace dependencies) — consumers show their
//!   placeholder and may still read [`Track::art_url`].
//! * `available == false` while there is no session bus (reconnects with back-off).
//!
//! Layout: `model` is the pure core (merging updates, picking the active player;
//! unit-tested), `parse` turns D-Bus values into plain data, `service` is the bus side.
//!
//! Testing: `tests/bus.rs` runs two mock players on a private `dbus-daemon` (never the
//! user's bus); the mock (`tests/support/mock.rs`) is also the `mock_player` example that
//! feeds the panel in the nested session.

mod model;
mod parse;
mod service;

use std::{path::PathBuf, time::Duration};

use async_channel::{Receiver, Sender};

pub use parse::art_path_from_url;

/// Every player's bus name starts with this.
pub const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Position polling period while a player is playing.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Longest queue read from a `TrackList` (the UI shows a handful of rows).
pub const MAX_QUEUE: usize = 100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl PlaybackStatus {
    /// MPRIS spelling (`"Playing"`, `"Paused"`, `"Stopped"`); anything else is `Stopped`.
    pub fn parse(s: &str) -> Self {
        match s {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// A track's metadata (`Metadata`, or one `GetTracksMetadata` entry).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    /// `mpris:trackid` (an object path; some players send a plain string).
    pub id: Option<String>,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    /// `mpris:artUrl` as sent.
    pub art_url: Option<String>,
    /// Local file for `file://` art URLs (checked to exist, current track only).
    pub art_path: Option<PathBuf>,
    /// `mpris:length`.
    pub length: Option<Duration>,
    /// `xesam:url`.
    pub url: Option<String>,
}

impl Track {
    /// Nothing known about the track (no title, artist, album or id).
    pub fn is_blank(&self) -> bool {
        self.title.is_empty() && self.artists.is_empty() && self.album.is_empty() && self.id.is_none()
    }

    /// `"artist, artist"`.
    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Player {
    /// Full well-known name, `org.mpris.MediaPlayer2.mpd` (the key for commands).
    pub bus_name: String,
    /// `Identity` ("Music Player Daemon"); empty if the player does not say.
    pub identity: String,
    /// `DesktopEntry` (without `.desktop`).
    pub desktop_entry: Option<String>,
    pub status: PlaybackStatus,
    pub track: Track,
    /// Position when the snapshot was taken (polled at ≤ 1 Hz while playing).
    pub position: Duration,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub can_control: bool,
    /// `Volume` (1.0 = 100 %), if the player has one.
    pub volume: Option<f64>,
    /// The player implements `org.mpris.MediaPlayer2.TrackList`.
    pub has_track_list: bool,
    /// At most [`MAX_QUEUE`] tracks, in order; empty without a track list.
    pub queue: Vec<Track>,
}

impl Player {
    /// `mpd` for `org.mpris.MediaPlayer2.mpd`.
    pub fn short_name(&self) -> &str {
        self.bus_name.strip_prefix(BUS_PREFIX).unwrap_or(&self.bus_name)
    }

    /// `identity`, else the short bus name.
    pub fn display_name(&self) -> &str {
        if self.identity.is_empty() { self.short_name() } else { &self.identity }
    }

    pub fn is_playing(&self) -> bool {
        self.status == PlaybackStatus::Playing
    }

    /// Index of the current track in [`Self::queue`] (by track id).
    pub fn queue_index(&self) -> Option<usize> {
        let id = self.track.id.as_deref()?;
        self.queue.iter().position(|t| t.id.as_deref() == Some(id))
    }

    /// Position as a fraction of the track length (0 when the length is unknown).
    pub fn progress(&self) -> f32 {
        match self.track.length {
            Some(len) if !len.is_zero() => (self.position.as_secs_f32() / len.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MprisState {
    /// Connected to the session bus (there may still be no player).
    pub available: bool,
    /// Sorted by bus name.
    pub players: Vec<Player>,
    /// Bus name of the active player.
    pub active: Option<String>,
    /// Bus name the user pinned with [`Handle::set_active`] (only while that player exists).
    pub pinned: Option<String>,
}

impl MprisState {
    pub fn active_player(&self) -> Option<&Player> {
        self.player(self.active.as_deref()?)
    }

    pub fn player(&self, bus_name: &str) -> Option<&Player> {
        self.players.iter().find(|p| p.bus_name == bus_name)
    }
}

/// `None` targets the active player; `Some(bus name)` a specific one.
pub type Target = Option<String>;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    PlayPause(Target),
    Next(Target),
    Previous(Target),
    Stop(Target),
    /// Absolute position (`SetPosition`, or a relative `Seek` for players without a
    /// track id). Ignored for players that cannot seek.
    SeekTo { player: Target, position: Duration },
    /// `TrackList.GoTo(track id)`.
    GoTo { player: Target, track_id: String },
    /// Pin a player (`Some`) so it stays active, or unpin (`None`).
    SetActive(Option<String>),
    /// Poll the position of playing players (default on).
    SetPositionPolling(bool),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        if self.tx.try_send(cmd).is_err() {
            tracing::debug!("mpris service is not running");
        }
    }
    pub fn play_pause(&self) {
        self.send(Command::PlayPause(None));
    }
    pub fn next(&self) {
        self.send(Command::Next(None));
    }
    pub fn previous(&self) {
        self.send(Command::Previous(None));
    }
    pub fn stop(&self) {
        self.send(Command::Stop(None));
    }
    pub fn seek_to(&self, position: Duration) {
        self.send(Command::SeekTo { player: None, position });
    }
    pub fn go_to(&self, track_id: impl Into<String>) {
        self.send(Command::GoTo { player: None, track_id: track_id.into() });
    }
    /// Pin `bus_name` as the active player.
    pub fn set_active(&self, bus_name: impl Into<String>) {
        self.send(Command::SetActive(Some(bus_name.into())));
    }
    pub fn unpin(&self) {
        self.send(Command::SetActive(None));
    }
    pub fn set_position_polling(&self, on: bool) {
        self.send(Command::SetPositionPolling(on));
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Spawn the `ashd-svc-mpris` thread on the session bus (`$DBUS_SESSION_BUS_ADDRESS`).
pub fn start() -> (Handle, Receiver<MprisState>) {
    start_at(None)
}

/// Like [`start`], on an explicit bus address (tests, the nested session).
pub fn start_at(address: Option<String>) -> (Handle, Receiver<MprisState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-mpris".into())
        .spawn(move || smol::block_on(service::run(rx, state_tx, address)))
        .expect("spawn ashd-svc-mpris");
    (Handle { tx }, state_rx)
}

pub(crate) const RETRY_MIN: Duration = Duration::from_millis(500);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(10);
