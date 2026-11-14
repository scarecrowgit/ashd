//! logind over the system bus (zbus 5, on its own thread): `CanSuspend` for the footer and
//! the confirmed power buttons. Same approach as `ashd-lock/src/logind.rs`.
//!
//! With `dry_run` (`ASHD_FAKE_POWER=1`, `ASHD_NESTED=1`, `--demo`) every power command is
//! only logged: a test must never reboot the machine it runs on.

use std::sync::mpsc;

use ashd_lockui::PowerAction;
use zbus::Connection;

const DEST: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// Handle to the logind thread; commands are fire-and-forget.
pub struct Logind {
    tx: mpsc::Sender<PowerAction>,
}

impl Logind {
    pub fn send(&self, action: PowerAction) {
        let _ = self.tx.send(action);
    }
}

/// `ASHD_FAKE_POWER=1` or `ASHD_NESTED=1` (the nested test session sets both).
pub fn dry_run_from_env() -> bool {
    ["ASHD_FAKE_POWER", "ASHD_NESTED"].iter().any(|k| std::env::var(k).as_deref() == Ok("1"))
}

/// Start the thread. `on_can_suspend` is called once with logind's answer.
pub fn start(dry_run: bool, on_can_suspend: impl FnOnce(bool) + Send + 'static) -> Logind {
    let (tx, rx) = mpsc::channel::<PowerAction>();
    std::thread::Builder::new()
        .name("ashd-greeter-logind".into())
        .spawn(move || smol::block_on(run(rx, dry_run, on_can_suspend)))
        .expect("spawn logind thread");
    Logind { tx }
}

async fn run(rx: mpsc::Receiver<PowerAction>, dry_run: bool, on_can_suspend: impl FnOnce(bool)) {
    let conn = match Connection::system().await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("logind: no system bus ({e}); power buttons disabled");
            None
        }
    };
    let can_suspend = match &conn {
        Some(c) => can(c, "CanSuspend").await,
        None => false,
    };
    on_can_suspend(can_suspend);
    for action in rx {
        if dry_run {
            tracing::info!("logind (dry run): would {action:?}");
            continue;
        }
        let Some(c) = &conn else { continue };
        let method = match action {
            PowerAction::Suspend => "Suspend",
            PowerAction::Reboot => "Reboot",
            PowerAction::PowerOff => "PowerOff",
        };
        tracing::info!("logind: {method}");
        if let Err(e) = c.call_method(Some(DEST), MANAGER_PATH, Some(MANAGER), method, &(false,)).await {
            tracing::warn!("logind {method} failed: {e}");
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
