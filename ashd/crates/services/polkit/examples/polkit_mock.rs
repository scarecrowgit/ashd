//! Mock polkit for the **nested session** (`cargo xtask nested … --with-bus`): a fake
//! `org.freedesktop.PolicyKit1` authority on the nested session's *private* bus plus a
//! fake agent helper on a Unix socket. It authenticates nothing: the "password" is a
//! fixed test string.
//!
//! ```text
//! polkit_mock serve --helper-socket $XDG_RUNTIME_DIR/fake-helper.sock [--verdict-delay-ms 1500]
//! ASHD_POLKIT_BUS=$DBUS_SESSION_BUS_ADDRESS ASHD_POLKIT_HELPER=socket:$XDG_RUNTIME_DIR/fake-helper.sock ashd-shell
//! polkit_mock begin --action org.freedesktop.policykit.exec --uids 0,1000 \
//!     --message "Authentication is needed to run …" --detail program=/usr/bin/true
//! polkit_mock cancel <cookie>
//! ```
//!
//! Refuses to run unless `ASHD_NESTED=1`: it must never claim the authority's name on a
//! real bus.

use std::{
    collections::HashMap,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use clap::{Parser, Subcommand};
use ashd_svc_polkit::{AUTHORITY_NAME, AUTHORITY_PATH, protocol::strescape};
use zbus::{
    Connection, interface,
    message::Header,
    zvariant::{OwnedValue, Value},
};

#[derive(Parser)]
struct Args {
    /// D-Bus address (default: $DBUS_SESSION_BUS_ADDRESS — the nested session's private bus).
    #[arg(long)]
    bus: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Own org.freedesktop.PolicyKit1 on the bus and serve the fake helper.
    Serve {
        #[arg(long)]
        helper_socket: PathBuf,
        /// The accepted "password".
        #[arg(long, default_value = "hunter2")]
        password: String,
        /// Pause before SUCCESS/FAILURE (to see the checking state).
        #[arg(long, default_value_t = 0)]
        verdict_delay_ms: u64,
        /// Send a PAM_TEXT_INFO line before each prompt.
        #[arg(long)]
        info: Option<String>,
    },
    /// Ask the mock authority to start an authentication (prints the cookie).
    Begin {
        #[arg(long, default_value = "org.freedesktop.policykit.exec")]
        action: String,
        #[arg(long, default_value = "Authentication is needed to run `/run/current-system/sw/bin/true' as the super user")]
        message: String,
        /// Comma-separated uids (default: the current user).
        #[arg(long, value_delimiter = ',')]
        uids: Vec<u32>,
        /// Extra details, `key=value` (repeatable).
        #[arg(long)]
        detail: Vec<String>,
        /// pid sent as polkit.subject-pid (default: the serving mock's pid).
        #[arg(long)]
        subject_pid: Option<u32>,
    },
    /// Ask the mock authority to cancel an authentication.
    Cancel { cookie: String },
}

#[derive(Default)]
struct Shared {
    agent: Mutex<Option<(String, String)>>,
    authenticated: Mutex<Vec<(String, String)>>,
    next: AtomicU64,
}

struct MockAuthority {
    shared: Arc<Shared>,
}

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl MockAuthority {
    fn register_authentication_agent(
        &self,
        subject: (String, HashMap<String, OwnedValue>),
        locale: String,
        object_path: String,
        #[zbus(header)] hdr: Header<'_>,
    ) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        eprintln!("mock: RegisterAuthenticationAgent({} {:?}, {locale}, {object_path}) from {sender}", subject.0, subject.1);
        *self.shared.agent.lock().unwrap() = Some((sender, object_path));
    }
    fn unregister_authentication_agent(&self, _subject: (String, HashMap<String, OwnedValue>), object_path: String) {
        eprintln!("mock: UnregisterAuthenticationAgent({object_path})");
        *self.shared.agent.lock().unwrap() = None;
    }
}

struct MockControl {
    shared: Arc<Shared>,
}

