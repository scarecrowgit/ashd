//! The bus side: discovery, one watcher task per player, command execution.
//!
//! Everything runs on one thread (`smol::block_on` + a `LocalExecutor`). The central
//! loop never awaits a call to a player — reads happen in the player's own watcher and
//! commands are spawned — so a hung player cannot stall the others or the UI.

use std::{cell::Cell, collections::HashMap, future::Future, rc::Rc, time::Duration};

use async_channel::{Receiver, Sender};
use futures_lite::StreamExt as _;
use smol::{LocalExecutor, Task, future::or};
use zbus::{
    Connection, Proxy,
    fdo::DBusProxy,
    proxy::{CacheProperties, SignalStream},
    zvariant::{OwnedObjectPath, OwnedValue},
};

use crate::{
    BUS_PREFIX, Command, MAX_QUEUE, MprisState, POLL_INTERVAL, RETRY_MAX, RETRY_MIN, Track, art_path_from_url,
    model::Model,
    parse::{PlayerProps, Props, RootProps, parse_player, parse_root, parse_track, usable_track_id},
};

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const TRACKLIST_IFACE: &str = "org.mpris.MediaPlayer2.TrackList";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

/// A call to a player that has not answered after this long is abandoned.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// A player that claimed its name but has not exported the objects yet is retried for
/// `START_TRIES × START_RETRY` before it is given up on.
const START_TRIES: u32 = 20;
const START_RETRY: Duration = Duration::from_millis(500);

enum End {
    Closed,
    Lost,
}

