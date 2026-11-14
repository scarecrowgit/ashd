use std::{
    fs, io,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_channel::Sender;
use futures_lite::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, future, io::BufReader};
use smol::{Async, Timer};

use crate::{MAX_LINE, Request, Response, socket_path, sock};

/// One request from a client, waiting for its answer.
///
/// Dropping it without calling [`Incoming::respond`] answers the client with
/// `Response::Error("request dropped by the shell")`, so a panel that forgets to answer
/// never hangs the CLI.
#[derive(Debug)]
pub struct Incoming {
    pub request: Request,
    reply: Sender<Response>,
}

impl Incoming {
    /// Answer the client. A client that has disconnected meanwhile is ignored.
    pub fn respond(self, response: Response) {
        let _ = self.reply.try_send(response);
    }
}

/// The listening socket. Bind it early (fail fast if another shell runs), then drive
/// [`Server::run`] on any executor: `cx.background_spawn(server.run(tx))` in the shell.
///
/// Robustness guarantees (all covered by tests):
/// - a socket file left by a crashed shell is replaced, a live one is `AddrInUse`;
///   a non-socket file at the path is never touched;
/// - the socket directory is created `0700` (the socket itself `0600`);
/// - lines are limited to [`MAX_LINE`] bytes, partial lines are buffered, invalid UTF-8 /
///   JSON gets a `bad request` error answer (the connection stays usable), an unterminated
///   last line at EOF is still served;
/// - idle connections are dropped after [`Server::IDLE_TIMEOUT`], at most
///   [`Server::MAX_CONNECTIONS`] are served at once, and a request nobody answers within
///   [`Server::REPLY_TIMEOUT`] gets an error answer;
/// - dropping the `Server` removes the socket file (only if it is still ours).
pub struct Server {
    listener: Async<UnixListener>,
    path: PathBuf,
    identity: Option<(u64, u64)>,
}

impl Server {
    /// A connection that sends nothing for this long is closed.
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
    /// How long a request may wait for the shell's answer.
    pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
    /// Concurrent connections served; more get a `too many connections` error and are closed.
    pub const MAX_CONNECTIONS: usize = 64;

    /// Bind [`socket_path`].
    pub fn bind_default() -> io::Result<Self> {
        Self::bind(socket_path())
    }