#[interface(name = "org.ashd.MockPolkit")]
impl MockControl {
    async fn begin(
        &self,
        action: String,
        message: String,
        uids: Vec<u32>,
        details: HashMap<String, String>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<String> {
        let Some((agent, path)) = self.shared.agent.lock().unwrap().clone() else {
            return Err(zbus::fdo::Error::Failed("no agent registered".into()));
        };
        let cookie = format!("mock-cookie-{}", self.shared.next.fetch_add(1, Ordering::SeqCst));
        let conn = conn.clone();
        let c = cookie.clone();
        std::thread::spawn(move || {
            let identities: Vec<(&str, HashMap<&str, Value<'_>>)> =
                uids.iter().map(|&u| ("unix-user", HashMap::from([("uid", Value::from(u))]))).collect();
            let body = (action.as_str(), message.as_str(), "", details, c.as_str(), identities);
            let r = smol::block_on(conn.call_method(
                Some(agent.as_str()),
                path.as_str(),
                Some("org.freedesktop.PolicyKit1.AuthenticationAgent"),
                "BeginAuthentication",
                &body,
            ));
            match r {
                Ok(_) => eprintln!("mock: BeginAuthentication({c}) -> ok"),
                Err(e) => eprintln!("mock: BeginAuthentication({c}) -> {e}"),
            }
        });
        Ok(cookie)
    }

    async fn cancel(&self, cookie: String, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let Some((agent, path)) = self.shared.agent.lock().unwrap().clone() else {
            return Err(zbus::fdo::Error::Failed("no agent registered".into()));
        };
        conn.call_method(
            Some(agent.as_str()),
            path.as_str(),
            Some("org.freedesktop.PolicyKit1.AuthenticationAgent"),
            "CancelAuthentication",
            &(cookie.as_str(),),
        )
        .await?;
        Ok(())
    }
}

fn serve_helper(conn: UnixStream, shared: &Shared, password: &str, delay: Duration, info: Option<&str>) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut w = conn;
    let mut line = || {
        let mut l = String::new();
        r.read_line(&mut l).ok().filter(|n| *n > 0).map(|_| l.trim_end_matches('\n').to_string())
    };
    let (Some(user), Some(cookie)) = (line(), line()) else { return };
    eprintln!("fake-helper: conversation for {user}");
    if let Some(info) = info {
        let _ = writeln!(w, "PAM_TEXT_INFO {}", strescape(info));
    }
    let _ = writeln!(w, "PAM_PROMPT_ECHO_OFF Password: ");
    let Some(answer) = line() else { return };
    std::thread::sleep(delay);
    if answer == password {
        // What the real (root) helper does with AuthenticationAgentResponse2.
        shared.authenticated.lock().unwrap().push((user.clone(), cookie.clone()));
        eprintln!("fake-helper: {user} authenticated ({cookie})");
        let _ = writeln!(w, "SUCCESS");
    } else {
        eprintln!("fake-helper: wrong answer for {user}");
        let _ = writeln!(w, "FAILURE");
    }
}

fn main() -> anyhow_free::Result {
    if std::env::var("ASHD_NESTED").as_deref() != Ok("1") {
        eprintln!("polkit_mock: refusing to run outside the nested session (ASHD_NESTED=1)");
        std::process::exit(2);
    }
    let args = Args::parse();
    let addr = args.bus.or_else(|| std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()).ok_or("no bus address")?;
    smol::block_on(async {
        match args.cmd {
            Cmd::Serve { helper_socket, password, verdict_delay_ms, info } => {
                let shared = Arc::new(Shared::default());
                let _ = std::fs::remove_file(&helper_socket);
                let listener = UnixListener::bind(&helper_socket)?;
                let s = shared.clone();
                std::thread::spawn(move || {
                    for conn in listener.incoming().flatten() {
                        let (s, p, i) = (s.clone(), password.clone(), info.clone());
                        std::thread::spawn(move || {
                            serve_helper(conn, &s, &p, Duration::from_millis(verdict_delay_ms), i.as_deref())
                        });
                    }
                });
                let _conn = zbus::connection::Builder::address(addr.as_str())?
                    .name(AUTHORITY_NAME)?
                    .serve_at(AUTHORITY_PATH, MockAuthority { shared: shared.clone() })?
                    .serve_at("/org/ashd/MockPolkit", MockControl { shared })?
                    .build()
                    .await?;
                eprintln!("mock: serving {AUTHORITY_NAME} (pid {}), helper at {}", std::process::id(), helper_socket.display());
                std::future::pending::<()>().await;
                Ok(())
            }
            Cmd::Begin { action, message, uids, detail, subject_pid } => {
                let conn = zbus::connection::Builder::address(addr.as_str())?.build().await?;
                let uids = if uids.is_empty() { vec![ashd_svc_polkit::sysinfo::current_uid()] } else { uids };
                let mut details: HashMap<String, String> =
                    detail.iter().filter_map(|d| d.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
                let pid = match subject_pid {
                    Some(p) => p,
                    None => {
                        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
                        dbus.get_connection_unix_process_id(zbus::names::BusName::try_from(AUTHORITY_NAME)?).await?
                    }
                };
                details.entry("polkit.subject-pid".into()).or_insert_with(|| pid.to_string());
                details.entry("polkit.caller-pid".into()).or_insert_with(|| std::process::id().to_string());
                let reply = conn
                    .call_method(
                        Some(AUTHORITY_NAME),
                        "/org/ashd/MockPolkit",
                        Some("org.ashd.MockPolkit"),
                        "Begin",
                        &(action, message, uids, details),
                    )
                    .await?;
                let cookie: String = reply.body().deserialize()?;
                println!("{cookie}");
                Ok(())
            }
            Cmd::Cancel { cookie } => {
                let conn = zbus::connection::Builder::address(addr.as_str())?.build().await?;
                conn.call_method(Some(AUTHORITY_NAME), "/org/ashd/MockPolkit", Some("org.ashd.MockPolkit"), "Cancel", &(cookie,))
                    .await?;
                Ok(())
            }
        }
    })
}

/// Tiny boxed-error result (no anyhow in this crate's dependencies).
mod anyhow_free {
    pub type Result = std::result::Result<(), Box<dyn std::error::Error>>;
}
