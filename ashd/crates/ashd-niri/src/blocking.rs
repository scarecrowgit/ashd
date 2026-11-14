//! Blocking (std-only) niri helpers for the CLI. Every call is bounded by
//! [`TIMEOUT`]: a hung compositor must not hang a keybind.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use niri_ipc::{Action, Reply, Request, Response};

use crate::NiriError;

/// Connect / write / read timeout of the blocking helpers.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// Longest reply line we read (the full window list of a busy session is a few hundred KiB).
const MAX_REPLY: u64 = 16 * 1024 * 1024;

/// A blocking connection that can carry several requests (not `Request::EventStream`).
pub struct Connection {
    reader: BufReader<UnixStream>,
}

impl Connection {
    /// Connect to `$NIRI_SOCKET`.
    pub fn connect() -> Result<Self, NiriError> {
        match std::env::var_os(crate::SOCKET_ENV).filter(|s| !s.is_empty()) {
            Some(path) => Self::connect_to(path),
            None => Err(NiriError::NoSocket),
        }
    }

    pub fn connect_to(path: impl AsRef<Path>) -> Result<Self, NiriError> {
        let stream = UnixStream::connect(path.as_ref())?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        Ok(Self { reader: BufReader::new(stream) })
    }

    pub fn request(&mut self, req: &Request) -> Result<Response, NiriError> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        let stream = self.reader.get_mut();
        stream.write_all(line.as_bytes()).map_err(timeout_kind)?;
        stream.flush().map_err(timeout_kind)?;
        let mut buf = Vec::new();
        let n = (&mut self.reader).take(MAX_REPLY).read_until(b'\n', &mut buf).map_err(timeout_kind)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            return Err(NiriError::Gone);
        }
        let reply: Reply = serde_json::from_slice(&buf)?;
        reply.map_err(NiriError::Niri)
    }

    pub fn action(&mut self, action: Action) -> Result<(), NiriError> {
        match self.request(&Request::Action(action))? {
            Response::Handled => Ok(()),
            other => Err(NiriError::Unexpected(Box::new(other))),
        }
    }
}

fn timeout_kind(e: std::io::Error) -> NiriError {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        NiriError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "niri did not answer in time"))
    } else {
        NiriError::Io(e)
    }
}

/// One request on a fresh connection to `$NIRI_SOCKET`.
pub fn request(req: Request) -> Result<Response, NiriError> {
    Connection::connect()?.request(&req)
}

pub fn action(action: Action) -> Result<(), NiriError> {
    Connection::connect()?.action(action)
}

/// `ashd app quit`: close every window whose app id equals the focused window's.
/// Returns the number of windows asked to close (0 if nothing is focused).
pub fn close_focused_app_windows() -> Result<usize, NiriError> {
    close_focused_app_windows_on(&mut Connection::connect()?)
}

/// [`close_focused_app_windows`] on an existing connection.
pub fn close_focused_app_windows_on(conn: &mut Connection) -> Result<usize, NiriError> {
    let Response::FocusedWindow(focused) = conn.request(&Request::FocusedWindow)? else {
        return Err(NiriError::Niri("unexpected reply to FocusedWindow".into()));
    };
    let Some(focused) = focused else { return Ok(0) };
    // A window without an app id only closes itself: "no app id" is not an app.
    let Some(app_id) = focused.app_id.clone().filter(|a| !a.is_empty()) else {
        conn.action(Action::CloseWindow { id: Some(focused.id) })?;
        return Ok(1);
    };
    let Response::Windows(windows) = conn.request(&Request::Windows)? else {
        return Err(NiriError::Niri("unexpected reply to Windows".into()));
    };
    let mut ids: Vec<u64> =
        windows.iter().filter(|w| w.app_id.as_deref() == Some(app_id.as_str())).map(|w| w.id).collect();
    if !ids.contains(&focused.id) {
        ids.push(focused.id);
    }
    // Close the focused one last so focus does not jump around mid-way.
    ids.sort_by_key(|id| (*id == focused.id, *id));
    let mut closed = 0;
    for id in ids {
        match conn.action(Action::CloseWindow { id: Some(id) }) {
            Ok(()) => closed += 1,
            // The window vanished meanwhile: nothing left to close.
            Err(NiriError::Niri(e)) => tracing::debug!("niri: close window {id}: {e}"),
            Err(e) => return Err(e),
        }
    }
    Ok(closed)
}
