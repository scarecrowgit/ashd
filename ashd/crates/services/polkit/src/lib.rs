//! polkit authentication agent (zbus 5, no GPUI).
//!
//! The shell's built-in replacement for a third-party agent (soteria): registers an
//! `org.freedesktop.PolicyKit1.AuthenticationAgent` for the current logind session,
//! publishes every `BeginAuthentication` as [`PolkitState::pending`] (one at a time, the
//! rest queued: [`PolkitState::queue_len`]) and runs the PAM conversation through polkit's
//! helper ([`helper`]). The dialog (`ashd-panel-polkit-dialog`) shows the request and
//! sends [`Handle::respond`] / [`Handle::select_identity`] / [`Handle::cancel`].
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<PolkitState>)`.
//!
//! ## Enabling
//!
//! [`start`] reads the environment ([`options_from_env`]):
//! - **`ASHD_POLKIT_AGENT=ashd`** (set by the NixOS module when `ashd.polkitAgent =
//!   "ashd"`): register on the system bus, helper auto-detected. Otherwise the agent stays
//!   off (`PolkitState::enabled == false`) — another agent (soteria) owns the session.
//! - **nested session** (`ASHD_NESTED=1`): never the real system bus or the real helper.
//!   The agent runs only if both `ASHD_POLKIT_BUS` (a D-Bus address serving a mock
//!   authority) and `ASHD_POLKIT_HELPER` (`socket:/path`, `exec:/path` or a path of a fake
//!   helper) are set; both variables are ignored outside a nested session.
//!
//! ## Security
//!
//! Responses (passwords) travel only as [`zeroize::Zeroizing`] buffers: from the dialog
//! through the command channel to the helper socket, wiped when dropped. They are never
//! logged and never part of the state; [`Command`]'s `Debug` redacts them. The dialog
//! shows exactly what polkit sent: message, action id, the details map and the requesting
//! processes resolved from `polkit.subject-pid` / `polkit.caller-pid`. In a bus session,
//! `BeginAuthentication` / `CancelAuthentication` are accepted only from the current owner
//! of `org.freedesktop.PolicyKit1`.
//!
//! On `SUCCESS` the helper (root) has already sent `AuthenticationAgentResponse2` to the
//! authority — only uid 0 may call it — so the agent just completes `BeginAuthentication`.
//!
//! Testing: `tests/mock_authority.rs` serves a mock authority over peer-to-peer
//! connections and a fake helper over a Unix socket (and `examples/polkit_fake_helper.rs`
//! as an exec helper); `examples/polkit_mock.rs` does the same on a private bus for the
//! nested session.

mod agent;
pub mod helper;
pub mod protocol;
pub mod sysinfo;

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use async_channel::{Receiver, Sender};
pub use helper::{HelperTarget, Timeouts, Transport};
use zeroize::Zeroizing;

/// The object path the agent is exported at.
pub const AGENT_PATH: &str = "/org/ashd/PolicyKit1/AuthenticationAgent";
/// The authority's well-known name.
pub const AUTHORITY_NAME: &str = "org.freedesktop.PolicyKit1";
pub const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";

/// A user the request may be authenticated as.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub uid: u32,
    /// Login name (what the helper authenticates).
    pub name: String,
    /// GECOS full name, else the login name.
    pub display_name: String,
}

