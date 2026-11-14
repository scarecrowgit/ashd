//! The `ashd` CLI ↔ `ashd-shell` protocol.
//!
//! Transport: a Unix stream socket at [`socket_path`] (`$ASHD_SOCKET`, else
//! `$XDG_RUNTIME_DIR/ashd/shell.sock`). Framing: newline-delimited JSON. The client
//! writes one [`Request`] per line; the server answers each with exactly one
//! [`Response`] line, in order. A connection may carry any number of requests.
//!
//! ```text
//! → {"osd":{"volume":{"step":5}}}
//! ← "ok"
//! → "ping"
//! ← {"pong":{"version":"0.1.0","protocol":1,"pid":4242}}
//! ```
//!
//! Client side: [`Client`] (blocking, std only) or the one-shot [`request`].
//! Server side: [`Server::bind`] + [`Server::run`] forwards every request as an
//! [`Incoming`] over an `async_channel`; the receiver (ashd-shell-core, on the GPUI main
//! thread) answers with [`Incoming::respond`].

mod client;
mod server;
mod sock;

use std::{ffi::OsString, path::PathBuf};

pub use client::{Client, is_running, request};
use ashd_tokens::ThemeName;
use serde::{Deserialize, Serialize};
pub use server::{Incoming, Server};

/// Bumped on any incompatible change to [`Request`] / [`Response`].
pub const PROTOCOL_VERSION: u32 = 1;

/// Env var overriding the socket path (tests, nested sessions).
pub const SOCKET_ENV: &str = "ASHD_SOCKET";

/// Longest line (request or response, without the newline) either side accepts.
pub const MAX_LINE: usize = 64 * 1024;

/// `$XDG_RUNTIME_DIR/ashd`, else `/tmp/ashd-$UID`: the directory [`socket_path`] uses when
/// `$ASHD_SOCKET` is not set. The server keeps it at mode 0700.
pub(crate) fn default_socket_dir() -> Option<PathBuf> {
    Some(resolve_socket_dir(std::env::var_os("XDG_RUNTIME_DIR"), rustix::process::getuid().as_raw()))
}

fn resolve_socket_dir(xdg_runtime_dir: Option<OsString>, uid: u32) -> PathBuf {
    match xdg_runtime_dir.filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("ashd"),
        None => PathBuf::from(format!("/tmp/ashd-{uid}")),
    }
}

fn resolve_socket_path(socket: Option<OsString>, xdg_runtime_dir: Option<OsString>, uid: u32) -> PathBuf {
    match socket.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => resolve_socket_dir(xdg_runtime_dir, uid).join("shell.sock"),
    }
}

/// `$ASHD_SOCKET`, else `$XDG_RUNTIME_DIR/ashd/shell.sock`, else `/tmp/ashd-$UID/shell.sock`.
pub fn socket_path() -> PathBuf {
    resolve_socket_path(
        std::env::var_os(SOCKET_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
        rustix::process::getuid().as_raw(),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// No shell is listening (socket missing or connection refused).
    #[error("ashd-shell is not running (no listener on {0})")]
    NotRunning(PathBuf),
    #[error("ipc i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipc protocol: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error("ashd-shell closed the connection without answering")]
    Closed,
    /// The shell answered with [`Response::Error`].
    #[error("ashd-shell: {0}")]
    Remote(String),
}

impl IpcError {
    /// No shell is listening.
    pub fn is_not_running(&self) -> bool {
        matches!(self, Self::NotRunning(_))
    }

    /// The shell did not answer within [`Client::TIMEOUT`].
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Io(e) if e.kind() == std::io::ErrorKind::TimedOut)
    }
}

// ── requests ──────────────────────────────────────────────────────────────────

/// What to do with a toggleable panel. The CLI default is `Toggle`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum PanelOp {
    #[default]
    Toggle,
    Open,
    Close,
}

/// Launcher prefix modes (det-04): `=` calc, `/` files, `>` run, `:` emoji, `!` web.
/// `None` in [`Request::Launcher`] means apps.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum LauncherMode {
    Calc,
    Files,
    Run,
    Emoji,
    Web,
}

