//! The login1 session: proxies, event merging, command execution.

use std::future::Future;

use async_channel::{Receiver, Sender};
use futures_lite::StreamExt as _;
use smol::{LocalExecutor, future::or};
use zbus::{Connection, zvariant::OwnedObjectPath};

use crate::{Command, Inhibitor, LogindState, RETRY_MAX, RETRY_MIN};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn can_suspend(&self) -> zbus::Result<String>;
    fn can_hibernate(&self) -> zbus::Result<String>;
    fn can_reboot(&self) -> zbus::Result<String>;
    fn can_power_off(&self) -> zbus::Result<String>;
    fn get_session(&self, id: &str) -> zbus::Result<OwnedObjectPath>;
    #[zbus(name = "GetSessionByPID")]
    fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
    fn list_inhibitors(&self) -> zbus::Result<Vec<(String, String, String, String, u32, u32)>>;
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn hibernate(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1")]
trait Session {
    fn lock(&self) -> zbus::Result<()>;
    fn terminate(&self) -> zbus::Result<()>;
    fn set_locked_hint(&self, hint: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn idle_hint(&self) -> zbus::Result<bool>;
    #[zbus(signal, name = "Lock")]
    fn lock_signal(&self) -> zbus::Result<()>;
    #[zbus(signal, name = "Unlock")]
    fn unlock_signal(&self) -> zbus::Result<()>;
}

pub(crate) struct Config {
    pub session_id: Option<String>,
    pub fake_power: bool,
}

enum End {
    Closed,
    Lost,
}

pub(crate) async fn run<F, Fut>(commands: Receiver<Command>, state: Sender<LogindState>, cfg: Config, mut connect: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = zbus::Result<Connection>>,
{
    let mut backoff = RETRY_MIN;
    let _ = state.force_send(LogindState::default());
    loop {
        while commands.try_recv().is_ok() {}
        let end = match connect().await {
            Ok(conn) => match session(&conn, &commands, &state, &cfg).await {
                Ok(end) => {
                    backoff = RETRY_MIN;
                    end
                }
                Err(e) => {
                    tracing::debug!("logind: {e}");
                    End::Lost
                }
            },
            Err(e) => {
                tracing::debug!("logind: cannot connect: {e}");
                End::Lost
            }
        };
        let _ = state.force_send(LogindState::default());
        if matches!(end, End::Closed) {
            return;
        }
        let closed = or(
            async {
                smol::Timer::after(backoff).await;
                false
            },
            async {
                while commands.recv().await.is_ok() {}
                true
            },
        )
        .await;
        if closed {
            return;
        }
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

enum Ev {
    Cmd(Command),
    CmdsClosed,
    Lock,
    Unlock,
    Sleep(bool),
    Locked(bool),
    Idle(bool),
    /// A stream ended: the bus connection is gone.
    Gone,
}

fn can(answer: &str) -> bool {
    matches!(answer, "yes" | "challenge")
}

async fn read_caps(mgr: &ManagerProxy<'_>, st: &mut LogindState) {
    st.can_suspend = mgr.can_suspend().await.is_ok_and(|a| can(&a));
    st.can_hibernate = mgr.can_hibernate().await.is_ok_and(|a| can(&a));
    st.can_reboot = mgr.can_reboot().await.is_ok_and(|a| can(&a));
    st.can_power_off = mgr.can_power_off().await.is_ok_and(|a| can(&a));
}

async fn read_inhibitors(mgr: &ManagerProxy<'_>) -> Vec<Inhibitor> {
    match mgr.list_inhibitors().await {
        Ok(v) => v
            .into_iter()
            .map(|(what, who, why, mode, uid, pid)| Inhibitor { what, who, why, mode, uid, pid })
            .collect(),
        Err(e) => {
            tracing::debug!("logind: ListInhibitors: {e}");
            Vec::new()
        }
    }
}

async fn session(conn: &Connection, commands: &Receiver<Command>, state: &Sender<LogindState>, cfg: &Config) -> zbus::Result<End> {
    let mgr = ManagerProxy::new(conn).await?;
    let path = match &cfg.session_id {
        Some(id) => mgr.get_session(id).await?,
        None => mgr.get_session_by_pid(std::process::id()).await?,
    };
    let sess = SessionProxy::builder(conn).path(path)?.build().await?;

    let mut st = LogindState {
        available: true,
        session_id: cfg.session_id.clone(),
        locked: sess.locked_hint().await.unwrap_or(false),
        idle_hint: sess.idle_hint().await.unwrap_or(false),
        ..Default::default()
    };
    read_caps(&mgr, &mut st).await;

    // Merge every source into one channel; forwarders run on a local executor so no
    // extra threads are spawned.
    let (tx, rx) = async_channel::unbounded::<Ev>();
    let ex = LocalExecutor::new();
    let mut lock_s = sess.receive_lock_signal().await?;
    let mut unlock_s = sess.receive_unlock_signal().await?;
    let mut sleep_s = mgr.receive_prepare_for_sleep().await?;
    let mut locked_s = sess.receive_locked_hint_changed().await;
    let mut idle_s = sess.receive_idle_hint_changed().await;
    ex.spawn({
        let tx = tx.clone();
        async move {
            while lock_s.next().await.is_some() {
                let _ = tx.send(Ev::Lock).await;
            }
            let _ = tx.send(Ev::Gone).await;
        }
    })
    .detach();
    ex.spawn({
        let tx = tx.clone();
        async move {
            while unlock_s.next().await.is_some() {
                let _ = tx.send(Ev::Unlock).await;
            }
            let _ = tx.send(Ev::Gone).await;
        }
    })
    .detach();
    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Some(sig) = sleep_s.next().await {
                if let Ok(args) = sig.args() {
                    let _ = tx.send(Ev::Sleep(args.start)).await;
                }
            }
            let _ = tx.send(Ev::Gone).await;
        }
    })
    .detach();
    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Some(c) = locked_s.next().await {
                if let Ok(v) = c.get().await {
                    let _ = tx.send(Ev::Locked(v)).await;
                }
            }
        }
    })
    .detach();
    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Some(c) = idle_s.next().await {
                if let Ok(v) = c.get().await {
                    let _ = tx.send(Ev::Idle(v)).await;
                }
            }
        }
    })
    .detach();
    let commands = commands.clone();
    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Ok(c) = commands.recv().await {
                let _ = tx.send(Ev::Cmd(c)).await;
            }
            let _ = tx.send(Ev::CmdsClosed).await;
        }
    })
    .detach();
    drop(tx);

    let _ = state.force_send(st.clone());
    let main = async {
        loop {
            let Ok(ev) = rx.recv().await else { return End::Lost };
            match ev {
                Ev::CmdsClosed => return End::Closed,
                Ev::Gone => return End::Lost,
                Ev::Lock => st.lock_seq += 1,
                Ev::Unlock => st.unlock_seq += 1,
                Ev::Sleep(start) => {
                    st.preparing_for_sleep = start;
                    if !start {
                        read_caps(&mgr, &mut st).await;
                    }
                }
                Ev::Locked(v) => st.locked = v,
                Ev::Idle(v) => st.idle_hint = v,
                Ev::Cmd(Command::RefreshInhibitors) => st.inhibitors = read_inhibitors(&mgr).await,
                Ev::Cmd(cmd) => {
                    execute(cmd, &mgr, &sess, cfg.fake_power).await;
                    continue;
                }
            }
            let _ = state.force_send(st.clone());
        }
    };
    Ok(ex.run(main).await)
}

async fn execute(cmd: Command, mgr: &ManagerProxy<'_>, sess: &SessionProxy<'_>, fake: bool) {
    // `SetLockedHint` is state bookkeeping for ashd-lock, not a power action.
    if fake && !matches!(cmd, Command::SetLockedHint(_)) {
        tracing::warn!(target: "ashd_svc_logind::fake", "ASHD_FAKE_POWER: would run {cmd:?}");
        return;
    }
    let result = match cmd {
        Command::LockSession => sess.lock().await,
        Command::SetLockedHint(v) => sess.set_locked_hint(v).await,
        Command::Suspend => mgr.suspend(true).await,
        Command::Hibernate => mgr.hibernate(true).await,
        Command::Reboot => mgr.reboot(true).await,
        Command::PowerOff => mgr.power_off(true).await,
        Command::Logout => sess.terminate().await,
        Command::RefreshInhibitors => Ok(()),
    };
    if let Err(e) = result {
        tracing::warn!("logind: {cmd:?} failed: {e}");
    }
}
