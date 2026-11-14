//! The logind service against a mock `org.freedesktop.login1` served over a private
//! peer-to-peer connection (a socketpair) — no bus, no real logind. Every mutating path
//! (power actions, lock, locked hint) is checked here against the mock's call log.

use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_channel::Receiver;
use ashd_svc_logind::{Command, LogindState, start_on};
use zbus::{Connection, connection::Builder, interface, object_server::SignalEmitter, zvariant::OwnedObjectPath};

type Log = Arc<Mutex<Vec<String>>>;

const SESSION: &str = "/org/freedesktop/login1/session/_333";

struct Manager(Log);

#[interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    fn can_suspend(&self) -> String {
        "no".into()
    }
    fn can_hibernate(&self) -> String {
        "na".into()
    }
    fn can_reboot(&self) -> String {
        "yes".into()
    }
    fn can_power_off(&self) -> String {
        "challenge".into()
    }
    fn get_session(&self, id: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(id, "333");
        Ok(OwnedObjectPath::try_from(SESSION).unwrap())
    }
    #[zbus(name = "ListInhibitors")]
    fn list_inhibitors(&self) -> Vec<(String, String, String, String, u32, u32)> {
        vec![("shutdown".into(), "Octium".into(), "Saving browser state".into(), "block".into(), 1000, 42)]
    }
    fn suspend(&self, interactive: bool) {
        self.0.lock().unwrap().push(format!("Suspend({interactive})"));
    }
    fn hibernate(&self, interactive: bool) {
        self.0.lock().unwrap().push(format!("Hibernate({interactive})"));
    }
    fn reboot(&self, interactive: bool) {
        self.0.lock().unwrap().push(format!("Reboot({interactive})"));
    }
    fn power_off(&self, interactive: bool) {
        self.0.lock().unwrap().push(format!("PowerOff({interactive})"));
    }
    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}

struct Session {
    log: Log,
    locked: bool,
}