    /// Bind `path`, creating its parent directory (mode 0700). A stale socket file (no
    /// listener) is replaced; a live one is `AddrInUse` — another shell is running.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            sock::prepare_dir(parent)?;
        }
        // Serialise concurrent binders so the stale-socket cleanup below cannot remove a
        // socket another shell has just created.
        let _lock = sock::BindLock::acquire(&path)?;
        let mut last_err = None;
        for _ in 0..3 {
            match UnixListener::bind(&path) {
                Ok(listener) => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
                    let identity = sock::identity(&path);
                    return Ok(Self { listener: Async::new(listener)?, path, identity });
                }
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                    last_err = Some(e);
                    if !sock::is_socket(&path)? {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("{} exists and is not a socket; refusing to replace it", path.display()),
                        ));
                    }
                    match sock::probe(&path)? {
                        sock::Probe::Live => {
                            return Err(io::Error::new(
                                io::ErrorKind::AddrInUse,
                                format!("{} is served by another ashd-shell", path.display()),
                            ));
                        }
                        sock::Probe::Stale => {
                            tracing::info!("removing stale ashd-shell socket {}", path.display());
                            match fs::remove_file(&path) {
                                Ok(()) => {}
                                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                                Err(e) => return Err(e),
                            }
                        }
                        sock::Probe::Missing => {}
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrInUse, "could not bind the socket")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections forever, forwarding each request to `tx`. Connections are
    /// handled concurrently on smol's global executor. Returns when `tx` is closed (checked
    /// whenever a connection is accepted or fails to be).
    pub async fn run(self, tx: Sender<Incoming>) {
        let active = Arc::new(AtomicUsize::new(0));
        loop {
            if tx.is_closed() {
                return;
            }
            match self.listener.accept().await {
                Ok((stream, _)) => {
                    let tx = tx.clone();
                    let guard = ConnGuard::new(&active);
                    smol::spawn(async move {
                        let over_limit = guard.count > Self::MAX_CONNECTIONS;
                        if let Err(e) = handle(stream, tx, over_limit).await {
                            tracing::debug!("ipc connection: {e}");
                        }
                        drop(guard);
                    })
                    .detach();
                }
                Err(e) => {
                    tracing::warn!("ipc accept: {e}");
                    Timer::after(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Only remove the socket file if it is still the one we created.
        if self.identity.is_some() && sock::identity(&self.path) == self.identity {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct ConnGuard {
    active: Arc<AtomicUsize>,
    /// Connections open including this one.
    count: usize,
}

impl ConnGuard {
    fn new(active: &Arc<AtomicUsize>) -> Self {
        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
        Self { active: active.clone(), count }
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A complete line (without the newline) is in the buffer.
    Complete,
    /// EOF after some bytes without a newline: the buffer holds the unterminated line.
    Unterminated,
    Eof,
    TooLong,
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (without the newline).
async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>, max: usize) -> io::Result<Line> {
    buf.clear();
    let n = (&mut *reader).take(max as u64 + 1).read_until(b'\n', buf).await?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(Line::Complete);
    }
    if n > max { Ok(Line::TooLong) } else { Ok(Line::Unterminated) }
}

async fn write_response<W: futures_lite::AsyncWrite + Unpin>(writer: &mut W, response: &Response) -> io::Result<()> {
    let mut out = serde_json::to_vec(response).map_err(io::Error::other)?;
    out.push(b'\n');
    writer.write_all(&out).await?;
    writer.flush().await
}

async fn handle(stream: Async<std::os::unix::net::UnixStream>, tx: Sender<Incoming>, over_limit: bool) -> io::Result<()> {
    let mut writer = Async::new(stream.get_ref().try_clone()?)?;
    if over_limit {
        write_response(&mut writer, &Response::error("too many connections")).await?;
        return Ok(());
    }
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        let idle = async {
            Timer::after(Server::IDLE_TIMEOUT).await;
            Err(io::Error::new(io::ErrorKind::TimedOut, "idle connection"))
        };
        let line = future::or(read_line(&mut reader, &mut buf, MAX_LINE), idle).await?;
        match line {
            Line::Eof => return Ok(()),
            Line::TooLong => {
                let msg = format!("request line longer than {MAX_LINE} bytes");
                write_response(&mut writer, &Response::error(msg)).await?;
                return Ok(());
            }
            Line::Complete | Line::Unterminated => {}
        }
        if buf.trim_ascii().is_empty() {
            if line == Line::Unterminated {
                return Ok(());
            }
            continue;
        }
        let response = match serde_json::from_slice::<Request>(&buf) {
            Err(e) => Response::error(format!("bad request: {e}")),
            Ok(request) => dispatch(&tx, request).await,
        };
        write_response(&mut writer, &response).await?;
        if line == Line::Unterminated {
            return Ok(());
        }
    }
}

async fn dispatch(tx: &Sender<Incoming>, request: Request) -> Response {
    let (reply, rx) = async_channel::bounded(1);
    if tx.send(Incoming { request, reply }).await.is_err() {
        return Response::error("ashd-shell is shutting down");
    }
    let timeout = async {
        Timer::after(Server::REPLY_TIMEOUT).await;
        Err(())
    };
    let answer = async { rx.recv().await.map_err(|_| ()) };
    match future::or(answer, timeout).await {
        Ok(response) => response,
        Err(()) if rx.is_closed() => Response::error("request dropped by the shell"),
        Err(()) => Response::error("ashd-shell did not answer in time"),
    }
}
