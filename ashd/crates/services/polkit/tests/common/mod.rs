//! Shared test fixtures: a mock `org.freedesktop.PolicyKit1.Authority` (and a minimal
//! login1) served over peer-to-peer connections, and a fake polkit helper on a Unix
//! socket. Nothing here touches a real bus, the real authority or the real helper.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_channel::{Receiver, Sender};
use ashd_svc_polkit::{AGENT_PATH, AUTHORITY_PATH, Handle, HelperTarget, Options, PolkitState, Timeouts};
use zbus::{
    Connection,
    connection::Builder,
    interface,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

pub type Log = Arc<Mutex<Vec<String>>>;

pub struct Authority {
    pub log: Log,
}

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Authority {
    fn register_authentication_agent(&self, subject: (String, HashMap<String, OwnedValue>), locale: String, object_path: String) {
        let sid = subject.1.get("session-id").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
        self.log.lock().unwrap().push(format!("Register({},{sid},{locale},{object_path})", subject.0));
    }
    fn unregister_authentication_agent(&self, subject: (String, HashMap<String, OwnedValue>), object_path: String) {
        let sid = subject.1.get("session-id").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
        self.log.lock().unwrap().push(format!("Unregister({sid},{object_path})"));
    }
}

pub struct LoginManager;

#[interface(name = "org.freedesktop.login1.Manager")]
impl LoginManager {
    fn get_session(&self, id: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        assert_eq!(id, "auto");
        Ok(OwnedObjectPath::try_from("/org/freedesktop/login1/session/c9").unwrap())
    }
}

pub struct LoginSession;

#[interface(name = "org.freedesktop.login1.Session")]
impl LoginSession {
    #[zbus(property)]
    fn id(&self) -> String {
        "c9".into()
    }
}

/// What the fake helper does.
#[derive(Default)]
pub struct Script {
    /// user → the password that succeeds.
    pub passwords: HashMap<String, String>,
    /// Sent before the prompt.
    pub info: Option<String>,
    /// Sent before `FAILURE`.
    pub error: Option<String>,
    /// Crash (close without a verdict) after reading the next N responses.
    pub crash_next: AtomicUsize,
    /// Never say anything (a hung helper).
    pub mute: bool,
    /// (user, cookie) of successful authentications — what the real helper reports to
    /// the authority with `AuthenticationAgentResponse2`.
    pub authenticated: Mutex<Vec<(String, String)>>,
    /// Users each connection asked for, in order.
    pub sessions: Mutex<Vec<String>>,
    pub connections: AtomicUsize,
}

pub struct FakeHelper {
    pub path: PathBuf,
    pub script: Arc<Script>,
    _dir: tempfile::TempDir,
}

impl FakeHelper {
    pub fn start(script: Script) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-helper.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let script = Arc::new(script);
        let s = script.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let s = s.clone();
                std::thread::spawn(move || serve(conn, &s));
            }
        });
        Self { path, script, _dir: dir }
    }

    pub fn target(&self) -> HelperTarget {
        HelperTarget::Socket(self.path.clone())
    }

    pub fn authenticated(&self) -> Vec<(String, String)> {
        self.script.authenticated.lock().unwrap().clone()
    }
}

