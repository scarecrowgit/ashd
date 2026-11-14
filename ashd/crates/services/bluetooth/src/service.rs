//! The BlueZ session: object cache, signals, agent requests, commands.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use futures_lite::StreamExt as _;
use smol::{LocalExecutor, future::or};
use zbus::{
    Connection, Message, MessageStream,
    message::Type as MsgType,
    zvariant::{ObjectPath, OwnedObjectPath, Value},
};

use crate::{
    Options,
    agent::{AGENT_PATH, Agent, AgentEv, AgentReq, CAPABILITY},
    derive::{self, ADAPTER, DEVICE, Interfaces, Objects, Props},
    model::{BluetoothState, Busy, Command, PROMPT_SECS, PairingKind, PairingReply, PairingRequest, SCAN_SECS},
};

pub const BLUEZ: &str = "org.bluez";
const OBJMAN: &str = "org.freedesktop.DBus.ObjectManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Where the connection comes from.
pub enum Source {
    /// The system bus, reconnecting with back-off.
    System,
    /// One given connection (a peer-to-peer one to a mock, in tests). No reconnection.
    Conn(Connection),
}

enum End {
    Closed,
    Lost,
}

pub(crate) async fn run(commands: Receiver<Command>, state: Sender<BluetoothState>, opts: Options) {
    if let Some(name) = opts.mock.as_deref() {
        crate::mock::run(name, commands, state).await;
        return;
    }
    let fake = opts.fake_commands;
    let mut once = match opts.source {
        Source::Conn(c) => Some(c),
        Source::System => None,
    };
    let system = once.is_none();
    let mut backoff = Duration::from_millis(500);
    let _ = state.force_send(BluetoothState::default());
    loop {
        while commands.try_recv().is_ok() {}
        let conn = if system { Connection::system().await } else { once.take().ok_or_else(|| zbus::Error::Failure("no more connections".into())) };
        let end = match conn {
            Ok(conn) => match session(&conn, &commands, &state, fake).await {
                Ok(end) => {
                    backoff = Duration::from_millis(500);
                    end
                }
                Err(e) => {
                    tracing::debug!("bluetooth: {e}");
                    End::Lost
                }
            },
            Err(e) => {
                tracing::debug!("bluetooth: cannot connect: {e}");
                End::Lost
            }
        };
        let _ = state.force_send(BluetoothState::default());
        if matches!(end, End::Closed) || !system {
            return;
        }
        // BlueZ may simply not be installed: look again now and then, cheaply.
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
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

enum Ev {
    Cmd(Command),
    CmdsClosed,
    Msg(Message),
    Agent(AgentEv),
    /// BlueZ went away (owner lost) or the connection dropped.
    Gone,
    /// BlueZ (re)appeared: reload everything.
    Reload,
    Done(Done),
}

/// A finished background operation.
struct Done {
    /// Device it was about; also the `busy` key.
    path: Option<String>,
    what: &'static str,
    result: Result<(), String>,
    kind: DoneKind,
}

enum DoneKind {
    Plain,
    Discovery,
    Pair,
}

struct Session {
    objects: Objects,
    busy: HashMap<String, Busy>,
    pairing: Option<(PairingRequest, Sender<PairingReply>, Instant)>,
    pair_seq: u64,
    /// Devices the user started pairing with: "just works" is accepted for these only.
    initiated: HashSet<String>,
    pairs_in_flight: usize,
    scan_deadline: Option<Instant>,
    error: Option<String>,
}

impl Session {
    fn state(&self) -> BluetoothState {
        let mut s = derive::state_of(&self.objects, &self.busy);
        s.pairing = self.pairing.as_ref().map(|(p, _, _)| p.clone());
        s.error = self.error.clone();
        s.scan_deadline = self.scan_deadline;
        s
    }

    fn device_name(&self, path: &str) -> (String, String) {
        let p = self.objects.get(path).and_then(|i| i.get(DEVICE));
        let text = |k: &str| p.and_then(|p| p.get(k)).and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_string));
        let address = text("Address").unwrap_or_default();
        let name = text("Alias").or_else(|| text("Name")).unwrap_or_else(|| address.clone());
        (address, name)
    }

    fn adapter_path(&self) -> Option<String> {
        derive::adapter_of(&self.objects).map(|a| a.path)
    }

    fn handle_message(&mut self, msg: &Message) {
        let header = msg.header();
        let (Some(path), Some(member)) = (header.path().map(|p| p.to_string()), header.member().map(|m| m.to_string())) else { return };
        let body = msg.body();
        match member.as_str() {
            "PropertiesChanged" => {
                if let Ok((iface, changed, invalidated)) = body.deserialize::<(String, Props, Vec<String>)>()
                    && path.starts_with("/org/bluez")
                {
                    derive::apply_changed(&mut self.objects, &path, &iface, changed, &invalidated);
                }
            }
            "InterfacesAdded" => {
                if let Ok((p, ifs)) = body.deserialize::<(OwnedObjectPath, Interfaces)>()
                    && p.as_str().starts_with("/org/bluez")
                {
                    self.objects.entry(p.to_string()).or_default().extend(ifs);
                }
            }
            "InterfacesRemoved" => {
                if let Ok((p, names)) = body.deserialize::<(OwnedObjectPath, Vec<String>)>() {
                    let key = p.to_string();
                    if let Some(entry) = self.objects.get_mut(&key) {
                        for n in names {
                            entry.remove(&n);
                        }
                        if entry.is_empty() {
                            self.objects.remove(&key);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

async fn call(conn: &Connection, path: &str, iface: &str, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<Message, String> {
    let p = ObjectPath::try_from(path).map_err(|e| e.to_string())?;
    conn.call_method(Some(BLUEZ), p, Some(iface), method, body).await.map_err(|e| e.to_string())
}

async fn set_prop(conn: &Connection, path: &str, iface: &str, name: &str, value: Value<'_>) -> Result<(), String> {
    call(conn, path, PROPS, "Set", &(iface, name, value)).await.map(|_| ())
}

async fn load(conn: &Connection) -> zbus::Result<Objects> {
    let reply = conn.call_method(Some(BLUEZ), "/", Some(OBJMAN), "GetManagedObjects", &()).await?;
    let all: HashMap<OwnedObjectPath, Interfaces> = reply.body().deserialize()?;
    Ok(all.into_iter().map(|(p, i)| (p.to_string(), i)).collect())
}

/// Strip the D-Bus noise from an error for the status line.
fn short_error(e: &str) -> String {
    // `org.freedesktop.DBus.Error.Failed: Page Timeout` → `Page Timeout`
    let msg = e.rsplit(": ").next().unwrap_or(e);
    let msg = msg.trim_matches(|c| c == '"' || c == '\'' || c == ' ');
    if msg.is_empty() { e.to_string() } else { msg.to_string() }
}

fn spawn_op<F>(ex: &LocalExecutor<'_>, tx: &Sender<Ev>, path: Option<String>, what: &'static str, kind: DoneKind, fut: F)
where
    F: Future<Output = Result<(), String>> + 'static,
{
    let tx = tx.clone();
    ex.spawn(async move {
        let result = fut.await;
        let _ = tx.send(Ev::Done(Done { path, what, result, kind })).await;
    })
    .detach();
}

async fn session(conn: &Connection, commands: &Receiver<Command>, state: &Sender<BluetoothState>, fake: bool) -> zbus::Result<End> {
    let objects = load(conn).await?;
    let (tx, rx) = async_channel::unbounded::<Ev>();
    let ex = LocalExecutor::new();

    // The agent object is served from the start, but only registered with BlueZ while a
    // pairing the user started is in flight.
    let (agent_tx, agent_rx) = async_channel::unbounded::<AgentEv>();
    conn.object_server().at(AGENT_PATH, Agent { events: agent_tx }).await?;
    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Ok(e) = agent_rx.recv().await {
                if tx.send(Ev::Agent(e)).await.is_err() {
                    break;
                }
            }
        }
    })
    .detach();

    // Signals: restrict to BlueZ's sender on a real bus (peer-to-peer has no names).
    let on_bus = conn.unique_name().is_some();
    let mut props_rule = zbus::MatchRule::builder().msg_type(MsgType::Signal).interface(PROPS)?.member("PropertiesChanged")?.path_namespace("/org/bluez")?;
    let mut objman_rule = zbus::MatchRule::builder().msg_type(MsgType::Signal).interface(OBJMAN)?.path("/")?;
    if on_bus {
        props_rule = props_rule.sender(BLUEZ)?;
        objman_rule = objman_rule.sender(BLUEZ)?;
    }
    let props_s = MessageStream::for_match_rule(props_rule.build(), conn, Some(256)).await?;
    let objman_s = MessageStream::for_match_rule(objman_rule.build(), conn, Some(64)).await?;
    for mut s in [props_s, objman_s] {
        let tx = tx.clone();
        ex.spawn(async move {
            while let Some(m) = s.next().await {
                if let Ok(m) = m
                    && tx.send(Ev::Msg(m)).await.is_err()
                {
                    return;
                }
            }
            let _ = tx.send(Ev::Gone).await;
        })
        .detach();
    }
    if on_bus {
        let dbus = zbus::fdo::DBusProxy::new(conn).await?;
        let mut owner = dbus.receive_name_owner_changed_with_args(&[(0, BLUEZ)]).await?;
        ex.spawn({
            let tx = tx.clone();
            async move {
                while let Some(sig) = owner.next().await {
                    let ev = match sig.args() {
                        Ok(a) if a.new_owner.is_some() => Ev::Reload,
                        _ => Ev::Gone,
                    };
                    let _ = tx.send(ev).await;
                }
            }
        })
        .detach();
    }
    ex.spawn({
        let tx = tx.clone();
        let commands = commands.clone();
        async move {
            while let Ok(c) = commands.recv().await {
                if tx.send(Ev::Cmd(c)).await.is_err() {
                    return;
                }
            }
            let _ = tx.send(Ev::CmdsClosed).await;
        }
    })
    .detach();

    let mut s = Session {
        objects,
        busy: HashMap::new(),
        pairing: None,
        pair_seq: 0,
        initiated: HashSet::new(),
        pairs_in_flight: 0,
        scan_deadline: None,
        error: None,
    };
    let mut last = s.state();
    let _ = state.force_send(last.clone());

    let main = async {
        loop {
            // Wake for the next deadline (discovery auto-stop, unanswered prompt).
            let next = [s.scan_deadline, s.pairing.as_ref().map(|(_, _, d)| *d)].into_iter().flatten().min();
            let ev = or(async { rx.recv().await.ok() }, async {
                match next {
                    Some(t) => smol::Timer::at(t).await,
                    None => std::future::pending().await,
                };
                None
            })
            .await;
            let mut batch = Vec::new();
            if let Some(e) = ev {
                batch.push(e);
                // Coalesce bursts (RSSI updates while scanning).
                while let Ok(e) = rx.try_recv() {
                    batch.push(e);
                }
            }
            for e in batch {
                match e {
                    Ev::CmdsClosed => return End::Closed,
                    Ev::Gone => return End::Lost,
                    Ev::Reload => match load(conn).await {
                        Ok(o) => s.objects = o,
                        Err(_) => return End::Lost,
                    },
                    Ev::Msg(m) => s.handle_message(&m),
                    Ev::Agent(a) => on_agent(&mut s, a),
                    Ev::Done(d) => on_done(&mut s, conn, &ex, &tx, d),
                    Ev::Cmd(c) => on_command(&mut s, conn, &ex, &tx, c, fake).await,
                }
            }
            let now = Instant::now();
            if s.scan_deadline.is_some_and(|t| now >= t) {
                s.scan_deadline = None;
                if let Some(a) = s.adapter_path() {
                    let conn = conn.clone();
                    spawn_op(&ex, &tx, None, "stop scan", DoneKind::Plain, async move { call(&conn, &a, "org.bluez.Adapter1", "StopDiscovery", &()).await.map(|_| ()) });
                }
            }
            if s.pairing.as_ref().is_some_and(|(_, _, d)| now >= *d) {
                s.pairing = None;
            }
            let cur = s.state();
            if cur != last {
                last = cur.clone();
                let _ = state.force_send(cur);
            }
        }
    };
    Ok(ex.run(main).await)
}

fn on_agent(s: &mut Session, ev: AgentEv) {
    let AgentEv::Request { device, req, reply } = ev else {
        s.pairing = None;
        return;
    };
    let (address, name) = s.device_name(&device);
    let kind = match req {
        AgentReq::RequestAuthorization | AgentReq::AuthorizeService { .. } if s.initiated.contains(&device) => {
            // "Just works" for the device the user is pairing with.
            let _ = reply.try_send(PairingReply::Accept);
            return;
        }
        AgentReq::RequestAuthorization => PairingKind::Authorize,
        AgentReq::AuthorizeService { uuid } => PairingKind::AuthorizeService { uuid },
        AgentReq::RequestConfirmation { passkey } => PairingKind::Confirm { passkey },
        AgentReq::DisplayPasskey { passkey, entered } => PairingKind::DisplayPasskey { passkey, entered },
        AgentReq::DisplayPinCode { pin } => PairingKind::DisplayPin { pin },
        AgentReq::RequestPinCode => PairingKind::RequestPin,
        AgentReq::RequestPasskey => PairingKind::RequestPasskey,
    };
    // A display update replaces the previous display prompt of the same device and keeps
    // its number.
    let seq = match &s.pairing {
        Some((p, _, _)) if !kind.needs_answer() && !p.kind.needs_answer() && p.path == device => p.seq,
        _ => {
            s.pair_seq += 1;
            s.pair_seq
        }
    };
    s.pairing = Some((PairingRequest { seq, path: device, address, name, kind }, reply, Instant::now() + Duration::from_secs(PROMPT_SECS)));
}

fn on_done(s: &mut Session, conn: &Connection, ex: &LocalExecutor<'_>, tx: &Sender<Ev>, d: Done) {
    if let Some(p) = &d.path {
        s.busy.remove(p);
    }
    match d.kind {
        DoneKind::Discovery if d.result.is_ok() => s.scan_deadline = Some(Instant::now() + Duration::from_secs(SCAN_SECS)),
        DoneKind::Pair => {
            s.pairs_in_flight = s.pairs_in_flight.saturating_sub(1);
            if let Some(p) = &d.path {
                s.initiated.remove(p);
            }
            // Display prompts end with the pairing.
            if s.pairing.as_ref().is_some_and(|(p, _, _)| !p.kind.needs_answer()) {
                s.pairing = None;
            }
            if s.pairs_in_flight == 0 {
                let conn = conn.clone();
                spawn_op(ex, tx, None, "unregister agent", DoneKind::Plain, async move {
                    call(&conn, "/org/bluez", "org.bluez.AgentManager1", "UnregisterAgent", &(ObjectPath::try_from(AGENT_PATH).map_err(|e| e.to_string())?,))
                        .await
                        .map(|_| ())
                });
            }
        }
        _ => {}
    }
    match d.result {
        Ok(()) => s.error = None,
        Err(e) => {
            // The agent unregistering after a rejected pairing is not worth a message.
            if d.what != "unregister agent" && d.what != "stop scan" {
                let e = short_error(&e);
                tracing::warn!("bluetooth: {} failed: {e}", d.what);
                s.error = Some(format!("{}: {e}", d.what));
            }
        }
    }
}

async fn on_command(s: &mut Session, conn: &Connection, ex: &LocalExecutor<'_>, tx: &Sender<Ev>, cmd: Command, fake: bool) {
    match &cmd {
        Command::Refresh => {
            if let Ok(o) = load(conn).await {
                s.objects = o;
            }
            return;
        }
        Command::PairingReply { seq, reply } => {
            if s.pairing.as_ref().is_some_and(|(p, _, _)| p.seq == *seq)
                && let Some((_, answer, _)) = s.pairing.take()
            {
                let _ = answer.try_send(reply.clone());
            }
            return;
        }
        _ if fake => {
            tracing::warn!(target: "ashd_svc_bluetooth::fake", "ASHD_NESTED: would run {cmd:?}");
            return;
        }
        _ => {}
    }
    s.error = None;
    let conn = conn.clone();
    let adapter = s.adapter_path();
    match cmd {
        Command::SetPowered(on) => {
            let Some(a) = adapter else { return };
            spawn_op(ex, tx, None, "power", DoneKind::Plain, async move { set_prop(&conn, &a, ADAPTER, "Powered", Value::from(on)).await });
        }
        Command::StartDiscovery => {
            let Some(a) = adapter else { return };
            spawn_op(ex, tx, None, "scan", DoneKind::Discovery, async move { call(&conn, &a, "org.bluez.Adapter1", "StartDiscovery", &()).await.map(|_| ()) });
        }
        Command::StopDiscovery => {
            let Some(a) = adapter else { return };
            s.scan_deadline = None;
            spawn_op(ex, tx, None, "stop scan", DoneKind::Plain, async move { call(&conn, &a, "org.bluez.Adapter1", "StopDiscovery", &()).await.map(|_| ()) });
        }
        Command::Connect { path } => {
            s.busy.insert(path.clone(), Busy::Connecting);
            let p = path.clone();
            spawn_op(ex, tx, Some(path), "connect", DoneKind::Plain, async move { call(&conn, &p, DEVICE, "Connect", &()).await.map(|_| ()) });
        }
        Command::Disconnect { path } => {
            s.busy.insert(path.clone(), Busy::Disconnecting);
            let p = path.clone();
            spawn_op(ex, tx, Some(path), "disconnect", DoneKind::Plain, async move { call(&conn, &p, DEVICE, "Disconnect", &()).await.map(|_| ()) });
        }
        Command::SetTrusted { path, trusted } => {
            spawn_op(ex, tx, None, "trust", DoneKind::Plain, async move { set_prop(&conn, &path, DEVICE, "Trusted", Value::from(trusted)).await });
        }
        Command::Remove { path } => {
            let Some(a) = adapter else { return };
            spawn_op(ex, tx, None, "forget", DoneKind::Plain, async move {
                let p = ObjectPath::try_from(path.as_str()).map_err(|e| e.to_string())?;
                call(&conn, &a, "org.bluez.Adapter1", "RemoveDevice", &(p,)).await.map(|_| ())
            });
        }
        Command::Pair { path } => {
            s.busy.insert(path.clone(), Busy::Pairing);
            s.initiated.insert(path.clone());
            s.pairs_in_flight += 1;
            let p = path.clone();
            spawn_op(ex, tx, Some(path), "pair", DoneKind::Pair, async move {
                let agent = ObjectPath::try_from(AGENT_PATH).map_err(|e| e.to_string())?;
                // "AlreadyExists" (registered by an earlier pairing in flight) is fine.
                let _ = call(&conn, "/org/bluez", "org.bluez.AgentManager1", "RegisterAgent", &(agent.clone(), CAPABILITY)).await;
                let _ = call(&conn, "/org/bluez", "org.bluez.AgentManager1", "RequestDefaultAgent", &(agent,)).await;
                call(&conn, &p, DEVICE, "Pair", &()).await?;
                // Paired by the user's own action: trust and connect as well.
                set_prop(&conn, &p, DEVICE, "Trusted", Value::from(true)).await?;
                call(&conn, &p, DEVICE, "Connect", &()).await.map(|_| ())
            });
        }
        Command::PairingReply { .. } | Command::Refresh => {}
    }
}