pub(crate) async fn run(commands: Receiver<Command>, state: Sender<MprisState>, address: Option<String>) {
    let mut backoff = RETRY_MIN;
    let _ = state.force_send(MprisState::default());
    loop {
        while commands.try_recv().is_ok() {}
        let connected = match &address {
            Some(a) => match zbus::connection::Builder::address(a.as_str()) {
                Ok(b) => b.build().await,
                Err(e) => Err(e),
            },
            None => match zbus::connection::Builder::session() {
                Ok(b) => b.build().await,
                Err(e) => Err(e),
            },
        };
        let end = match connected {
            Ok(conn) => match session(&conn, &commands, &state).await {
                Ok(end) => {
                    backoff = RETRY_MIN;
                    end
                }
                Err(e) => {
                    tracing::debug!("mpris: {e}");
                    End::Lost
                }
            },
            Err(e) => {
                tracing::debug!("mpris: no session bus: {e}");
                End::Lost
            }
        };
        let _ = state.force_send(MprisState::default());
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

/// Events of the central loop.
enum Ev {
    Cmd(Command),
    CmdsClosed,
    /// `NameOwnerChanged` for an MPRIS name (`old` / `new` are unique names).
    Owner { name: String, new: Option<String> },
    Initial { name: String, owner: String, root: RootProps, props: PlayerProps, queue: Option<Vec<Track>> },
    Root { name: String, owner: String, root: RootProps },
    Props { name: String, owner: String, props: PlayerProps },
    Position { name: String, owner: String, position: Duration },
    Queue { name: String, owner: String, queue: Option<Vec<Track>> },
    /// A watcher could not start (the player never answered).
    Failed { name: String, owner: String },
    /// The bus connection is gone.
    Gone,
}

struct Watcher {
    owner: String,
    poke: Sender<()>,
    _task: Task<()>,
}

fn is_player_name(name: &str) -> bool {
    // `playerctld` mirrors whichever player was active: ignoring it avoids duplicates.
    name.starts_with(BUS_PREFIX) && name != "org.mpris.MediaPlayer2.playerctld"
}

async fn session(conn: &Connection, commands: &Receiver<Command>, state: &Sender<MprisState>) -> zbus::Result<End> {
    let ex: Rc<LocalExecutor<'static>> = Rc::new(LocalExecutor::new());
    let dbus = DBusProxy::new(conn).await?;
    // Subscribe before listing: no name can appear in between unnoticed.
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    let names = dbus.list_names().await?;

    let (tx, rx) = async_channel::unbounded::<Ev>();
    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Some(sig) = owner_changes.next().await {
                if let Ok(args) = sig.args() {
                    let name = args.name().to_string();
                    if is_player_name(&name) {
                        let new = args.new_owner().as_ref().map(|o| o.to_string());
                        let _ = tx.send(Ev::Owner { name, new }).await;
                    }
                }
            }
            let _ = tx.send(Ev::Gone).await;
        }
    })
    .detach();
    ex.spawn({
        let (tx, commands) = (tx.clone(), commands.clone());
        async move {
            while let Ok(c) = commands.recv().await {
                let _ = tx.send(Ev::Cmd(c)).await;
            }
            let _ = tx.send(Ev::CmdsClosed).await;
        }
    })
    .detach();
    for name in names {
        let name = name.to_string();
        if !is_player_name(&name) {
            continue;
        }
        if let Ok(bus_name) = zbus::names::BusName::try_from(name.as_str())
            && let Ok(owner) = dbus.get_name_owner(bus_name).await
        {
            let _ = tx.send(Ev::Owner { name, new: Some(owner.to_string()) }).await;
        }
    }

    let polling = Rc::new(Cell::new(true));
    let mut model = Model::connected();
    let mut watchers: HashMap<String, Watcher> = HashMap::new();
    let mut published = MprisState::default();
    publish(state, &model, &mut published);

    let main = async {
        loop {
            let Ok(ev) = rx.recv().await else { return End::Lost };
            let dirty = match ev {
                Ev::CmdsClosed => return End::Closed,
                Ev::Gone => return End::Lost,
                Ev::Cmd(cmd) => exec(cmd, &ex, conn, &mut model, &watchers, &polling),
                Ev::Owner { name, new } => {
                    let mut dirty = false;
                    match new {
                        Some(owner) if watchers.get(&name).is_some_and(|w| w.owner == owner) => {}
                        Some(owner) => {
                            // new player, or the name changed hands
                            watchers.remove(&name);
                            dirty = model.remove(&name, None);
                            let (poke, poke_rx) = async_channel::unbounded();
                            let task = ex.spawn(watch(Watch {
                                conn: conn.clone(),
                                ex: ex.clone(),
                                name: name.clone(),
                                owner: owner.clone(),
                                tx: tx.clone(),
                                polling: polling.clone(),
                                poke: poke_rx,
                            }));
                            watchers.insert(name, Watcher { owner, poke, _task: task });
                        }
                        None => {
                            watchers.remove(&name);
                            dirty = model.remove(&name, None);
                        }
                    }
                    dirty
                }
                Ev::Initial { name, owner, root, props, queue } => {
                    if watchers.get(&name).is_some_and(|w| w.owner == owner) {
                        model.insert(&name, &owner, root, props, queue);
                        true
                    } else {
                        false
                    }
                }
                Ev::Root { name, owner, root } => model.update_root(&name, &owner, root),
                Ev::Props { name, owner, props } => model.update_props(&name, &owner, props),
                Ev::Position { name, owner, position } => model.update_position(&name, &owner, position),
                Ev::Queue { name, owner, queue } => model.update_queue(&name, &owner, queue),
                Ev::Failed { name, owner } => {
                    if watchers.get(&name).is_some_and(|w| w.owner == owner) {
                        watchers.remove(&name);
                    }
                    false
                }
            };
            if dirty {
                publish(state, &model, &mut published);
            }
        }
    };
    Ok(ex.run(main).await)
}

fn publish(state: &Sender<MprisState>, model: &Model, published: &mut MprisState) {
    let snapshot = model.state();
    if snapshot != *published {
        *published = snapshot.clone();
        let _ = state.force_send(snapshot);
    }
}

// ── commands ──────────────────────────────────────────────────────────────────

