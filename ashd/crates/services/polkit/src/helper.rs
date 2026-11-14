//! Talking to polkit's authentication helper: transport detection and one PAM
//! conversation (see [`crate::protocol`] for the wire format).
//!
//! Transports, in polkit 127's own order (`polkit_agent_session_initiate`):
//! 1. the socket-activated helper, `/run/polkit/agent-helper.socket` (systemd
//!    `polkit-agent-helper.socket`, `Accept=yes`; the helper learns the caller's uid and
//!    pidfd from the socket) — the only one on NixOS with polkit ≥ 127;
//! 2. a setuid `polkit-agent-helper-1 <user>` (older polkit / other distributions; on
//!    older NixOS the wrapper `/run/wrappers/bin/polkit-agent-helper-1`).
//!
//! A conversation never hangs: connecting, the helper's first line and the answer to a
//! response all have deadlines ([`Timeouts`]); EOF or a crash ends it with
//! [`ConvEvent::Closed`]. Dropping the conversation task closes the socket / kills the
//! helper process.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use futures_lite::{AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, io::BufReader};
use zeroize::Zeroizing;

use crate::protocol::{HelperLine, parse_line};

/// The socket of polkit's socket-activated helper.
pub const SYSTEM_SOCKET: &str = "/run/polkit/agent-helper.socket";

/// Setuid helpers tried when the socket does not exist (first existing setuid file wins).
pub const SETUID_CANDIDATES: &[&str] = &[
    "/run/wrappers/bin/polkit-agent-helper-1",
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-agent-helper-1",
    "/usr/lib/policykit-1/polkit-agent-helper-1",
];

/// Longest helper line accepted (PAM messages are short; this only bounds memory).
const MAX_LINE: usize = 16 * 1024;

/// Which helper to use.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum HelperTarget {
    /// Detect: [`SYSTEM_SOCKET`] if it exists (falling back to a setuid helper if the
    /// connection fails, like libpolkit-agent), else the first setuid
    /// [`SETUID_CANDIDATES`] entry.
    #[default]
    Auto,
    /// A socket speaking the socket-activated protocol (user line, cookie line, …).
    Socket(PathBuf),
    /// An executable speaking the setuid protocol (`<exe> <user>`, cookie on stdin).
    Exec(PathBuf),
}

impl HelperTarget {
    /// `socket:/path`, `exec:/path`, or a bare path (a socket file → `Socket`, else `Exec`).
    pub fn parse(s: &str) -> Self {
        if let Some(p) = s.strip_prefix("socket:") {
            HelperTarget::Socket(p.into())
        } else if let Some(p) = s.strip_prefix("exec:") {
            HelperTarget::Exec(p.into())
        } else if is_socket(Path::new(s)) {
            HelperTarget::Socket(s.into())
        } else {
            HelperTarget::Exec(s.into())
        }
    }

    /// The transports to try, in order. Empty: no helper on this system.
    pub fn transports(&self) -> Vec<Transport> {
        match self {
            HelperTarget::Socket(p) => vec![Transport::Socket(p.clone())],
            HelperTarget::Exec(p) => vec![Transport::Exec(p.clone())],
            HelperTarget::Auto => {
                let mut v = Vec::new();
                if Path::new(SYSTEM_SOCKET).exists() {
                    v.push(Transport::Socket(SYSTEM_SOCKET.into()));
                }
                if let Some(p) = SETUID_CANDIDATES.iter().map(Path::new).find(|p| is_setuid(p)) {
                    v.push(Transport::Exec(p.into()));
                }
                v
            }
        }
    }
}

fn is_socket(p: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.file_type().is_socket())
}

fn is_setuid(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o4000 != 0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Socket(PathBuf),
    Exec(PathBuf),
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Socket(p) => write!(f, "socket {}", p.display()),
            Transport::Exec(p) => write!(f, "setuid helper {}", p.display()),
        }
    }
}

/// Deadlines of one conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Connecting to the socket / spawning the helper.
    pub connect: Duration,
    /// From the cookie until the helper's first line (its first prompt).
    pub first_line: Duration,
    /// From writing a response until the helper's next line (PAM's failure delay is ~2 s).
    pub reply: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { connect: Duration::from_secs(5), first_line: Duration::from_secs(15), reply: Duration::from_secs(30) }
    }
}