impl LauncherMode {
    /// The query prefix that selects this mode in the launcher input.
    pub fn prefix(self) -> char {
        match self {
            Self::Calc => '=',
            Self::Files => '/',
            Self::Run => '>',
            Self::Emoji => ':',
            Self::Web => '!',
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum VolumeChange {
    /// Relative change in percent points (`+5`, `-5`).
    Step(i32),
    /// Toggle mute of the default sink.
    Mute,
}

/// Change a value *and* show the OSD (bottom-centre, 1.2 s). Sent by keybinds.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OsdRequest {
    Volume(VolumeChange),
    /// Toggle mute of the default source.
    MicMute,
    /// Relative backlight change in percent points.
    Brightness(i32),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScratchOp {
    /// Hide the focused window: send it to the compositor's hidden scratch workspace
    /// (niri `hide-window`).
    Hide,
    /// Bring back the most recently hidden window (niri `unhide-window`).
    Show,
    /// Show / hide the scratchpad *panel* (the right-hand surface).
    Toggle,
    /// Show the panel (`ashd scratch open`).
    Open,
    /// Hide the panel (`ashd scratch close`).
    Close,
    /// Hide the window with this niri id (`ashd scratch hide --id N`).
    HideWindow(u64),
    /// Bring back the hidden window with this niri id (`ashd scratch show --id N`).
    ShowWindow(u64),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CanvasOp {
    /// Switch the focused workspace between Scroll and Canvas mode.
    Toggle,
    ZoomIn,
    ZoomOut,
    Fit,
    Home,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeRequest {
    /// → [`Response::Theme`] with the shell's current theme.
    Get,
    /// Switch the shell's surfaces (300 ms crossfade). Persisting the choice and writing
    /// the generated theme files is the CLI's job (`ashd theme set` does both).
    Set(ThemeName),
    /// Next theme in [`ThemeName::ALL`] order; → [`Response::Theme`] with the new one.
    Cycle,
}

/// What the media keys do to the active MPRIS player.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MediaControl {
    PlayPause,
    Next,
    Previous,
    Stop,
}

/// Everything the CLI can ask the shell. Panel requests are routed to the panel that
/// claims them (ARCHITECTURE.md "Requests"); unclaimed ones get [`Response::Error`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Request {
    /// → [`Response::Pong`].
    Ping,
    /// → [`Response::Status`]. Introspection for tests / `cargo xtask`.
    Status,
    /// Close the open modal/popup, if any.
    Dismiss,
    /// Quit the shell (all surfaces).
    Quit,

    Launcher {
        #[serde(default)]
        op: PanelOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<LauncherMode>,
    },
    Clipboard {
        #[serde(default)]
        op: PanelOp,
    },
    /// Notification centre.
    Notifications {
        #[serde(default)]
        op: PanelOp,
    },
    QuickSettings {
        #[serde(default)]
        op: PanelOp,
    },
    /// Calendar popup (anchored to the clock).
    Calendar {
        #[serde(default)]
        op: PanelOp,
    },
    /// Session / power menu.
    Session {
        #[serde(default)]
        op: PanelOp,
    },
    Cheatsheet {
        #[serde(default)]
        op: PanelOp,
    },
    /// Theme switcher (five theme cards, live preview, "apply to" consumers). `ashd theme pick`.
    ThemeSwitcher {
        #[serde(default)]
        op: PanelOp,
    },
    Osd(OsdRequest),
    Scratch(ScratchOp),
    Canvas(CanvasOp),
    Theme(ThemeRequest),
    /// Lock the session now (the shell spawns `ashd-lock`).
    Lock,
    /// Media popup (`ashd media [--open|--close]`).
    Media {
        #[serde(default)]
        op: PanelOp,
    },
    /// Media keys: control the active player; answered even while the popup is closed
    /// (`ashd media play-pause|next|previous|stop`).
    MediaControl(MediaControl),
    /// Screenshot / recording overlay (`ashd screenshot [--region|--window|--output]
    /// [--record]`).
    Screenshot(ScreenshotRequest),
    /// Stop (or toggle) the running screen recording (`ashd record stop|toggle`).
    Record(RecordOp),
}

/// What the screenshot overlay captures.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMode {
    /// A rectangle the user drags.
    Region,
    /// The window under the pointer.
    Window,
    /// A whole output.
    Output,
}

/// [`Request::Screenshot`]: open the capture overlay (or close / re-aim it).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenshotRequest {
    /// Start in this mode; `None` keeps the last one used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<CaptureMode>,
    /// Start in "Record" instead of "Shot". While a recording runs, a toggle with
    /// `record` stops it.
    #[serde(default)]
    pub record: bool,
    #[serde(default)]
    pub op: PanelOp,
}

/// [`Request::Record`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RecordOp {
    /// Stop the recording (an error if none runs).
    Stop,
    /// Stop the recording if one runs, else open the overlay in "Record".
    Toggle,
}

impl Request {
    /// Stable short name for logs and errors (`quick-settings`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Status => "status",
            Self::Dismiss => "dismiss",
            Self::Quit => "quit",
            Self::Launcher { .. } => "launcher",
            Self::Clipboard { .. } => "clipboard",
            Self::Notifications { .. } => "notifications",
            Self::QuickSettings { .. } => "quick-settings",
            Self::Calendar { .. } => "calendar",
            Self::Session { .. } => "session",
            Self::Cheatsheet { .. } => "cheatsheet",
            Self::ThemeSwitcher { .. } => "theme-switcher",
            Self::Osd(_) => "osd",
            Self::Scratch(_) => "scratch",
            Self::Canvas(_) => "canvas",
            Self::Theme(_) => "theme",
            Self::Lock => "lock",
            Self::Media { .. } => "media",
            Self::MediaControl(_) => "media-control",
            Self::Screenshot(_) => "screenshot",
            Self::Record(_) => "record",
        }
    }
}

// ── responses ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Response {
    Ok,
    Pong { version: String, protocol: u32, pid: u32 },
    Theme(ThemeName),
    Status(ShellStatus),
    Error(String),
}