/// Run `fut` in the background with a deadline; failures are logged.
fn spawn_call(ex: &LocalExecutor<'static>, what: &'static str, fut: impl Future<Output = zbus::Result<()>> + 'static) {
    ex.spawn(async move {
        if let Err(e) = timeout(fut).await {
            tracing::debug!("mpris: {what} failed: {e}");
        }
    })
    .detach();
}

async fn timeout<T>(fut: impl Future<Output = zbus::Result<T>>) -> zbus::Result<T> {
    or(fut, async {
        smol::Timer::after(CALL_TIMEOUT).await;
        Err(zbus::Error::Failure("timed out".into()))
    })
    .await
}

/// Returns whether the model changed (it publishes then).
fn exec(
    cmd: Command,
    ex: &LocalExecutor<'static>,
    conn: &Connection,
    model: &mut Model,
    watchers: &HashMap<String, Watcher>,
    polling: &Cell<bool>,
) -> bool {
    let simple = |target: Option<String>, method: &'static str, iface: &'static str| {
        if let Some((_, owner, _)) = model.resolve(target.as_deref()) {
            let (conn, owner) = (conn.clone(), owner.to_string());
            spawn_call(ex, method, async move {
                conn.call_method(Some(owner.as_str()), PATH, Some(iface), method, &()).await.map(|_| ())
            });
        }
    };
    match cmd {
        Command::PlayPause(t) => simple(t, "PlayPause", PLAYER_IFACE),
        Command::Next(t) => simple(t, "Next", PLAYER_IFACE),
        Command::Previous(t) => simple(t, "Previous", PLAYER_IFACE),
        Command::Stop(t) => simple(t, "Stop", PLAYER_IFACE),
        Command::GoTo { player, track_id } => {
            if let Some((_, owner, _)) = model.resolve(player.as_deref())
                && let Ok(path) = OwnedObjectPath::try_from(track_id)
            {
                let (conn, owner) = (conn.clone(), owner.to_string());
                spawn_call(ex, "GoTo", async move {
                    conn.call_method(Some(owner.as_str()), PATH, Some(TRACKLIST_IFACE), "GoTo", &(path,)).await.map(|_| ())
                });
            }
        }
        Command::SeekTo { player, position } => {
            let Some((name, owner, p)) = model.resolve(player.as_deref()) else { return false };
            if !p.can_seek {
                return false;
            }
            let position = p.track.length.map_or(position, |len| position.min(len));
            let target_us = i64::try_from(position.as_micros()).unwrap_or(i64::MAX);
            let current_us = i64::try_from(p.position.as_micros()).unwrap_or(0);
            let track = p.track.id.as_deref().and_then(usable_track_id).map(|id| OwnedObjectPath::from(id.to_owned()));
            let (name, owner) = (name.to_string(), owner.to_string());
            let conn = conn.clone();
            let call_owner = owner.clone();
            match track {
                Some(id) => spawn_call(ex, "SetPosition", async move {
                    conn.call_method(Some(call_owner.as_str()), PATH, Some(PLAYER_IFACE), "SetPosition", &(id, target_us))
                        .await
                        .map(|_| ())
                }),
                // No usable track id (some players): move relative to where we think it is.
                None => spawn_call(ex, "Seek", async move {
                    conn.call_method(Some(call_owner.as_str()), PATH, Some(PLAYER_IFACE), "Seek", &(target_us - current_us,))
                        .await
                        .map(|_| ())
                }),
            }
            // Optimistic: the bar must not snap back before the player says `Seeked`.
            return model.update_position(&name, &owner, position);
        }
        Command::SetActive(name) => return model.set_pinned(name),
        Command::SetPositionPolling(on) => {
            polling.set(on);
            if on {
                for w in watchers.values() {
                    let _ = w.poke.try_send(());
                }
            }
        }
    }
    false
}

// ── per-player watcher ────────────────────────────────────────────────────────

struct Watch {
    conn: Connection,
    ex: Rc<LocalExecutor<'static>>,
    name: String,
    owner: String,
    tx: Sender<Ev>,
    polling: Rc<Cell<bool>>,
    poke: Receiver<()>,
}