/// What a conversation reports to the agent core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConvEvent {
    Line(HelperLine),
    /// Ended without `SUCCESS`/`FAILURE`: unreachable helper, EOF, crash, timeout.
    Closed(String),
}

enum Io {
    Socket(smol::net::unix::UnixStream),
    Exec(smol::process::Child),
}

async fn with_deadline<T>(d: Duration, f: impl std::future::Future<Output = T>) -> Option<T> {
    smol::future::or(async { Some(f.await) }, async {
        smol::Timer::after(d).await;
        None
    })
    .await
}

async fn connect(t: &Transport, user: &str, timeout: Duration) -> Result<Io, String> {
    match t {
        Transport::Socket(p) => match with_deadline(timeout, smol::net::unix::UnixStream::connect(p)).await {
            Some(Ok(s)) => Ok(Io::Socket(s)),
            Some(Err(e)) => Err(format!("{t}: {e}")),
            None => Err(format!("{t}: connection timed out")),
        },
        Transport::Exec(p) => smol::process::Command::new(p)
            .arg(user)
            .stdin(smol::process::Stdio::piped())
            .stdout(smol::process::Stdio::piped())
            .stderr(smol::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map(Io::Exec)
            .map_err(|e| format!("{t}: {e}")),
    }
}

/// Run one conversation for `user` / `cookie`. Responses arrive on `responses` (closing it
/// — or dropping this future — ends the conversation). Every helper line and the end are
/// sent on `events`.
pub(crate) async fn converse(
    transports: Vec<Transport>,
    user: String,
    cookie: String,
    timeouts: Timeouts,
    events: Sender<ConvEvent>,
    responses: Receiver<Zeroizing<String>>,
) {
    let end = run(&transports, &user, &cookie, timeouts, &events, &responses).await;
    if let Err(reason) = end {
        let _ = events.send(ConvEvent::Closed(reason)).await;
    }
}

async fn run(
    transports: &[Transport],
    user: &str,
    cookie: &str,
    timeouts: Timeouts,
    events: &Sender<ConvEvent>,
    responses: &Receiver<Zeroizing<String>>,
) -> Result<(), String> {
    if transports.is_empty() {
        return Err("no polkit authentication helper on this system".into());
    }
    let mut errors = Vec::new();
    let mut io = None;
    for t in transports {
        match connect(t, user, timeouts.connect).await {
            Ok(c) => {
                io = Some(c);
                break;
            }
            Err(e) => {
                tracing::warn!("polkit: cannot reach the helper: {e}");
                errors.push(e);
            }
        }
    }
    let Some(io) = io else { return Err(format!("cannot reach the authentication helper ({})", errors.join("; "))) };
    match io {
        Io::Socket(stream) => {
            let mut w = stream.clone();
            let pre = format!("{user}\n{cookie}\n");
            write(&mut w, pre.as_bytes()).await?;
            lines(BufReader::new(stream), w, timeouts, events, responses).await
        }
        Io::Exec(mut child) => {
            let (Some(mut w), Some(r)) = (child.stdin.take(), child.stdout.take()) else {
                return Err("helper has no stdio".into());
            };
            let pre = format!("{cookie}\n");
            write(&mut w, pre.as_bytes()).await?;
            let r = lines(BufReader::new(r), w, timeouts, events, responses).await;
            drop(child); // kill_on_drop
            r
        }
    }
}

async fn write(w: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<(), String> {
    w.write_all(bytes).await.map_err(|e| format!("cannot write to the helper: {e}"))?;
    w.flush().await.map_err(|e| format!("cannot write to the helper: {e}"))
}

enum Step {
    Line(std::io::Result<usize>),
    Response(Option<Zeroizing<String>>),
    Timeout,
}

async fn lines(
    mut r: impl futures_lite::AsyncBufRead + Unpin,
    mut w: impl AsyncWrite + Unpin,
    timeouts: Timeouts,
    events: &Sender<ConvEvent>,
    responses: &Receiver<Zeroizing<String>>,
) -> Result<(), String> {
    let mut deadline = Some((Instant::now() + timeouts.first_line, timeouts.first_line));
    // Kept across iterations: `read_until` appends, so a read interrupted by a response
    // or a timer loses nothing.
    let mut buf = Vec::new();
    loop {
        let step = {
            let line = async { Step::Line(r.read_until(b'\n', &mut buf).await) };
            let resp = async { Step::Response(responses.recv().await.ok()) };
            let timer = async {
                match deadline {
                    Some((at, _)) => smol::Timer::at(at).await,
                    None => std::future::pending().await,
                };
                Step::Timeout
            };
            smol::future::or(line, smol::future::or(resp, timer)).await
        };
        match step {
            Step::Line(Ok(0)) => return Err("the authentication helper closed the connection".into()),
            Step::Line(Err(e)) => return Err(format!("cannot read from the authentication helper: {e}")),
            Step::Line(Ok(_)) if !buf.ends_with(b"\n") && buf.len() <= MAX_LINE => {
                // EOF in the middle of a line.
                return Err("the authentication helper closed the connection".into());
            }
            Step::Line(Ok(_)) => {
                if buf.len() > MAX_LINE {
                    return Err("the authentication helper sent an overlong line".into());
                }
                deadline = None;
                let line = parse_line(&buf);
                buf.clear();
                let last = matches!(line, HelperLine::Success | HelperLine::Failure | HelperLine::Unknown(_));
                if events.send(ConvEvent::Line(line)).await.is_err() || last {
                    return Ok(());
                }
            }
            Step::Response(Some(resp)) => {
                let mut bytes = Zeroizing::new(Vec::with_capacity(resp.len() + 1));
                bytes.extend_from_slice(resp.as_bytes());
                bytes.push(b'\n');
                drop(resp);
                write(&mut w, &bytes).await?;
                deadline = Some((Instant::now() + timeouts.reply, timeouts.reply));
            }
            // The core dropped the conversation (cancel, identity switch, request done).
            Step::Response(None) => return Ok(()),
            Step::Timeout => {
                let secs = deadline.map(|(_, d)| d.as_secs_f32()).unwrap_or_default();
                return Err(format!("the authentication helper did not answer within {secs:.0} s"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_targets() {
        assert_eq!(HelperTarget::parse("socket:/x/s"), HelperTarget::Socket("/x/s".into()));
        assert_eq!(HelperTarget::parse("exec:/x/h"), HelperTarget::Exec("/x/h".into()));
        assert_eq!(HelperTarget::parse("/nonexistent/h"), HelperTarget::Exec("/nonexistent/h".into()));
        assert_eq!(HelperTarget::Socket("/s".into()).transports(), vec![Transport::Socket("/s".into())]);
    }

    #[test]
    fn a_bare_socket_path_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.sock");
        let _l = std::os::unix::net::UnixListener::bind(&p).unwrap();
        assert_eq!(HelperTarget::parse(p.to_str().unwrap()), HelperTarget::Socket(p.clone()));
    }

    #[test]
    fn non_setuid_files_are_not_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("polkit-agent-helper-1");
        std::fs::write(&p, b"").unwrap();
        assert!(!is_setuid(&p));
    }

    #[test]
    fn unreachable_helper_reports_instead_of_hanging() {
        let (tx, rx) = async_channel::unbounded();
        let (_rtx, rrx) = async_channel::unbounded();
        smol::block_on(converse(
            vec![Transport::Socket("/nonexistent/ashd-polkit.sock".into())],
            "u".into(),
            "c".into(),
            Timeouts::default(),
            tx,
            rrx,
        ));
        match rx.try_recv().unwrap() {
            ConvEvent::Closed(r) => assert!(r.contains("cannot reach"), "{r}"),
            other => panic!("{other:?}"),
        }
        let (tx, rx) = async_channel::unbounded();
        let (_rtx, rrx) = async_channel::unbounded();
        smol::block_on(converse(vec![], "u".into(), "c".into(), Timeouts::default(), tx, rrx));
        assert!(matches!(rx.try_recv().unwrap(), ConvEvent::Closed(_)));
    }
}