impl Response {
    pub fn error(msg: impl Into<String>) -> Self {
        Self::Error(msg.into())
    }

    /// `Err(IpcError::Remote)` for [`Response::Error`], `Ok(self)` otherwise.
    pub fn into_result(self) -> Result<Response, IpcError> {
        match self {
            Self::Error(e) => Err(IpcError::Remote(e)),
            other => Ok(other),
        }
    }
}

/// Snapshot of the shell for tests / `ashd status`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ShellStatus {
    pub version: String,
    pub theme: ThemeName,
    pub niri_connected: bool,
    /// Open modal panel id (`launcher`, `quick-settings`, …), if any.
    pub modal: Option<String>,
    pub surfaces: Vec<SurfaceStatus>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SurfaceStatus {
    /// `rail` | `bar` | `scratchpad` | `overlay-passive` | `overlay-modal`.
    pub kind: String,
    /// niri output name (`DP-1`) when known, else the GPUI display id.
    pub output: String,
    pub width: f32,
    pub height: f32,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn socket_path_resolution() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(resolve_socket_path(os("/x/y.sock"), os("/run/user/1000"), 1000), PathBuf::from("/x/y.sock"));
        assert_eq!(
            resolve_socket_path(None, os("/run/user/1000"), 1000),
            PathBuf::from("/run/user/1000/ashd/shell.sock")
        );
        // Empty variables count as unset.
        assert_eq!(
            resolve_socket_path(os(""), os("/run/user/1000"), 1000),
            PathBuf::from("/run/user/1000/ashd/shell.sock")
        );
        assert_eq!(resolve_socket_path(None, None, 1234), PathBuf::from("/tmp/ashd-1234/shell.sock"));
        assert_eq!(resolve_socket_path(None, os(""), 1234), PathBuf::from("/tmp/ashd-1234/shell.sock"));
    }

    #[test]
    fn wire_format_is_stable() {
        let cases: Vec<(Request, &str)> = vec![
            (Request::Ping, r#""ping""#),
            (Request::Osd(OsdRequest::Volume(VolumeChange::Step(5))), r#"{"osd":{"volume":{"step":5}}}"#),
            (Request::Osd(OsdRequest::MicMute), r#"{"osd":"mic-mute"}"#),
            (
                Request::Launcher { op: PanelOp::Toggle, mode: Some(LauncherMode::Calc) },
                r#"{"launcher":{"op":"toggle","mode":"calc"}}"#,
            ),
            (Request::QuickSettings { op: PanelOp::Open }, r#"{"quick-settings":{"op":"open"}}"#),
            (Request::ThemeSwitcher { op: PanelOp::Toggle }, r#"{"theme-switcher":{"op":"toggle"}}"#),
            (Request::Theme(ThemeRequest::Set(ThemeName::Moss)), r#"{"theme":{"set":"moss"}}"#),
            (Request::Scratch(ScratchOp::Toggle), r#"{"scratch":"toggle"}"#),
            (Request::Canvas(CanvasOp::ZoomIn), r#"{"canvas":"zoom-in"}"#),
            (
                Request::Screenshot(ScreenshotRequest {
                    mode: Some(CaptureMode::Region),
                    record: false,
                    op: PanelOp::Toggle,
                }),
                r#"{"screenshot":{"mode":"region","record":false,"op":"toggle"}}"#,
            ),
            (
                Request::Screenshot(ScreenshotRequest { mode: None, record: true, op: PanelOp::Open }),
                r#"{"screenshot":{"record":true,"op":"open"}}"#,
            ),
            (Request::Record(RecordOp::Stop), r#"{"record":"stop"}"#),
            (Request::Record(RecordOp::Toggle), r#"{"record":"toggle"}"#),
        ];
        for (req, json) in cases {
            assert_eq!(serde_json::to_string(&req).unwrap(), json);
            assert_eq!(serde_json::from_str::<Request>(json).unwrap(), req);
        }
        // `op` defaults to toggle.
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"launcher":{}}"#).unwrap(),
            Request::Launcher { op: PanelOp::Toggle, mode: None }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"screenshot":{}}"#).unwrap(),
            Request::Screenshot(ScreenshotRequest::default())
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"screenshot":{"mode":"output"}}"#).unwrap(),
            Request::Screenshot(ScreenshotRequest { mode: Some(CaptureMode::Output), ..Default::default() })
        );
        assert_eq!(serde_json::to_string(&Response::Ok).unwrap(), r#""ok""#);
        assert_eq!(serde_json::to_string(&Response::error("x")).unwrap(), r#"{"error":"x"}"#);
    }

    #[test]
    fn error_helpers() {
        assert!(IpcError::NotRunning(PathBuf::from("/x")).is_not_running());
        assert!(!IpcError::Closed.is_not_running());
        let timeout = IpcError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "t"));
        assert!(timeout.is_timeout());
        assert!(!IpcError::Closed.is_timeout());
        assert!(matches!(Response::error("boom").into_result(), Err(IpcError::Remote(m)) if m == "boom"));
        assert_eq!(Response::Ok.into_result().unwrap(), Response::Ok);
    }
}