/// What wakes a watcher.
enum W {
    Root,
    Player,
    TrackList,
    Seeked(i64),
    Poll,
}

async fn watch(w: Watch) {
    if let Err(e) = watch_inner(&w).await {
        tracing::debug!("mpris: {}: {e}", w.name);
        let _ = w.tx.send(Ev::Failed { name: w.name.clone(), owner: w.owner.clone() }).await;
    }
}

async fn proxy(conn: &Connection, owner: &str, interface: &'static str) -> zbus::Result<Proxy<'static>> {
    zbus::proxy::Builder::new(conn)
        .destination(owner.to_string())?
        .path(PATH)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// Forward a signal stream into the watcher's channel.
fn forward(
    ex: &LocalExecutor<'static>,
    mut stream: SignalStream<'static>,
    tx: Sender<W>,
    map: impl Fn(&zbus::Message) -> Option<W> + 'static,
) -> Task<()> {
    ex.spawn(async move {
        while let Some(msg) = stream.next().await {
            if let Some(w) = map(&msg) {
                let _ = tx.send(w).await;
            }
        }
    })
}

async fn get_all(conn: &Connection, owner: &str, iface: &str) -> zbus::Result<Props> {
    let reply = timeout(conn.call_method(Some(owner), PATH, Some(PROPS_IFACE), "GetAll", &(iface,))).await?;
    reply.body().deserialize::<Props>()
}

async fn get_position(conn: &Connection, owner: &str) -> Option<Duration> {
    let reply = timeout(conn.call_method(Some(owner), PATH, Some(PROPS_IFACE), "Get", &(PLAYER_IFACE, "Position")))
        .await
        .ok()?;
    let v: OwnedValue = reply.body().deserialize().ok()?;
    let us = i64::try_from(v).ok()?;
    u64::try_from(us).ok().map(Duration::from_micros)
}

/// `None`: the player has no readable `TrackList`.
async fn read_queue(conn: &Connection, owner: &str) -> Option<Vec<Track>> {
    let reply = timeout(conn.call_method(Some(owner), PATH, Some(PROPS_IFACE), "Get", &(TRACKLIST_IFACE, "Tracks")))
        .await
        .ok()?;
    let v: OwnedValue = reply.body().deserialize().ok()?;
    let ids: Vec<OwnedObjectPath> = v.try_into().ok()?;
    let ids: Vec<OwnedObjectPath> = ids.into_iter().take(MAX_QUEUE).collect();
    if ids.is_empty() {
        return Some(Vec::new());
    }
    let reply = timeout(conn.call_method(Some(owner), PATH, Some(TRACKLIST_IFACE), "GetTracksMetadata", &(&ids,)))
        .await
        .ok()?;
    let mds: Vec<Props> = reply.body().deserialize().ok()?;
    Some(
        ids.iter()
            .zip(mds)
            .map(|(id, md)| Track { id: Some(id.to_string()), ..parse_track(&md) })
            .collect(),
    )
}

async fn read_player(conn: &Connection, owner: &str) -> zbus::Result<PlayerProps> {
    let mut props = parse_player(&get_all(conn, owner, PLAYER_IFACE).await?);
    // Only a file that exists is worth handing to an image element.
    props.track.art_path = props.track.art_url.as_deref().and_then(art_path_from_url).filter(|p| p.is_file());
    Ok(props)
}

async fn watch_inner(w: &Watch) -> zbus::Result<()> {
    let (wtx, wrx) = async_channel::unbounded::<W>();
    let mut tasks: Vec<Task<()>> = Vec::new();

    // Subscribe first, read afterwards: nothing can change unnoticed in between.
    let props_p = proxy(&w.conn, &w.owner, PROPS_IFACE).await?;
    tasks.push(forward(&w.ex, props_p.receive_signal("PropertiesChanged").await?, wtx.clone(), |msg| {
        let (iface, _, _): (String, Props, Vec<String>) = msg.body().deserialize().ok()?;
        match iface.as_str() {
            ROOT_IFACE => Some(W::Root),
            PLAYER_IFACE => Some(W::Player),
            TRACKLIST_IFACE => Some(W::TrackList),
            _ => None,
        }
    }));
    let player_p = proxy(&w.conn, &w.owner, PLAYER_IFACE).await?;
    tasks.push(forward(&w.ex, player_p.receive_signal("Seeked").await?, wtx.clone(), |msg| {
        msg.body().deserialize::<i64>().ok().map(W::Seeked)
    }));
    let tracklist_p = proxy(&w.conn, &w.owner, TRACKLIST_IFACE).await?;
    tasks.push(forward(&w.ex, tracklist_p.receive_all_signals().await?, wtx.clone(), |_| Some(W::TrackList)));
    tasks.push(w.ex.spawn({
        let (poke, wtx) = (w.poke.clone(), wtx.clone());
        async move {
            while poke.recv().await.is_ok() {
                let _ = wtx.send(W::Poll).await;
            }
        }
    }));

    // First full read. Players that claimed the name before exporting their objects get
    // a few more chances.
    let mut tries = 0;
    let (root, mut props) = loop {
        let read = async {
            let root = parse_root(&get_all(&w.conn, &w.owner, ROOT_IFACE).await?);
            Ok::<_, zbus::Error>((root, read_player(&w.conn, &w.owner).await?))
        };
        match read.await {
            Ok(r) => break r,
            Err(e) if tries + 1 < START_TRIES => {
                tracing::trace!("mpris: {} not ready: {e}", w.name);
                tries += 1;
                smol::Timer::after(START_RETRY).await;
            }
            Err(e) => return Err(e),
        }
    };
    let queue = read_queue(&w.conn, &w.owner).await;
    let send = |ev: Ev| async { let _ = w.tx.send(ev).await; };
    send(Ev::Initial { name: w.name.clone(), owner: w.owner.clone(), root, props: props.clone(), queue }).await;

    loop {
        let playing = props.status == crate::PlaybackStatus::Playing && w.polling.get();
        let first = if playing {
            or(async { wrx.recv().await.ok() }, async {
                smol::Timer::after(POLL_INTERVAL).await;
                Some(W::Poll)
            })
            .await
        } else {
            wrx.recv().await.ok()
        };
        let Some(first) = first else { return Ok(()) };

        // Coalesce whatever else is already queued: one refresh per kind.
        let (mut root, mut player, mut queue, mut poll, mut seeked) = (false, false, false, false, None);
        let mut note = |ev: W| match ev {
            W::Root => root = true,
            W::Player => player = true,
            W::TrackList => queue = true,
            W::Poll => poll = true,
            W::Seeked(us) => seeked = Some(us),
        };
        note(first);
        while let Ok(more) = wrx.try_recv() {
            note(more);
        }

        if root && let Ok(all) = get_all(&w.conn, &w.owner, ROOT_IFACE).await {
            send(Ev::Root { name: w.name.clone(), owner: w.owner.clone(), root: parse_root(&all) }).await;
        }
        if player {
            // `GetAll` includes the position, so a polled position is not needed as well.
            if let Ok(p) = read_player(&w.conn, &w.owner).await {
                props = p;
                send(Ev::Props { name: w.name.clone(), owner: w.owner.clone(), props: props.clone() }).await;
            }
        } else if poll
            && w.polling.get()
            && let Some(position) = get_position(&w.conn, &w.owner).await
        {
            send(Ev::Position { name: w.name.clone(), owner: w.owner.clone(), position }).await;
        }
        if let Some(us) = seeked
            && let Ok(us) = u64::try_from(us)
        {
            send(Ev::Position { name: w.name.clone(), owner: w.owner.clone(), position: Duration::from_micros(us) }).await;
        }
        if queue {
            let queue = read_queue(&w.conn, &w.owner).await;
            send(Ev::Queue { name: w.name.clone(), owner: w.owner.clone(), queue }).await;
        }
    }
}
