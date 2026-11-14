//! The agent: D-Bus object, registration with the authority, the request queue and the
//! helper conversations. Runs on the `ashd-svc-polkit` thread (one `LocalExecutor`).

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_channel::{Receiver, Sender};
use futures_lite::StreamExt as _;
use smol::{LocalExecutor, future::or};
use zbus::{
    Connection,
    message::Header,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
use zeroize::Zeroizing;

use crate::{
    AUTHORITY_NAME, AuthRequest, Command, Options, Phase, PolkitState, RETRY_MAX, RETRY_MIN,
    helper::{ConvEvent, converse},
    protocol::{HelperLine, check_response},
    sysinfo,
};

/// Errors returned to the authority (`org.freedesktop.PolicyKit1.Error.*`, as
/// libpolkit-agent does).
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
    NotAuthorized(String),
}

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn register_authentication_agent(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        locale: &str,
        object_path: &str,
    ) -> zbus::Result<()>;
    fn unregister_authentication_agent(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        object_path: &str,
    ) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    fn get_session(&self, id: &str) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1")]
trait LoginSession {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
}

pub(crate) struct Begin {
    action_id: String,
    message: String,
    icon_name: String,
    details: BTreeMap<String, String>,
    cookie: String,
    uids: Vec<u32>,
    reply: Sender<Result<(), AgentError>>,
}

pub(crate) enum Ev {
    Begin(Begin),
    AuthorityCancel(String),
    Cmd(Command),
    CmdsClosed,
    Conv(u64, ConvEvent),
    /// New owner of `org.freedesktop.PolicyKit1` (`None`: the authority went away).
    Owner(Option<String>),
    /// The connection is gone.
    Lost,
}

/// The exported `org.freedesktop.PolicyKit1.AuthenticationAgent`.
struct AgentObject {
    tx: Sender<Ev>,
    /// `Some(owner)` in a bus session: only the authority may call us.
    authority: Option<Arc<Mutex<Option<String>>>>,
}

impl AgentObject {
    fn check_sender(&self, hdr: &Header<'_>) -> Result<(), AgentError> {
        let Some(owner) = &self.authority else { return Ok(()) };
        let sender = hdr.sender().map(|s| s.to_string());
        let owner = owner.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if sender.is_some() && sender == owner {
            Ok(())
        } else {
            tracing::warn!("polkit: rejected a call from {sender:?}, which is not the authority");
            Err(AgentError::NotAuthorized("only the polkit authority may call the agent".into()))
        }
    }
}