impl Identity {
    /// `Full Name (login)`, or the login name alone.
    pub fn label(&self) -> String {
        if self.display_name == self.name { self.name.clone() } else { format!("{} ({})", self.display_name, self.name) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessRole {
    /// `polkit.subject-pid`: the process the authorization is for (e.g. `pkexec`).
    Subject,
    /// `polkit.caller-pid`: the process that asked polkit.
    Caller,
}

/// A requesting process, resolved from `/proc` when it is still there and readable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub role: ProcessRole,
    pub pid: u32,
    /// `/proc/<pid>/exe` (not readable for other users' processes).
    pub exe: Option<String>,
    /// `/proc/<pid>/comm`.
    pub comm: Option<String>,
    /// `/proc/<pid>/cmdline`, arguments joined with spaces, truncated.
    pub cmdline: Option<String>,
}

impl ProcessInfo {
    /// The executable path, else the command name, else `pid N`.
    pub fn program(&self) -> String {
        self.exe.clone().or_else(|| self.comm.clone()).unwrap_or_else(|| format!("pid {}", self.pid))
    }
}

/// Where the current request stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Connecting to the helper / waiting for its first prompt.
    #[default]
    Starting,
    /// The helper asked [`AuthRequest::prompt`]; waiting for [`Handle::respond`].
    Prompting,
    /// A response was sent; waiting for the helper's verdict.
    Checking,
    /// The helper is unreachable or ended unexpectedly ([`AuthRequest::error`] says why);
    /// [`Handle::retry`] starts a new conversation, [`Handle::cancel`] gives up.
    Unavailable,
}

/// One `BeginAuthentication` call, as shown by the dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthRequest {
    /// Unique per service run (commands may name it to avoid acting on the wrong request).
    pub id: u64,
    pub action_id: String,
    /// polkit's (already localised) message.
    pub message: String,
    pub icon_name: String,
    /// Every detail polkit sent, sorted by key.
    pub details: BTreeMap<String, String>,
    pub identities: Vec<Identity>,
    /// uid of the identity being authenticated.
    pub selected: Option<u32>,
    /// The helper's current prompt (`Password: `).
    pub prompt: Option<String>,
    /// The answer to `prompt` may be shown (`PAM_PROMPT_ECHO_ON`).
    pub echo: bool,
    /// Last `PAM_TEXT_INFO`.
    pub info: Option<String>,
    /// Last error: `PAM_ERROR_MSG`, a failed attempt, an unreachable helper.
    pub error: Option<String>,
    pub phase: Phase,
    /// Failed attempts so far.
    pub attempts: u32,
    /// The requesting processes (`polkit.subject-pid`, `polkit.caller-pid`).
    pub processes: Vec<ProcessInfo>,
}

impl AuthRequest {
    pub fn selected_identity(&self) -> Option<&Identity> {
        self.selected.and_then(|uid| self.identities.iter().find(|i| i.uid == uid))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolkitState {
    /// The built-in agent is configured to run here (see the crate docs).
    pub enabled: bool,
    /// Registered with the authority for [`Self::session_id`].
    pub available: bool,
    pub session_id: Option<String>,
    /// Why the agent is not (or no longer) registered.
    pub error: Option<String>,
    /// The request the dialog shows.
    pub pending: Option<AuthRequest>,
    /// Further requests waiting behind `pending`.
    pub queue_len: usize,
    /// Successful authentications (counter).
    pub succeeded: u64,
}

/// Commands. `id: None` = the current request; `Some(id)` is ignored unless that request
/// is the current one.
pub enum Command {
    SelectIdentity { id: Option<u64>, uid: u32 },
    /// The answer to the current prompt (a password). Wiped when dropped.
    Respond { id: Option<u64>, response: Zeroizing<String> },
    Cancel { id: Option<u64> },
    /// New helper conversation after [`Phase::Unavailable`].
    Retry { id: Option<u64> },
    /// Cancel everything, unregister, stop the thread.
    Shutdown,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SelectIdentity { id, uid } => write!(f, "SelectIdentity {{ id: {id:?}, uid: {uid} }}"),
            Command::Respond { id, .. } => write!(f, "Respond {{ id: {id:?}, response: <redacted> }}"),
            Command::Cancel { id } => write!(f, "Cancel {{ id: {id:?} }}"),
            Command::Retry { id } => write!(f, "Retry {{ id: {id:?} }}"),
            Command::Shutdown => f.write_str("Shutdown"),
        }
    }
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        // On a closed channel the command (and any response in it) is dropped and wiped.
        let _ = self.tx.try_send(cmd);
    }
    /// Authenticate as `uid` instead (restarts the conversation).
    pub fn select_identity(&self, uid: u32) {
        self.send(Command::SelectIdentity { id: None, uid });
    }
    /// Answer the current prompt.
    pub fn respond(&self, response: Zeroizing<String>) {
        self.send(Command::Respond { id: None, response });
    }
    /// Dismiss the current request (`BeginAuthentication` fails with `Cancelled`).
    pub fn cancel(&self) {
        self.send(Command::Cancel { id: None });
    }
    pub fn select_identity_for(&self, id: u64, uid: u32) {
        self.send(Command::SelectIdentity { id: Some(id), uid });
    }
    pub fn respond_to(&self, id: u64, response: Zeroizing<String>) {
        self.send(Command::Respond { id: Some(id), response });
    }
    pub fn cancel_request(&self, id: u64) {
        self.send(Command::Cancel { id: Some(id) });
    }
    pub fn retry(&self, id: u64) {
        self.send(Command::Retry { id: Some(id) });
    }
    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Which bus the authority is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bus {
    System,
    /// A D-Bus address (`unix:path=…`) — a mock authority in the nested session.
    Address(String),
}

