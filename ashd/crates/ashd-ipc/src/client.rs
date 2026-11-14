use std::{
    io::{self, BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{IpcError, MAX_LINE, Request, Response, socket_path, sock};

/// Blocking client (std only — usable from the CLI without any runtime).
///
/// Every operation is bounded by [`Client::TIMEOUT`] (configurable): connecting, writing
/// and waiting for the answer. After an I/O error or timeout the connection is dropped
/// (a late answer could otherwise be read as the answer to the *next* request) and the
/// next [`Client::request`] transparently reconnects.
pub struct Client {
    path: PathBuf,
    timeout: Duration,
    conn: Option<Conn>,
}

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    /// Default per-request timeout. The shell answers from its main thread; anything
    /// slower than this is a hung shell.
    pub const TIMEOUT: Duration = Duration::from_secs(5);

    /// Connect to [`socket_path`].
    pub fn connect() -> Result<Self, IpcError> {
        Self::connect_to(socket_path())
    }

    pub fn connect_to(path: impl AsRef<Path>) -> Result<Self, IpcError> {
        Self::connect_timeout(path, Self::TIMEOUT)
    }

    /// Like [`Client::connect_to`] with a custom connect/read/write timeout.
    pub fn connect_timeout(path: impl AsRef<Path>, timeout: Duration) -> Result<Self, IpcError> {
        let path = path.as_ref().to_path_buf();
        let conn = Conn::open(&path, timeout)?;
        Ok(Self { path, timeout, conn: Some(conn) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Change the timeout used from the next request on.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
        if let Some(conn) = &self.conn {
            let _ = conn.set_timeout(timeout);
        }
    }

    /// Send one request and wait for its response. [`Response::Error`] is returned as
    /// `Ok(Response::Error(..))`; use [`Response::into_result`] to turn it into an error.
    ///
    /// A timeout surfaces as `IpcError::Io` with `ErrorKind::TimedOut`
    /// (see [`IpcError::is_timeout`]).
    pub fn request(&mut self, req: &Request) -> Result<Response, IpcError> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        if self.conn.is_none() {
            self.conn = Some(Conn::open(&self.path, self.timeout)?);
        }
        let conn = self.conn.as_mut().expect("connected above");
        let result = conn.exchange(line.as_bytes());
        if matches!(result, Err(IpcError::Io(_) | IpcError::Closed | IpcError::Protocol(_))) {
            self.conn = None;
        }
        result
    }

    /// `Request::Ping` → `(version, protocol, pid)`.
    pub fn ping(&mut self) -> Result<(String, u32, u32), IpcError> {
        match self.request(&Request::Ping)?.into_result()? {
            Response::Pong { version, protocol, pid } => Ok((version, protocol, pid)),
            other => Err(IpcError::Remote(format!("unexpected answer to ping: {other:?}"))),
        }
    }
}

impl Conn {
    fn open(path: &Path, timeout: Duration) -> Result<Self, IpcError> {
        let stream = match sock::connect_timeout(path, timeout) {
            Ok(s) => s,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                return Err(IpcError::NotRunning(path.to_path_buf()));
            }
            Err(e) => return Err(e.into()),
        };
        let conn = Self { writer: stream.try_clone()?, reader: BufReader::new(stream) };
        conn.set_timeout(timeout)?;
        Ok(conn)
    }

    fn set_timeout(&self, timeout: Duration) -> io::Result<()> {
        let timeout = Some(timeout.max(Duration::from_millis(1)));
        self.writer.set_read_timeout(timeout)?;
        self.writer.set_write_timeout(timeout)
    }

    fn exchange(&mut self, line: &[u8]) -> Result<Response, IpcError> {
        self.writer.write_all(line).map_err(timeout_kind)?;
        self.writer.flush().map_err(timeout_kind)?;
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
            .map_err(timeout_kind)?;
        if n == 0 {
            return Err(IpcError::Closed);
        }
        if buf.last() != Some(&b'\n') && n > MAX_LINE {
            return Err(IpcError::Io(io::Error::new(
                ErrorKind::InvalidData,
                format!("response longer than {MAX_LINE} bytes"),
            )));
        }
        // An unterminated final line means the shell died mid-answer.
        if buf.last() != Some(&b'\n') {
            return Err(IpcError::Closed);
        }
        Ok(serde_json::from_slice(buf.trim_ascii_end())?)
    }
}

/// `WouldBlock` is how a read/write timeout shows up on a blocking socket; report it as
/// the `TimedOut` it is.
fn timeout_kind(e: io::Error) -> IpcError {
    if e.kind() == ErrorKind::WouldBlock {
        IpcError::Io(io::Error::new(ErrorKind::TimedOut, "timed out waiting for ashd-shell"))
    } else {
        IpcError::Io(e)
    }
}

/// One-shot: connect, send, read, disconnect.
pub fn request(req: &Request) -> Result<Response, IpcError> {
    Client::connect()?.request(req)
}

/// Is a ashd-shell answering on [`socket_path`]? (`ping` with a short timeout.)
pub fn is_running() -> bool {
    Client::connect_timeout(socket_path(), Duration::from_millis(750)).and_then(|mut c| c.ping()).is_ok()
}
