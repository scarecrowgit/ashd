//! niri IPC for ashd (no GPUI).
//!
//! - [`NiriState`]: plain-data snapshot of the compositor (workspaces, windows, focus,
//!   window layouts, keyboard layouts, overview, outputs), kept up to date by
//!   [`NiriState::apply`] — niri-ipc's own event-stream reducers, so it stays correct when
//!   the vendored `niri-ipc` is re-synced.
//! - [`start`]: spawns the `ashd-niri` thread that follows `$NIRI_SOCKET`'s event stream
//!   (reconnecting with backoff) and publishes a fresh `NiriState` after every event
//!   batch; returns a [`NiriHandle`] for actions/requests.
//! - [`strip`]: derives the scrolling-strip columns (order + widths) from
//!   `WindowLayout::{pos_in_scrolling_layout, tile_size}`.
//! - [`canvas`]: canvas-mode helpers (ashd fork): plane windows/bounds and [`LiveViews`],
//!   the per-frame canvas/strip view state. View events (`CanvasViewChanged`,
//!   `StripViewChanged`, up to one per output frame) do not republish `NiriState`; they
//!   go to [`NiriHandle::subscribe_views`] subscribers, coalesced to at most one publish
//!   per [`VIEWS_MIN_INTERVAL`]. Against a stock niri no view events arrive and every
//!   workspace reports `WorkspaceMode::Strip`.
//! - [`blocking`]: std-only helpers for the CLI (`ashd app quit`).
//!
//! The vendored `niri_ipc` crate is re-exported; use `ashd_niri::niri_ipc::Action` etc.
//! so every crate sees one copy of the types.

pub mod blocking;
pub mod canvas;
mod service;
mod state;
pub mod strip;

pub use canvas::{CanvasWindow, LiveViews, PlaneRect};
pub use niri_ipc;
pub use service::{NiriHandle, REQUEST_TIMEOUT, VIEWS_MIN_INTERVAL, is_live_view_event, start, start_with};
pub use state::NiriState;
pub use strip::{Strip, StripColumn};

/// Env var with the niri socket path (set by niri for its children).
pub const SOCKET_ENV: &str = niri_ipc::socket::SOCKET_PATH_ENV;

#[derive(Debug, thiserror::Error)]
pub enum NiriError {
    #[error("$NIRI_SOCKET is not set (not running under niri?)")]
    NoSocket,
    #[error("niri ipc i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("niri ipc json: {0}")]
    Json(#[from] serde_json::Error),
    /// niri answered `Err(message)`.
    #[error("niri: {0}")]
    Niri(String),
    #[error("unexpected niri response: {0:?}")]
    Unexpected(Box<niri_ipc::Response>),
    #[error("the ashd-niri thread is gone")]
    Gone,
}

/// `niri_ipc::Action::Spawn` for an argv. This is how the shell launches apps: niri
/// spawns them with its own environment, so nothing from the shell's process (e.g. a dev
/// `LD_LIBRARY_PATH`) leaks into user apps.
pub fn spawn_action(argv: impl IntoIterator<Item = impl Into<String>>) -> niri_ipc::Action {
    niri_ipc::Action::Spawn { command: argv.into_iter().map(Into::into).collect() }
}

/// `niri_ipc::Action::SpawnSh` for a shell command line.
pub fn spawn_sh_action(command: impl Into<String>) -> niri_ipc::Action {
    niri_ipc::Action::SpawnSh { command: command.into() }
}
