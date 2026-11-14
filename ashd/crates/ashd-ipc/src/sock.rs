//! Socket plumbing shared by [`crate::Client`] and [`crate::Server`]: a connect that can
//! never hang, a liveness probe for socket files, and the 0700 socket directory.

use std::{
    fs::{self, File},
    io,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rustix::{
    fs::{FlockOperation, flock},
    io::Errno,
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with},
};

/// What is behind a socket path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    /// Something accepted (or would accept: full backlog) our connection.
    Live,
    /// A socket file nobody listens on (a crashed shell left it behind).
    Stale,
    /// Nothing at all.
    Missing,
}

/// Result of a single non-blocking connect attempt.
enum Attempt {
    Connected(OwnedFd),
    /// The listener's backlog is full: alive, but busy.
    Busy,
}

fn try_connect(path: &Path) -> io::Result<Attempt> {
    let fd = socket_with(AddressFamily::UNIX, SocketType::STREAM, SocketFlags::CLOEXEC | SocketFlags::NONBLOCK, None)
        .map_err(io::Error::from)?;
    let addr = SocketAddrUnix::new(path).map_err(|e| {
        if e == Errno::NAMETOOLONG {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("socket path {} is too long (unix sockets allow ~107 bytes)", path.display()),
            )
        } else {
            io::Error::from(e)
        }
    })?;
    match connect(&fd, &addr) {
        Ok(()) => Ok(Attempt::Connected(fd)),
        // For unix sockets EAGAIN means "backlog full" (the listener is busy).
        Err(Errno::AGAIN) => Ok(Attempt::Busy),
        Err(e) => Err(e.into()),
    }
}

/// Connect to `path` without ever blocking longer than `timeout` (a plain blocking
/// `connect(2)` on a unix socket whose backlog is full waits forever). The returned
/// stream is blocking.
pub(crate) fn connect_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_connect(path)? {
            Attempt::Connected(fd) => {
                let stream = UnixStream::from(fd);
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Attempt::Busy => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out connecting to {}", path.display()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Is anybody listening on `path`?
pub(crate) fn probe(path: &Path) -> io::Result<Probe> {
    match try_connect(path) {
        Ok(Attempt::Connected(_)) | Ok(Attempt::Busy) => Ok(Probe::Live),
        Err(e) => match e.kind() {
            io::ErrorKind::ConnectionRefused => Ok(Probe::Stale),
            io::ErrorKind::NotFound => Ok(Probe::Missing),
            _ => Err(e),
        },
    }
}

/// Is `path` a unix socket file (not following symlinks)?
pub(crate) fn is_socket(path: &Path) -> io::Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_type().is_socket())
}

/// `(dev, ino)` of the file at `path`, to recognise our own socket file later.
pub(crate) fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Create `dir` (and missing parents) with mode 0700. A directory we create, or the
/// default socket directory, is also verified to be ours and tightened to 0700; any
/// other pre-existing directory (`ASHD_SOCKET=/tmp/x.sock` → `/tmp`) is left alone.
pub(crate) fn prepare_dir(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    let existed = dir.is_dir();
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let is_default = crate::default_socket_dir().is_some_and(|d| d == dir);
    if existed && !is_default {
        return Ok(());
    }
    let meta = fs::metadata(dir)?;
    if meta.uid() != current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", dir.display()),
        ));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// An exclusive advisory lock on `<socket>.lock`, held while a server replaces a stale
/// socket file so two shells starting together cannot delete each other's fresh socket.
pub(crate) struct BindLock {
    _file: File,
}

impl BindLock {
    pub(crate) fn acquire(socket: &Path) -> io::Result<Self> {
        let mut name = socket.as_os_str().to_owned();
        name.push(".lock");
        let path = PathBuf::from(name);
        let file = File::options().create(true).truncate(false).write(true).open(path)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(Errno::WOULDBLOCK) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}