/// Everything the agent needs; [`options_from_env`] builds the real one.
#[derive(Clone, Debug)]
pub struct Options {
    pub bus: Bus,
    pub helper: HelperTarget,
    /// logind session to register for; `None`: `$XDG_SESSION_ID`, else logind's `auto`.
    pub session_id: Option<String>,
    pub locale: String,
    pub object_path: String,
    pub timeouts: Timeouts,
    /// Where requesting pids are looked up (`/proc`).
    pub proc_root: PathBuf,
}

impl Options {
    pub fn new(bus: Bus, helper: HelperTarget) -> Self {
        Self {
            bus,
            helper,
            session_id: std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty()),
            locale: locale(),
            object_path: AGENT_PATH.into(),
            timeouts: Timeouts::default(),
            proc_root: "/proc".into(),
        }
    }
}

fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "C".into())
}

fn env_flag(k: &str) -> bool {
    std::env::var(k).is_ok_and(|v| v == "1")
}

/// Options from the environment, or why the agent stays off (see the crate docs).
pub fn options_from_env() -> Result<Options, String> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if env_flag("ASHD_NESTED") {
        let (Some(bus), Some(helper)) = (get("ASHD_POLKIT_BUS"), get("ASHD_POLKIT_HELPER")) else {
            return Err("nested session without ASHD_POLKIT_BUS and ASHD_POLKIT_HELPER: polkit agent off".into());
        };
        return Ok(Options::new(Bus::Address(bus), HelperTarget::parse(&helper)));
    }
    if get("ASHD_POLKIT_BUS").is_some() || get("ASHD_POLKIT_HELPER").is_some() {
        tracing::warn!("polkit: ASHD_POLKIT_BUS / ASHD_POLKIT_HELPER are ignored outside a nested session");
    }
    match get("ASHD_POLKIT_AGENT").as_deref() {
        Some("ashd" | "1" | "true") => Ok(Options::new(Bus::System, HelperTarget::Auto)),
        _ => Err("built-in polkit agent off (ASHD_POLKIT_AGENT is not `ashd`)".into()),
    }
}

fn disabled(reason: String) -> (Handle, Receiver<PolkitState>) {
    tracing::info!("polkit: {reason}");
    let (state_tx, state_rx) = async_channel::bounded(1);
    let _ = state_tx.force_send(PolkitState { error: Some(reason), ..Default::default() });
    (Handle::disconnected(), state_rx)
}

/// Spawn the `ashd-svc-polkit` thread configured from the environment (off unless
/// enabled, see the crate docs). Never fails.
pub fn start() -> (Handle, Receiver<PolkitState>) {
    match options_from_env() {
        Ok(opts) => start_with(opts),
        Err(reason) => disabled(reason),
    }
}

/// Spawn the agent with explicit options (reconnects to the bus with back-off).
pub fn start_with(opts: Options) -> (Handle, Receiver<PolkitState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-polkit".into())
        .spawn(move || {
            let bus = opts.bus.clone();
            smol::block_on(agent::run(rx, state_tx, opts, true, move || {
                let bus = bus.clone();
                async move {
                    match bus {
                        Bus::System => zbus::Connection::system().await,
                        Bus::Address(a) => zbus::connection::Builder::address(a.as_str())?.build().await,
                    }
                }
            }))
        })
        .expect("spawn ashd-svc-polkit");
    (Handle { tx }, state_rx)
}

/// Like [`start_with`] over existing peer-to-peer connections to an authority (tests):
/// each time the current one is lost the next is taken from `conns` — that is how an
/// authority restart is simulated. No sender verification, no bus-name tracking.
pub fn start_on(conns: Receiver<zbus::Connection>, opts: Options) -> (Handle, Receiver<PolkitState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-polkit".into())
        .spawn(move || {
            smol::block_on(agent::run(rx, state_tx, opts, false, move || {
                let conns = conns.clone();
                async move { conns.recv().await.map_err(|_| zbus::Error::Failure("no more connections".into())) }
            }))
        })
        .expect("spawn ashd-svc-polkit");
    (Handle { tx }, state_rx)
}

pub(crate) const RETRY_MIN: Duration = Duration::from_millis(500);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(10);