fn uid_of(kind: &str, details: &HashMap<String, OwnedValue>) -> Option<u32> {
    if kind != "unix-user" {
        // polkitd expands unix-group admin identities into their members before calling.
        tracing::debug!("polkit: skipping identity of kind {kind}");
        return None;
    }
    let v = details.get("uid")?;
    u32::try_from(v).ok().or_else(|| i32::try_from(v).ok().and_then(|i| u32::try_from(i).ok()))
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl AgentObject {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<(), AgentError> {
        self.check_sender(&hdr)?;
        let uids = identities.iter().filter_map(|(k, d)| uid_of(k, d)).collect();
        let (reply, result) = async_channel::bounded(1);
        let begin = Begin { action_id, message, icon_name, details: details.into_iter().collect(), cookie, uids, reply };
        self.tx.send(Ev::Begin(begin)).await.map_err(|_| AgentError::Failed("the agent is shutting down".into()))?;
        result.recv().await.unwrap_or_else(|_| Err(AgentError::Failed("the agent is shutting down".into())))
    }

    async fn cancel_authentication(&self, cookie: String, #[zbus(header)] hdr: Header<'_>) -> Result<(), AgentError> {
        self.check_sender(&hdr)?;
        let _ = self.tx.send(Ev::AuthorityCancel(cookie)).await;
        Ok(())
    }
}

enum End {
    Closed,
    Lost,
}

pub(crate) async fn run<F, Fut>(commands: Receiver<Command>, state: Sender<PolkitState>, opts: Options, bus: bool, mut connect: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = zbus::Result<Connection>>,
{
    let mut st = PolkitState { enabled: true, ..Default::default() };
    let _ = state.force_send(st.clone());
    let mut backoff = RETRY_MIN;
    let mut next_id = 1;
    loop {
        // Commands sent while disconnected refer to nothing (responses are wiped on drop).
        while commands.try_recv().is_ok() {}
        let end = match connect().await {
            Ok(conn) => {
                let mut core = Core::new(&opts, &state, st.clone(), next_id);
                let end = core.session(&conn, &commands, bus).await;
                next_id = core.next_id;
                st = core.st;
                backoff = RETRY_MIN;
                end
            }
            Err(e) => {
                tracing::debug!("polkit: cannot connect: {e}");
                st.error = Some(format!("cannot connect to the bus: {e}"));
                End::Lost
            }
        };
        st.available = false;
        st.pending = None;
        st.queue_len = 0;
        let _ = state.force_send(st.clone());
        if matches!(end, End::Closed) {
            return;
        }
        let closed = or(
            async {
                smol::Timer::after(backoff).await;
                false
            },
            async {
                while let Ok(c) = commands.recv().await {
                    if matches!(c, Command::Shutdown) {
                        return true;
                    }
                }
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

struct Entry {
    req: AuthRequest,
    cookie: String,
    reply: Sender<Result<(), AgentError>>,
    /// `PAM_ERROR_MSG` of the running conversation.
    pam_error: Option<String>,
}

struct Conv {
    gen_: u64,
    responses: Sender<Zeroizing<String>>,
    _task: smol::Task<()>,
}

struct Core<'a> {
    opts: &'a Options,
    state: &'a Sender<PolkitState>,
    st: PolkitState,
    queue: VecDeque<Entry>,
    conv: Option<Conv>,
    gen_: u64,
    next_id: u64,
    me: u32,
    events_tx: Sender<Ev>,
    events_rx: Receiver<Ev>,
}

fn subject(session: &str) -> (&'static str, HashMap<&'static str, Value<'_>>) {
    ("unix-session", HashMap::from([("session-id", Value::from(session))]))
}

const CANCELLED: &str = "Authentication dialog was dismissed by the user";

impl<'a> Core<'a> {
    fn new(opts: &'a Options, state: &'a Sender<PolkitState>, st: PolkitState, next_id: u64) -> Self {
        let (events_tx, events_rx) = async_channel::unbounded();
        Self {
            opts,
            state,
            st,
            queue: VecDeque::new(),
            conv: None,
            gen_: 0,
            next_id,
            me: sysinfo::current_uid(),
            events_tx,
            events_rx,
        }
    }

    fn publish(&mut self) {
        self.st.pending = self.queue.front().map(|e| e.req.clone());
        self.st.queue_len = self.queue.len().saturating_sub(1);
        let _ = self.state.force_send(self.st.clone());
    }

    async fn session_id(&self, conn: &Connection) -> zbus::Result<String> {
        if let Some(id) = &self.opts.session_id {
            return Ok(id.clone());
        }
        // Not started from inside the session (a systemd user unit): logind's `auto` is
        // the caller's session, else the user's display session.
        let mgr = LoginManagerProxy::builder(conn).cache_properties(zbus::proxy::CacheProperties::No).build().await?;
        let path = mgr.get_session("auto").await?;
        let sess = LoginSessionProxy::builder(conn).path(path)?.cache_properties(zbus::proxy::CacheProperties::No).build().await?;
        sess.id().await
    }

    async fn session(&mut self, conn: &Connection, commands: &Receiver<Command>, bus: bool) -> End {
        match self.session_inner(conn, commands, bus).await {
            Ok(end) => end,
            Err(e) => {
                tracing::warn!("polkit: {e}");
                self.st.error = Some(e.to_string());
                End::Lost
            }
        }
    }

    async fn session_inner(&mut self, conn: &Connection, commands: &Receiver<Command>, bus: bool) -> zbus::Result<End> {
        let session = match self.session_id(conn).await {
            Ok(s) => s,
            Err(e) => {
                self.st.error = Some(format!("no logind session to register for: {e}"));
                return Ok(End::Lost);
            }
        };
        self.st.session_id = Some(session.clone());
        let authority = AuthorityProxy::builder(conn).cache_properties(zbus::proxy::CacheProperties::No).build().await?;

        let ex = LocalExecutor::new();
        let (tx, rx) = (self.events_tx.clone(), self.events_rx.clone());
        let owner = Arc::new(Mutex::new(None::<String>));
        let object = AgentObject { tx: tx.clone(), authority: bus.then(|| owner.clone()) };
        conn.object_server().at(self.opts.object_path.as_str(), object).await?;

        // Lost connection: every message stream ends.
        let mut stream = zbus::MessageStream::from(conn);
        ex.spawn({
            let tx = tx.clone();
            async move {
                while stream.next().await.is_some() {}
                let _ = tx.send(Ev::Lost).await;
            }
        })
        .detach();
        if bus {
            let dbus = zbus::fdo::DBusProxy::new(conn).await?;
            let mut changes = dbus.receive_name_owner_changed_with_args(&[(0, AUTHORITY_NAME)]).await?;
            ex.spawn({
                let tx = tx.clone();
                async move {
                    while let Some(sig) = changes.next().await {
                        if let Ok(args) = sig.args() {
                            let new = args.new_owner().as_ref().map(|n| n.to_string());
                            let _ = tx.send(Ev::Owner(new)).await;
                        }
                    }
                }
            })
            .detach();
            let current = match zbus::names::BusName::try_from(AUTHORITY_NAME) {
                Ok(name) => dbus.get_name_owner(name).await.ok().map(|o| o.to_string()),
                Err(_) => None,
            };
            *owner.lock().unwrap_or_else(|e| e.into_inner()) = current.clone();
            if current.is_none() {
                self.st.error = Some("the polkit authority is not running".into());
            }
        }
        ex.spawn({
            let tx = tx.clone();
            let commands = commands.clone();
            async move {
                while let Ok(c) = commands.recv().await {
                    let _ = tx.send(Ev::Cmd(c)).await;
                }
                let _ = tx.send(Ev::CmdsClosed).await;
            }
        })
        .detach();

        let main = async {
            let authority_up = !bus || owner.lock().unwrap_or_else(|e| e.into_inner()).is_some();
            // In a bus session a failed registration waits for the next authority owner;
            // over a peer-to-peer link it means the link is useless.
            if authority_up
                && let Err(e) = self.register(&authority, &session).await
                && !bus
            {
                tracing::warn!("polkit: RegisterAuthenticationAgent: {e}");
                return End::Lost;
            }
            self.publish();
            loop {
                let Ok(ev) = rx.recv().await else { return End::Lost };
                match ev {
                    Ev::CmdsClosed | Ev::Cmd(Command::Shutdown) => {
                        self.drop_all(|| AgentError::Cancelled("the agent is shutting down".into()));
                        if self.st.available {
                            let subj = subject(&session);
                            let unreg = authority.unregister_authentication_agent(&subj, &self.opts.object_path);
                            let _ = or(async { Some(unreg.await) }, async {
                                smol::Timer::after(Duration::from_secs(2)).await;
                                None
                            })
                            .await;
                        }
                        self.st.available = false;
                        return End::Closed;
                    }
                    Ev::Lost => {
                        self.drop_all(|| AgentError::Failed("connection lost".into()));
                        self.st.error = Some("lost the bus connection".into());
                        return End::Lost;
                    }
                    Ev::Owner(new) => {
                        // The authority restarted (or stopped): its pending calls are gone.
                        self.drop_all(|| AgentError::Failed("the authority went away".into()));
                        *owner.lock().unwrap_or_else(|e| e.into_inner()) = new.clone();
                        self.st.available = false;
                        if new.is_some() {
                            tracing::info!("polkit: authority (re)started, registering again");
                            let _ = self.register(&authority, &session).await;
                        } else {
                            self.st.error = Some("the polkit authority is not running".into());
                        }
                    }
                    Ev::Begin(b) => self.begin(b, &ex),
                    Ev::AuthorityCancel(cookie) => self.authority_cancel(&cookie, &ex),
                    Ev::Cmd(cmd) => self.command(cmd, &ex),
                    Ev::Conv(g, e) => {
                        if self.conv.as_ref().is_some_and(|c| c.gen_ == g) {
                            self.conversation_event(e, &ex);
                        }
                    }
                }
                self.publish();
            }
        };
        let end = ex.run(main).await;
        self.conv = None;
        let _ = conn.object_server().remove::<AgentObject, _>(self.opts.object_path.as_str()).await;
        Ok(end)
    }

    async fn register(&mut self, authority: &AuthorityProxy<'_>, session: &str) -> zbus::Result<()> {
        match authority.register_authentication_agent(&subject(session), &self.opts.locale, &self.opts.object_path).await {
            Ok(()) => {
                tracing::info!("polkit: registered as the authentication agent of session {session}");
                self.st.available = true;
                self.st.error = None;
                Ok(())
            }
            Err(e) => {
                tracing::warn!("polkit: RegisterAuthenticationAgent failed: {e}");
                self.st.available = false;
                self.st.error = Some(format!("cannot register with polkit: {e}"));
                Err(e)
            }
        }
    }

    fn begin(&mut self, b: Begin, ex: &LocalExecutor<'_>) {
        let mut uids = Vec::new();
        for uid in b.uids {
            if !uids.contains(&uid) {
                uids.push(uid);
            }
        }
        if uids.is_empty() {
            let _ = b.reply.try_send(Err(AgentError::Failed("no identity to authenticate as".into())));
            return;
        }
        let identities: Vec<_> = uids.iter().map(|&u| sysinfo::identity(u)).collect();
        let selected = if uids.contains(&self.me) { self.me } else { uids[0] };
        let processes = sysinfo::processes(&b.details, &self.opts.proc_root);
        let id = self.next_id;
        self.next_id += 1;
        tracing::info!("polkit: request {id} for {}", b.action_id);
        self.queue.push_back(Entry {
            req: AuthRequest {
                id,
                action_id: b.action_id,
                message: b.message,
                icon_name: b.icon_name,
                details: b.details,
                identities,
                selected: Some(selected),
                processes,
                ..Default::default()
            },
            cookie: b.cookie,
            reply: b.reply,
            pam_error: None,
        });
        if self.queue.len() == 1 {
            self.start_conversation(ex);
        }
    }

    /// New helper conversation for the current request's selected identity.
    fn start_conversation(&mut self, ex: &LocalExecutor<'_>) {
        self.conv = None;
        let Some(e) = self.queue.front_mut() else { return };
        e.req.phase = Phase::Starting;
        e.req.prompt = None;
        e.req.echo = false;
        e.req.info = None;
        e.pam_error = None;
        let user = e.req.selected_identity().map(|i| i.name.clone()).unwrap_or_default();
        self.gen_ += 1;
        let gen_ = self.gen_;
        let (responses, rrx) = async_channel::unbounded();
        let (ctx, crx) = async_channel::unbounded();
        let evtx = self.events_tx.clone();
        let transports = self.opts.helper.transports();
        let cookie = e.cookie.clone();
        let timeouts = self.opts.timeouts;
        let task = ex.spawn(async move {
            let forward = async {
                while let Ok(ev) = crx.recv().await {
                    if evtx.send(Ev::Conv(gen_, ev)).await.is_err() {
                        break;
                    }
                }
            };
            smol::future::zip(converse(transports, user, cookie, timeouts, ctx, rrx), forward).await;
        });
        self.conv = Some(Conv { gen_, responses, _task: task });
    }

    /// Complete the current request and move on to the next one.
    fn finish(&mut self, result: Result<(), AgentError>, ex: &LocalExecutor<'_>) {
        self.conv = None;
        let Some(e) = self.queue.pop_front() else { return };
        let ok = result.is_ok();
        tracing::info!("polkit: request {} {}", e.req.id, if ok { "authenticated" } else { "ended" });
        if ok {
            self.st.succeeded += 1;
        }
        let _ = e.reply.try_send(result);
        if !self.queue.is_empty() {
            self.start_conversation(ex);
        }
    }

    fn drop_all(&mut self, err: impl Fn() -> AgentError) {
        self.conv = None;
        for e in self.queue.drain(..) {
            let _ = e.reply.try_send(Err(err()));
        }
    }

    fn authority_cancel(&mut self, cookie: &str, ex: &LocalExecutor<'_>) {
        let Some(pos) = self.queue.iter().position(|e| e.cookie == cookie) else { return };
        if pos == 0 {
            self.finish(Err(AgentError::Cancelled("cancelled by the authority".into())), ex);
        } else if let Some(e) = self.queue.remove(pos) {
            let _ = e.reply.try_send(Err(AgentError::Cancelled("cancelled by the authority".into())));
        }
    }

    fn command(&mut self, cmd: Command, ex: &LocalExecutor<'_>) {
        let current = self.queue.front().map(|e| e.req.id);
        let targets = |id: Option<u64>| current.is_some() && (id.is_none() || id == current);
        match cmd {
            Command::Respond { id, response } if targets(id) => {
                let Some(e) = self.queue.front_mut() else { return };
                if e.req.phase != Phase::Prompting {
                    return; // `response` is wiped on drop
                }
                if let Err(err) = check_response(&response) {
                    e.req.error = Some(err.to_string());
                    return;
                }
                if let Some(c) = &self.conv
                    && c.responses.try_send(response).is_ok()
                {
                    e.req.phase = Phase::Checking;
                    e.req.error = None;
                    e.req.info = None;
                }
            }
            Command::SelectIdentity { id, uid } if targets(id) => {
                let Some(e) = self.queue.front_mut() else { return };
                if e.req.selected != Some(uid) && e.req.identities.iter().any(|i| i.uid == uid) {
                    e.req.selected = Some(uid);
                    e.req.error = None;
                    e.req.attempts = 0;
                    self.start_conversation(ex);
                }
            }
            Command::Cancel { id } if targets(id) => self.finish(Err(AgentError::Cancelled(CANCELLED.into())), ex),
            Command::Retry { id } if targets(id) && self.queue.front().is_some_and(|e| e.req.phase == Phase::Unavailable) => {
                if let Some(e) = self.queue.front_mut() {
                    e.req.error = None;
                }
                self.start_conversation(ex);
            }
            _ => {}
        }
    }

    fn conversation_event(&mut self, ev: ConvEvent, ex: &LocalExecutor<'_>) {
        let Some(e) = self.queue.front_mut() else { return };
        match ev {
            ConvEvent::Line(HelperLine::Prompt { text, echo }) => {
                e.req.prompt = Some(text);
                e.req.echo = echo;
                e.req.phase = Phase::Prompting;
            }
            ConvEvent::Line(HelperLine::Info(s)) => e.req.info = Some(s),
            ConvEvent::Line(HelperLine::Error(s)) => {
                e.req.error = Some(s.clone());
                e.pam_error = Some(s);
            }
            ConvEvent::Line(HelperLine::Success) => self.finish(Ok(()), ex),
            ConvEvent::Line(HelperLine::Failure | HelperLine::Unknown(_)) => {
                let answered = e.req.phase == Phase::Checking;
                e.req.attempts += 1;
                let msg = e.pam_error.take().unwrap_or_else(|| {
                    if answered { "Wrong password. Try again.".into() } else { "Authentication failed.".into() }
                });
                tracing::info!("polkit: request {} attempt {} failed", e.req.id, e.req.attempts);
                if answered {
                    // A rejected answer: new conversation, the error stays visible.
                    self.start_conversation(ex);
                    if let Some(e) = self.queue.front_mut() {
                        e.req.error = Some(msg);
                    }
                } else {
                    // Failed before any answer (broken helper / PAM): no automatic retry
                    // loop; the user retries or cancels.
                    self.conv = None;
                    e.req.phase = Phase::Unavailable;
                    e.req.error = Some(msg);
                }
            }
            ConvEvent::Closed(reason) => {
                tracing::warn!("polkit: helper conversation ended: {reason}");
                self.conv = None;
                e.req.phase = Phase::Unavailable;
                e.req.error = Some(format!("Authentication helper failed: {reason}."));
            }
        }
    }
}