fn serve(conn: UnixStream, s: &Script) {
    s.connections.fetch_add(1, Ordering::SeqCst);
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut w = conn;
    let mut line = || {
        let mut l = String::new();
        r.read_line(&mut l).ok().filter(|n| *n > 0).map(|_| l.trim_end_matches('\n').to_string())
    };
    let Some(user) = line() else { return };
    let Some(cookie) = line() else { return };
    s.sessions.lock().unwrap().push(user.clone());
    if s.mute {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    if let Some(info) = &s.info {
        let _ = writeln!(w, "PAM_TEXT_INFO {}", ashd_svc_polkit::protocol::strescape(info));
    }
    let _ = writeln!(w, "PAM_PROMPT_ECHO_OFF Password: ");
    let Some(response) = line() else { return };
    if s.crash_next.load(Ordering::SeqCst) > 0 {
        s.crash_next.fetch_sub(1, Ordering::SeqCst);
        return; // dropped: EOF without a verdict
    }
    if s.passwords.get(&user) == Some(&response) {
        s.authenticated.lock().unwrap().push((user, cookie));
        let _ = writeln!(w, "SUCCESS");
    } else {
        if let Some(e) = &s.error {
            let _ = writeln!(w, "PAM_ERROR_MSG {}", ashd_svc_polkit::protocol::strescape(e));
        }
        let _ = writeln!(w, "FAILURE");
    }
}

pub fn me() -> (u32, String) {
    let id = ashd_svc_polkit::sysinfo::identity(ashd_svc_polkit::sysinfo::current_uid());
    (id.uid, id.name)
}

/// A connected pair: (mock authority side, agent side).
pub fn pair(log: &Log, with_login: bool) -> (Connection, Connection) {
    let (a, b) = UnixStream::pair().unwrap();
    smol::block_on(async {
        let mut server = Builder::async_io_unix_stream(a)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(AUTHORITY_PATH, Authority { log: log.clone() })
            .unwrap();
        if with_login {
            server = server
                .serve_at("/org/freedesktop/login1", LoginManager)
                .unwrap()
                .serve_at("/org/freedesktop/login1/session/c9", LoginSession)
                .unwrap();
        }
        let client = Builder::async_io_unix_stream(b).p2p().build();
        let (s, c) = smol::future::zip(server.build(), client).await;
        (s.unwrap(), c.unwrap())
    })
}

pub struct Fixture {
    pub server: Connection,
    pub log: Log,
    pub rx: Receiver<PolkitState>,
    pub handle: Handle,
    pub helper: FakeHelper,
    pub conns: Sender<Connection>,
}

pub fn options(helper: HelperTarget) -> Options {
    let mut o = Options::new(ashd_svc_polkit::Bus::System, helper);
    o.session_id = Some("c7".into());
    o.locale = "en_US.UTF-8".into();
    o.timeouts = Timeouts {
        connect: Duration::from_secs(2),
        first_line: Duration::from_millis(500),
        reply: Duration::from_secs(5),
    };
    o
}

pub fn fixture(script: Script) -> Fixture {
    let helper = FakeHelper::start(script);
    let opts = options(helper.target());
    fixture_with(helper, opts, false)
}

pub fn fixture_with(helper: FakeHelper, opts: Options, with_login: bool) -> Fixture {
    let log = Log::default();
    let (server, client) = pair(&log, with_login);
    let (conns, conns_rx) = async_channel::unbounded();
    conns.try_send(client).unwrap();
    let (handle, rx) = ashd_svc_polkit::start_on(conns_rx, opts);
    Fixture { server, log, rx, handle, helper, conns }
}

pub fn wait_for(rx: &Receiver<PolkitState>, what: &str, pred: impl Fn(&PolkitState) -> bool) -> PolkitState {
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

pub fn wait_log(log: &Log, want: &str) {
    for _ in 0..400 {
        if log.lock().unwrap().iter().any(|l| l == want) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("mock never saw `{want}`; saw {:?}", log.lock().unwrap());
}

/// The authority's side of `BeginAuthentication`, running in the background.
pub struct Call(std::sync::mpsc::Receiver<Result<(), String>>);

impl Call {
    /// Ok, or the D-Bus error name.
    pub fn result(&self) -> Result<(), String> {
        self.0.recv_timeout(Duration::from_secs(10)).expect("BeginAuthentication never returned")
    }
    pub fn pending(&self) -> bool {
        matches!(self.0.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
    }
}

pub fn begin(server: &Connection, action: &str, cookie: &str, uids: &[u32], details: &[(&str, &str)]) -> Call {
    let server = server.clone();
    let action = action.to_string();
    let cookie = cookie.to_string();
    let uids = uids.to_vec();
    let details: HashMap<String, String> = details.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let identities: Vec<(&str, HashMap<&str, Value<'_>>)> =
            uids.iter().map(|&u| ("unix-user", HashMap::from([("uid", Value::from(u))]))).collect();
        let body = (action.as_str(), "Authentication is needed to run `true' as the super user", "", details, cookie.as_str(), identities);
        let r = smol::block_on(server.call_method(
            None::<&str>,
            AGENT_PATH,
            Some("org.freedesktop.PolicyKit1.AuthenticationAgent"),
            "BeginAuthentication",
            &body,
        ));
        let _ = tx.send(r.map(|_| ()).map_err(|e| match e {
            zbus::Error::MethodError(name, _, _) => name.to_string(),
            other => other.to_string(),
        }));
    });
    Call(rx)
}

pub fn cancel(server: &Connection, cookie: &str) {
    smol::block_on(server.call_method(
        None::<&str>,
        AGENT_PATH,
        Some("org.freedesktop.PolicyKit1.AuthenticationAgent"),
        "CancelAuthentication",
        &(cookie,),
    ))
    .unwrap();
}

pub const CANCELLED: &str = "org.freedesktop.PolicyKit1.Error.Cancelled";