#[interface(name = "org.freedesktop.login1.Session")]
impl Session {
    #[zbus(name = "Lock")]
    fn lock_method(&self) {
        self.log.lock().unwrap().push("Lock".into());
    }
    fn terminate(&self) {
        self.log.lock().unwrap().push("Terminate".into());
    }
    async fn set_locked_hint(&mut self, hint: bool, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        self.log.lock().unwrap().push(format!("SetLockedHint({hint})"));
        self.locked = hint;
        self.locked_hint_changed(&emitter).await?;
        Ok(())
    }
    #[zbus(property)]
    fn locked_hint(&self) -> bool {
        self.locked
    }
    #[zbus(property)]
    fn idle_hint(&self) -> bool {
        false
    }
    #[zbus(signal, name = "Lock")]
    async fn lock_signal(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal, name = "Unlock")]
    async fn unlock_signal(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct Fixture {
    server: Connection,
    log: Log,
    rx: Receiver<LogindState>,
    handle: ashd_svc_logind::Handle,
}

fn fixture(fake: bool) -> Fixture {
    let log: Log = Log::default();
    let (a, b) = UnixStream::pair().unwrap();
    let (server, client) = smol::block_on(async {
        let server = Builder::async_io_unix_stream(a)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at("/org/freedesktop/login1", Manager(log.clone()))
            .unwrap()
            .serve_at(SESSION, Session { log: log.clone(), locked: false })
            .unwrap()
            .build();
        let client = Builder::async_io_unix_stream(b).p2p().build();
        let (s, c) = smol::future::zip(server, client).await;
        (s.unwrap(), c.unwrap())
    });
    let (handle, rx) = start_on(client, Some("333".into()), fake);
    Fixture { server, log, rx, handle }
}

fn wait_for(rx: &Receiver<LogindState>, what: &str, pred: impl Fn(&LogindState) -> bool) -> LogindState {
    smol::block_on(smol::future::or(
        async {
            loop {
                match rx.recv().await {
                    Ok(s) if pred(&s) => return s,
                    Ok(_) => {}
                    Err(_) => panic!("state channel closed waiting for {what}"),
                }
            }
        },
        async {
            smol::Timer::after(Duration::from_secs(10)).await;
            panic!("timed out waiting for: {what}")
        },
    ))
}

fn wait_log(log: &Log, want: &str) {
    for _ in 0..200 {
        if log.lock().unwrap().iter().any(|l| l == want) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("mock never saw `{want}`; saw {:?}", log.lock().unwrap());
}

#[test]
fn state_and_signals() {
    let f = fixture(false);
    let s = wait_for(&f.rx, "available", |s| s.available);
    assert_eq!(s.session_id.as_deref(), Some("333"));
    // this "host" has sleep disabled
    assert!(!s.can_suspend && !s.can_hibernate);
    assert!(s.can_reboot && s.can_power_off, "challenge counts as allowed");
    assert!(!s.locked && !s.idle_hint);

    smol::block_on(async {
        let em = SignalEmitter::new(&f.server, SESSION).unwrap();
        Session::lock_signal(&em).await.unwrap();
        Session::lock_signal(&em).await.unwrap();
        Session::unlock_signal(&em).await.unwrap();
        let em = SignalEmitter::new(&f.server, "/org/freedesktop/login1").unwrap();
        Manager::prepare_for_sleep(&em, true).await.unwrap();
    });
    let s = wait_for(&f.rx, "signals", |s| s.lock_seq == 2 && s.unlock_seq == 1 && s.preparing_for_sleep);
    assert!(s.available);
    smol::block_on(async {
        let em = SignalEmitter::new(&f.server, "/org/freedesktop/login1").unwrap();
        Manager::prepare_for_sleep(&em, false).await.unwrap();
    });
    wait_for(&f.rx, "woke up", |s| !s.preparing_for_sleep);
}

#[test]
fn commands_reach_login1_and_locked_hint_comes_back() {
    let f = fixture(false);
    wait_for(&f.rx, "available", |s| s.available);

    f.handle.suspend();
    wait_log(&f.log, "Suspend(true)");
    f.handle.hibernate();
    wait_log(&f.log, "Hibernate(true)");
    f.handle.reboot();
    wait_log(&f.log, "Reboot(true)");
    f.handle.power_off();
    wait_log(&f.log, "PowerOff(true)");
    f.handle.logout();
    wait_log(&f.log, "Terminate");
    f.handle.lock_session();
    wait_log(&f.log, "Lock");

    f.handle.send(Command::SetLockedHint(true));
    wait_log(&f.log, "SetLockedHint(true)");
    wait_for(&f.rx, "locked", |s| s.locked);
    f.handle.send(Command::SetLockedHint(false));
    wait_for(&f.rx, "unlocked", |s| !s.locked);
}

#[test]
fn inhibitors_are_listed_on_request() {
    let f = fixture(false);
    let s = wait_for(&f.rx, "available", |s| s.available);
    assert!(s.inhibitors.is_empty());
    f.handle.refresh_inhibitors();
    let s = wait_for(&f.rx, "inhibitors", |s| !s.inhibitors.is_empty());
    let i = &s.inhibitors[0];
    assert_eq!((i.who.as_str(), i.mode.as_str(), i.pid), ("Octium", "block", 42));
    assert!(i.affects("shutdown") && !i.affects("sleep") && i.blocks());
}

#[test]
fn fake_power_logs_instead_of_acting() {
    let f = fixture(true);
    wait_for(&f.rx, "available", |s| s.available);
    f.handle.reboot();
    f.handle.power_off();
    f.handle.suspend();
    f.handle.logout();
    f.handle.lock_session();
    // a harmless call afterwards proves the queue was processed in order
    f.handle.send(Command::SetLockedHint(true));
    wait_log(&f.log, "SetLockedHint(true)");
    let log = f.log.lock().unwrap().clone();
    assert_eq!(log, ["SetLockedHint(true)"], "no power action may reach login1 in fake mode");
}
