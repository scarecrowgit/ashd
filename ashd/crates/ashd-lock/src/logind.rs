//! logind over the system bus (zbus 5, on its own thread): `CanSuspend` for the footer,
//! `Session.SetLockedHint`, and the confirmed power buttons.
//!
//! In the nested test session (`ASHD_NESTED=1`) every *write* is a dry run that is only
//! logged: the system bus is the real machine's, and a test must never reboot it or flip
//! the real session's LockedHint.

use std::sync::mpsc;

use ashd_lockui::PowerAction;
use zbus::Connection;

const DEST: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const SESSION: &str = "org.freedesktop.login1.Session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SetLockedHint(bool),
    Power(PowerAction),
}

/// Handle to the logind thread; commands are fire-and-forget.
pub struct Logind {
    tx: mpsc::Sender<Command>,
}

impl Logind {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Let queued commands (LockedHint(false)) run before the process exits; at most `timeout`.
    pub fn finish(self, thread: std::thread::JoinHandle<()>, timeout: std::time::Duration) {
        drop(self.tx);
        let deadline = std::time::Instant::now() + timeout;
        while !thread.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Start the thread. `on_can_suspend` is called once with logind's answer.
pub fn start(dry_run: bool, on_can_suspend: impl FnOnce(bool) + Send + 'static) -> (Logind, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<Command>();
    let handle = std::thread::Builder::new()
        .name("ashd-lock-logind".into())
        .spawn(move || smol::block_on(run(rx, dry_run, on_can_suspend)))
        .expect("spawn logind thread");
    (Logind { tx }, handle)
}

async fn run(rx: mpsc::Receiver<Command>, dry_run: bool, on_can_suspend: impl FnOnce(bool)) {
    let conn = match Connection::system().await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("logind: no system bus ({e}); power buttons and LockedHint disabled");
            None
        }
    };
    let can_suspend = match &conn {
        Some(c) => can(c, "CanSuspend").await,
        None => false,
    };
    on_can_suspend(can_suspend);
    let session = match &conn {
        Some(c) => session_path(c).await,
        None => None,
    };
    // Commands arrive from the UI thread; blocking here is fine (dedicated thread, zbus
    // drives the connection on its own executor thread).
    for cmd in rx {
        if dry_run {
            tracing::info!("logind (dry run, ASHD_NESTED=1): would {cmd:?}");
            continue;
        }
        let Some(c) = &conn else { continue };
        let r = match cmd {
            Command::SetLockedHint(v) => match &session {
                Some(path) => c.call_method(Some(DEST), path.as_str(), Some(SESSION), "SetLockedHint", &(v,)).await.map(drop),
                None => Ok(()),
            },
            Command::Power(a) => {
                let method = match a {
                    PowerAction::Suspend => "Suspend",
                    PowerAction::Reboot => "Reboot",
                    PowerAction::PowerOff => "PowerOff",
                };
                tracing::info!("logind: {method}");
                c.call_method(Some(DEST), MANAGER_PATH, Some(MANAGER), method, &(false,)).await.map(drop)
            }
        };
        if let Err(e) = r {
            tracing::warn!("logind {cmd:?} failed: {e}");
        }
    }
}

async fn can(c: &Connection, method: &str) -> bool {
    match c.call_method(Some(DEST), MANAGER_PATH, Some(MANAGER), method, &()).await {
        Ok(reply) => {
            let answer = reply.body().deserialize::<String>().unwrap_or_default();
            tracing::debug!("logind {method}: {answer}");
            answer == "yes" || answer == "challenge"
        }
        Err(e) => {
            tracing::debug!("logind {method}: {e}");
            false
        }
    }
}

/// Our session object: `XDG_SESSION_ID` if set, else the caller's session (`auto`).
async fn session_path(c: &Connection) -> Option<String> {
    if let Ok(id) = std::env::var("XDG_SESSION_ID")
        && !id.is_empty()
        && let Ok(reply) = c.call_method(Some(DEST), MANAGER_PATH, Some(MANAGER), "GetSession", &(id.as_str(),)).await
        && let Ok(path) = reply.body().deserialize::<zbus::zvariant::OwnedObjectPath>()
    {
        return Some(path.to_string());
    }
    Some("/org/freedesktop/login1/session/auto".into())
}
