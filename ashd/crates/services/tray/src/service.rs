//! The bus side: watcher / host, one watcher task per item, the dbusmenu of the open menu.
//!
//! Everything runs on one thread (`smol::block_on` + a `LocalExecutor`). The central loop
//! never awaits a call to an app: reads happen in the item's own task, commands are
//! spawned and every call has a deadline, so a hung app cannot stall the others or the UI.

use std::{
    collections::HashSet,
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, SystemTime},
};

use async_channel::{Receiver, Sender};
use futures_lite::{StreamExt as _, future::poll_once};
use smol::{LocalExecutor, Task, future::or};
use zbus::{
    Connection, MatchRule, MessageStream,
    fdo::{DBusProxy, RequestNameFlags, RequestNameReply},
    message::Type as MessageType,
    zvariant::OwnedValue,
};

use crate::{
    Command, Config, DEFAULT_ITEM_PATH, MenuNode, OpenMenu, RETRY_MAX, RETRY_MIN, TrayItem, TrayState, WATCHER_NAME,
    WATCHER_PATH, icons,
    icons::Resolver,
    menu,
    model::{RawItem, build_item, parse_item, parse_service},
    watcher::{self, Shared, Watcher},
};

const WATCHER_IFACE: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACES: [&str; 2] = ["org.kde.StatusNotifierItem", "org.freedesktop.StatusNotifierItem"];
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

/// A call to an app that has not answered after this long is abandoned.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// An item that registered before it exported its objects is retried for
/// `START_TRIES × START_RETRY` before it is given up on.
const START_TRIES: u32 = 20;
const START_RETRY: Duration = Duration::from_millis(500);
/// Signal bursts (`NewIcon` per frame of an animation) become one refresh per interval.
const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
/// Above this many cached pixmaps the unreferenced old ones are deleted.
const CACHE_PRUNE_ABOVE: usize = 256;

pub(crate) async fn timeout<T>(dur: Duration, fut: impl Future<Output = zbus::Result<T>>) -> zbus::Result<T> {
    or(fut, async {
        smol::Timer::after(dur).await;
        Err(zbus::Error::Failure("timed out".into()))
    })
    .await
}

enum End {
    Closed,
    Lost,
}

/// Events of the central loop.
pub(crate) enum Ev {
    Cmd(Command),
    CmdsClosed,
    /// `NameOwnerChanged` (unique names included: an item may be registered by either).
    Name { name: String, new: Option<String> },
    /// An item to track, `"<bus><path>"`. `announce`: found by this service (not
    /// registered through its watcher object), so the watcher list must learn about it.
    Registered { key: String, announce: bool },
    Unregistered { key: String },
    Item { key: String, owner: String, iface: &'static str, item: Box<TrayItem> },
    ItemFailed { key: String },
    Menu { seq: u64, item: String, root: Box<MenuNode> },
    MenuFailed { seq: u64, item: String },
    /// The bus connection is gone.
    Gone,
}

pub(crate) async fn run(commands: Receiver<Command>, state: Sender<TrayState>, config: Config) {
    let mut backoff = RETRY_MIN;
    let _ = state.force_send(TrayState::default());
    loop {
        while commands.try_recv().is_ok() {}
        let connected = match &config.address {
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
            Ok(conn) => match session(&conn, &commands, &state, &config).await {
                Ok(end) => {
                    backoff = RETRY_MIN;
                    end
                }
                Err(e) => {
                    tracing::debug!("tray: {e}");
                    End::Lost
                }
            },
            Err(e) => {
                tracing::debug!("tray: no session bus: {e}");
                End::Lost
            }
        };
        let _ = state.force_send(TrayState::default());
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

/// `org.kde.StatusNotifierHost-<pid>`; a second service in one process (tests) gets a
/// suffix.
fn host_name() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let (pid, n) = (std::process::id(), N.fetch_add(1, Ordering::Relaxed));
    if n == 0 { format!("org.kde.StatusNotifierHost-{pid}") } else { format!("org.kde.StatusNotifierHost-{pid}-{n}") }
}

struct Entry {
    key: String,
    bus: String,
    path: String,
    /// Unique name of the app, known once its first read worked.
    owner: Option<String>,
    iface: &'static str,
    /// `None` until the first read worked: not shown before.
    item: Option<TrayItem>,
    _task: Task<()>,
}

struct Svc<'c> {
    conn: &'c Connection,
    ex: Rc<LocalExecutor<'static>>,
    tx: Sender<Ev>,
    wdata: Shared,
    resolver: Resolver,
    host: String,
    entries: Vec<Entry>,
    is_watcher: bool,
    /// Owner of the watcher we are a host of (when not the watcher ourselves).
    remote_owner: Option<String>,
    remote_task: Option<Task<()>>,
    menu: Option<OpenMenu>,
    menu_seq: u64,
    menu_item: Option<String>,
    menu_task: Option<Task<()>>,
}

async fn session(conn: &Connection, commands: &Receiver<Command>, state: &Sender<TrayState>, config: &Config) -> zbus::Result<End> {
    let ex: Rc<LocalExecutor<'static>> = Rc::new(LocalExecutor::new());
    let (tx, rx) = async_channel::unbounded::<Ev>();
    let wdata = Shared::default();
    let cache = config.cache_dir.clone().unwrap_or_else(icons::cache_dir);
    // Stale pixmaps of an earlier run are of no use; a new run starts clean.
    let _ = std::fs::remove_dir_all(&cache);

    // The watcher object first (calls arriving right after the name is owned must find it).
    conn.object_server().at(WATCHER_PATH, Watcher { data: wdata.clone(), events: tx.clone() }).await?;
    let dbus = DBusProxy::new(conn).await?;
    // Subscribe before requesting: no change can slip in between.
    let mut owner_changes = dbus.receive_name_owner_changed().await?;

    ex.spawn({
        let tx = tx.clone();
        async move {
            while let Some(sig) = owner_changes.next().await {
                if let Ok(args) = sig.args() {
                    let name = args.name().to_string();
                    let new = args.new_owner().as_ref().map(|o| o.to_string());
                    let _ = tx.send(Ev::Name { name, new }).await;
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

    let host = host_name();
    // Our host name: best effort (no queueing, no replacing).
    if let Err(e) = conn.request_name_with_flags(host.as_str(), RequestNameFlags::DoNotQueue.into()).await {
        tracing::debug!("tray: host name {host}: {e}");
    }
    // The watcher name: NOT `ReplaceExisting` (zbus' `Default` flags are ReplaceExisting | DoNotQueue,
    // and other zbus-based shells set AllowReplacement, so a default request would steal it), and queued, so
    // when the current owner leaves the bus promotes us and `NameOwnerChanged` says so.
    // the empty flag set (`BitFlags::default()` of this enum is not empty)
    let no_flags = (RequestNameFlags::DoNotQueue | RequestNameFlags::ReplaceExisting) & RequestNameFlags::AllowReplacement;
    let reply = conn.request_name_with_flags(WATCHER_NAME, no_flags).await?;
    let is_watcher = matches!(reply, RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner);

    let mut svc = Svc {
        conn,
        ex: ex.clone(),
        tx: tx.clone(),
        wdata,
        resolver: Resolver::new(cache.clone()),
        host,
        entries: Vec::new(),
        is_watcher: false,
        remote_owner: None,
        remote_task: None,
        menu: None,
        menu_seq: 0,
        menu_item: None,
        menu_task: None,
    };
    if is_watcher {
        svc.become_watcher().await;
    } else if let Ok(owner) = dbus.get_name_owner(zbus::names::BusName::try_from(WATCHER_NAME)?).await {
        svc.attach_remote(owner.to_string());
    }

    let mut published = TrayState::default();
    let mut publishes = 0u32;
    svc.publish(state, &mut published, &mut publishes, &cache);
    let main = async {
        loop {
            let Ok(ev) = rx.recv().await else { return End::Lost };
            match ev {
                Ev::CmdsClosed => return End::Closed,
                Ev::Gone => return End::Lost,
                ev => {
                    if svc.handle(ev).await {
                        svc.publish(state, &mut published, &mut publishes, &cache);
                    }
                }
            }
        }
    };
    Ok(ex.run(main).await)
}

impl Svc<'_> {
    fn snapshot(&self) -> TrayState {
        TrayState {
            available: true,
            is_watcher: self.is_watcher,
            items: self.entries.iter().filter_map(|e| e.item.clone()).collect(),
            menu: self.menu.clone(),
        }
    }

    fn publish(&self, state: &Sender<TrayState>, published: &mut TrayState, publishes: &mut u32, cache: &Path) {
        let snapshot = self.snapshot();
        if snapshot == *published {
            return;
        }
        *published = snapshot.clone();
        let _ = state.force_send(snapshot);
        *publishes += 1;
        if (*publishes).is_multiple_of(32) {
            prune_cache(cache, &self.snapshot());
        }
    }

    fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// We own the watcher name (at start, or after the previous owner left).
    async fn become_watcher(&mut self) {
        self.is_watcher = true;
        self.remote_owner = None;
        self.remote_task = None;
        {
            let mut d = watcher::lock(&self.wdata);
            for e in &self.entries {
                if !d.items.contains(&e.key) {
                    d.items.push(e.key.clone());
                }
            }
        }
        watcher::announce_host(self.conn, &self.wdata, &self.host).await;
        // Items that registered with an earlier watcher and never will again.
        if let Ok(dbus) = DBusProxy::new(self.conn).await
            && let Ok(names) = dbus.list_names().await
        {
            for name in names {
                let name = name.to_string();
                if !ITEM_IFACES.iter().any(|i| name.starts_with(&format!("{i}-"))) {
                    continue;
                }
                // already tracked under another spelling of its name (`:1.42` instead of the
                // well-known name)?
                let owner = match zbus::names::BusName::try_from(name.as_str()) {
                    Ok(n) => dbus.get_name_owner(n).await.ok().map(|o| o.to_string()),
                    Err(_) => None,
                };
                let known = owner.is_some_and(|o| self.entries.iter().any(|e| e.owner.as_deref() == Some(o.as_str()) && e.path == DEFAULT_ITEM_PATH));
                if !known {
                    let _ = self.tx.send(Ev::Registered { key: format!("{name}{DEFAULT_ITEM_PATH}"), announce: true }).await;
                }
            }
        }
    }

    /// Be a host of the watcher `owner` serves.
    fn attach_remote(&mut self, owner: String) {
        if self.remote_owner.as_deref() == Some(owner.as_str()) {
            return;
        }
        self.remote_owner = Some(owner.clone());
        let (conn, tx, host) = (self.conn.clone(), self.tx.clone(), self.host.clone());
        self.remote_task = Some(self.ex.spawn(async move {
            if let Err(e) = remote_watch(&conn, &owner, &host, &tx).await {
                tracing::debug!("tray: remote watcher {owner}: {e}");
            }
        }));
    }

    async fn remove(&mut self, key: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.key != key);
        let removed = self.entries.len() != before;
        if removed && self.is_watcher {
            watcher::announce_unregistered(self.conn, &self.wdata, key).await;
        }
        if removed && self.menu_item.as_deref() == Some(key) {
            self.drop_menu();
        }
        removed
    }

    fn drop_menu(&mut self) -> bool {
        self.menu_seq += 1;
        self.menu_task = None;
        self.menu_item = None;
        self.menu.take().is_some()
    }

    /// Returns whether the published state may have changed.
    async fn handle(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Cmd(cmd) => self.exec(cmd),
            Ev::Name { name, new } => {
                let mut dirty = false;
                if name == WATCHER_NAME {
                    let ours = self.conn.unique_name().map(|n| n.to_string());
                    match new.clone() {
                        Some(n) if Some(&n) == ours.as_ref() => {
                            if !self.is_watcher {
                                self.become_watcher().await;
                                dirty = true;
                            }
                        }
                        Some(n) => {
                            if !self.is_watcher {
                                self.attach_remote(n);
                            }
                        }
                        None => {
                            if !self.is_watcher {
                                self.remote_owner = None;
                                self.remote_task = None;
                            }
                        }
                    }
                }
                if new.is_none() {
                    let gone: Vec<String> = self
                        .entries
                        .iter()
                        .filter(|e| e.bus == name || e.owner.as_deref() == Some(name.as_str()))
                        .map(|e| e.key.clone())
                        .collect();
                    for key in gone {
                        dirty |= self.remove(&key).await;
                    }
                }
                dirty
            }
            Ev::Registered { key, announce } => {
                let (bus, path) = parse_service(&key);
                if bus.is_empty() || self.entry(&key).is_some() {
                    return false;
                }
                if announce && self.is_watcher {
                    watcher::announce_registered(self.conn, &self.wdata, &key).await;
                }
                let task = self.ex.spawn(watch_item(ItemWatch {
                    conn: self.conn.clone(),
                    key: key.clone(),
                    bus: bus.clone(),
                    path: path.clone(),
                    tx: self.tx.clone(),
                    resolver: self.resolver.clone(),
                }));
                self.entries.push(Entry { key, bus, path, owner: None, iface: ITEM_IFACES[0], item: None, _task: task });
                false
            }
            Ev::Unregistered { key } => self.remove(&key).await,
            Ev::ItemFailed { key } => self.remove(&key).await,
            Ev::Item { key, owner, iface, item } => {
                // The same app registered twice under different spellings of its name.
                let twin = self.entries.iter().any(|e| e.key != key && e.owner.as_deref() == Some(owner.as_str()) && e.item.is_some() && e.path == item.object_path);
                if twin {
                    return self.remove(&key).await;
                }
                let Some(e) = self.entries.iter_mut().find(|e| e.key == key) else { return false };
                let changed = e.item.as_ref() != Some(&*item);
                e.owner = Some(owner);
                e.iface = iface;
                e.item = Some(*item);
                changed
            }
            Ev::Menu { seq, item, root } => {
                if seq != self.menu_seq {
                    return false;
                }
                let new = OpenMenu { seq, item, root: *root };
                if self.menu.as_ref() == Some(&new) {
                    return false;
                }
                self.menu = Some(new);
                true
            }
            Ev::MenuFailed { seq, item } => {
                if seq == self.menu_seq {
                    tracing::debug!("tray: no dbusmenu for {item}: falling back to ContextMenu");
                    self.item_call(&item, "ContextMenu", ItemBody::Xy(0, 0), false);
                }
                false
            }
            Ev::CmdsClosed | Ev::Gone => false,
        }
    }

    // ── commands ──────────────────────────────────────────────────────────────

    fn exec(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Activate { id, x, y } => {
                // An app without `Activate` (menu-only items) gets its menu instead.
                let menu = self.entry(&id).and_then(|e| e.item.as_ref()).is_some_and(|i| i.has_menu());
                self.item_call(&id, "Activate", ItemBody::Xy(x, y), menu);
                false
            }
            Command::SecondaryActivate { id, x, y } => {
                self.item_call(&id, "SecondaryActivate", ItemBody::Xy(x, y), false);
                false
            }
            Command::ContextMenu { id, x, y } => {
                self.item_call(&id, "ContextMenu", ItemBody::Xy(x, y), false);
                false
            }
            Command::Scroll { id, delta, orientation } => {
                self.item_call(&id, "Scroll", ItemBody::Scroll(delta, orientation.as_str()), false);
                false
            }
            Command::OpenMenu { id } => self.open_menu(&id),
            Command::CloseMenu => {
                let target = self.menu.as_ref().and_then(|m| self.menu_target(&m.item));
                if let Some((owner, path)) = target {
                    let conn = self.conn.clone();
                    self.ex
                        .spawn(async move {
                            let _ = menu::send_event(&conn, &owner, &path, 0, "closed").await;
                        })
                        .detach();
                }
                self.drop_menu()
            }
            Command::MenuClick { item, menu_id } => {
                if let Some((owner, path)) = self.menu_target(&item) {
                    let conn = self.conn.clone();
                    self.ex
                        .spawn(async move {
                            if let Err(e) = menu::send_event(&conn, &owner, &path, menu_id, "clicked").await {
                                tracing::debug!("tray: menu click {menu_id}: {e}");
                            }
                        })
                        .detach();
                }
                false
            }
            Command::MenuAboutToShow { item, menu_id } => {
                if let Some((owner, path)) = self.menu_target(&item) {
                    let (conn, tx, seq) = (self.conn.clone(), self.tx.clone(), self.menu_seq);
                    let (resolver, theme_path) = (self.resolver.clone(), self.entry(&item).and_then(|e| e.item.as_ref()).map(|i| i.icon_theme_path.clone()).unwrap_or_default());
                    self.ex
                        .spawn(async move {
                            menu::prepare(&conn, &owner, &path, menu_id).await;
                            if let Ok(root) = menu::reload(&conn, &owner, &path, &resolver, &theme_path).await {
                                let _ = tx.send(Ev::Menu { seq, item, root: Box::new(root) }).await;
                            }
                        })
                        .detach();
                }
                false
            }
        }
    }

    /// `(owner, dbusmenu path)` of an item.
    fn menu_target(&self, id: &str) -> Option<(String, String)> {
        let e = self.entry(id)?;
        Some((e.owner.clone()?, e.item.as_ref()?.menu_path.clone()?))
    }

    fn open_menu(&mut self, id: &str) -> bool {
        let dirty = self.drop_menu();
        let seq = self.menu_seq;
        let Some((owner, path)) = self.menu_target(id) else {
            // No dbusmenu: the app draws its own menu.
            self.item_call(id, "ContextMenu", ItemBody::Xy(0, 0), false);
            return dirty;
        };
        self.menu_item = Some(id.to_string());
        let theme_path = self.entry(id).and_then(|e| e.item.as_ref()).map(|i| i.icon_theme_path.clone()).unwrap_or_default();
        self.menu_task = Some(self.ex.spawn(watch_menu(MenuWatch {
            conn: self.conn.clone(),
            tx: self.tx.clone(),
            resolver: self.resolver.clone(),
            seq,
            item: id.to_string(),
            owner,
            path,
            theme_path,
        })));
        dirty
    }

    fn item_call(&self, id: &str, method: &'static str, body: ItemBody, menu_fallback: bool) {
        let Some(e) = self.entry(id) else { return };
        let Some(owner) = e.owner.clone() else { return };
        let (conn, path, iface, tx, id) = (self.conn.clone(), e.path.clone(), e.iface, self.tx.clone(), id.to_string());
        self.ex
            .spawn(async move {
                let call = async {
                    match &body {
                        ItemBody::Xy(x, y) => conn.call_method(Some(owner.as_str()), path.as_str(), Some(iface), method, &(*x, *y)).await,
                        ItemBody::Scroll(d, o) => conn.call_method(Some(owner.as_str()), path.as_str(), Some(iface), method, &(*d, *o)).await,
                    }
                };
                if let Err(e) = timeout(CALL_TIMEOUT, call).await {
                    tracing::debug!("tray: {method} on {id}: {e}");
                    if menu_fallback {
                        let _ = tx.send(Ev::Cmd(Command::OpenMenu { id })).await;
                    }
                }
            })
            .detach();
    }
}

enum ItemBody {
    Xy(i32, i32),
    Scroll(i32, &'static str),
}

// ── remote watcher (host mode) ────────────────────────────────────────────────

async fn remote_watch(conn: &Connection, owner: &str, host: &str, tx: &Sender<Ev>) -> zbus::Result<()> {
    // Subscribe first, read afterwards: nothing can change unnoticed in between.
    let rule = MatchRule::builder().msg_type(MessageType::Signal).sender(owner)?.path(WATCHER_PATH)?.interface(WATCHER_IFACE)?.build();
    let mut sigs = MessageStream::for_match_rule(rule, conn, Some(64)).await?;
    if let Err(e) =
        timeout(CALL_TIMEOUT, conn.call_method(Some(owner), WATCHER_PATH, Some(WATCHER_IFACE), "RegisterStatusNotifierHost", &(host,))).await
    {
        tracing::debug!("tray: RegisterStatusNotifierHost: {e}");
    }
    let reply = timeout(
        CALL_TIMEOUT,
        conn.call_method(Some(owner), WATCHER_PATH, Some(PROPS_IFACE), "Get", &(WATCHER_IFACE, "RegisteredStatusNotifierItems")),
    )
    .await?;
    let value: OwnedValue = reply.body().deserialize()?;
    let items: Vec<String> = value.try_into().unwrap_or_default();
    for service in items {
        let _ = tx.send(Ev::Registered { key: canonical(&service), announce: false }).await;
    }
    while let Some(msg) = sigs.next().await {
        let Ok(msg) = msg else { continue };
        let header = msg.header();
        let Some(member) = header.member().map(|m| m.as_str().to_string()) else { continue };
        let Ok(service) = msg.body().deserialize::<String>() else { continue };
        match member.as_str() {
            "StatusNotifierItemRegistered" => {
                let _ = tx.send(Ev::Registered { key: canonical(&service), announce: false }).await;
            }
            "StatusNotifierItemUnregistered" => {
                let _ = tx.send(Ev::Unregistered { key: canonical(&service) }).await;
            }
            _ => {}
        }
    }
    Ok(())
}

fn canonical(service: &str) -> String {
    let (bus, path) = parse_service(service);
    format!("{bus}{path}")
}

// ── one item ──────────────────────────────────────────────────────────────────

struct ItemWatch {
    conn: Connection,
    key: String,
    bus: String,
    path: String,
    tx: Sender<Ev>,
    resolver: Resolver,
}

async fn watch_item(w: ItemWatch) {
    if let Err(e) = watch_item_inner(&w).await {
        tracing::debug!("tray: {}: {e}", w.key);
        let _ = w.tx.send(Ev::ItemFailed { key: w.key.clone() }).await;
    }
}

async fn resolve_owner(conn: &Connection, bus: &str) -> zbus::Result<String> {
    if bus.starts_with(':') {
        return Ok(bus.to_string());
    }
    let dbus = DBusProxy::new(conn).await?;
    let name = zbus::names::BusName::try_from(bus)?;
    Ok(timeout(CALL_TIMEOUT, async { Ok(dbus.get_name_owner(name).await?) }).await?.to_string())
}

/// `GetAll` of the item interface (`prefer` first, then the other spelling).
async fn read_item(conn: &Connection, owner: &str, path: &str, prefer: &'static str) -> zbus::Result<(&'static str, RawItem)> {
    let order = if prefer == ITEM_IFACES[0] { ITEM_IFACES } else { [ITEM_IFACES[1], ITEM_IFACES[0]] };
    let mut last = zbus::Error::Failure("no StatusNotifierItem interface".into());
    for iface in order {
        match timeout(CALL_TIMEOUT, conn.call_method(Some(owner), path, Some(PROPS_IFACE), "GetAll", &(iface,))).await {
            Ok(reply) => {
                let props: std::collections::HashMap<String, OwnedValue> = reply.body().deserialize()?;
                if !props.is_empty() {
                    return Ok((iface, parse_item(&props)));
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn publish_item(w: &ItemWatch, owner: &str, iface: &'static str, raw: RawItem) {
    let (key, bus, path, resolver) = (w.key.clone(), w.bus.clone(), w.path.clone(), w.resolver.clone());
    let item = smol::unblock(move || build_item(&key, &bus, &path, &raw, &resolver)).await;
    let _ = w.tx.send(Ev::Item { key: w.key.clone(), owner: owner.to_string(), iface, item: Box::new(item) }).await;
}

async fn watch_item_inner(w: &ItemWatch) -> zbus::Result<()> {
    let owner = resolve_owner(&w.conn, &w.bus).await?;
    // Subscribe first, read afterwards: nothing can change unnoticed in between.
    let rule = MatchRule::builder().msg_type(MessageType::Signal).sender(owner.as_str())?.path(w.path.as_str())?.build();
    let mut sigs = MessageStream::for_match_rule(rule, &w.conn, Some(64)).await?;

    // First full read; apps that registered before exporting get a few more chances.
    let mut tries = 0;
    let (mut iface, raw) = loop {
        match read_item(&w.conn, &owner, &w.path, ITEM_IFACES[0]).await {
            Ok(r) => break r,
            Err(e) if tries + 1 < START_TRIES => {
                tracing::trace!("tray: {} not ready: {e}", w.key);
                tries += 1;
                smol::Timer::after(START_RETRY).await;
            }
            Err(e) => return Err(e),
        }
    };
    publish_item(w, &owner, iface, raw).await;

    loop {
        let Some(_signal) = sigs.next().await else { return Ok(()) };
        // One refresh per burst.
        smol::Timer::after(REFRESH_INTERVAL).await;
        while poll_once(sigs.next()).await.flatten().is_some() {}
        match read_item(&w.conn, &owner, &w.path, iface).await {
            Ok((i, raw)) => {
                iface = i;
                publish_item(w, &owner, iface, raw).await;
            }
            Err(e) => tracing::debug!("tray: refresh {}: {e}", w.key),
        }
    }
}

// ── the open menu ─────────────────────────────────────────────────────────────

struct MenuWatch {
    conn: Connection,
    tx: Sender<Ev>,
    resolver: Resolver,
    seq: u64,
    item: String,
    owner: String,
    path: String,
    theme_path: String,
}

async fn watch_menu(w: MenuWatch) {
    // Subscribe first: a change during the fetch triggers one more fetch afterwards.
    let sigs = async {
        let rule = MatchRule::builder().msg_type(MessageType::Signal).sender(w.owner.as_str())?.path(w.path.as_str())?.interface(menu::IFACE)?.build();
        MessageStream::for_match_rule(rule, &w.conn, Some(32)).await
    }
    .await;
    let mut last = match menu::load(&w.conn, &w.owner, &w.path, &w.resolver, &w.theme_path).await {
        Ok(root) => {
            let _ = w.tx.send(Ev::Menu { seq: w.seq, item: w.item.clone(), root: Box::new(root.clone()) }).await;
            root
        }
        Err(e) => {
            tracing::debug!("tray: menu of {}: {e}", w.item);
            let _ = w.tx.send(Ev::MenuFailed { seq: w.seq, item: w.item.clone() }).await;
            return;
        }
    };
    let Ok(mut sigs) = sigs else { return };
    while let Some(msg) = sigs.next().await {
        let Ok(msg) = msg else { continue };
        if !matches!(msg.header().member().map(|m| m.as_str()), Some("LayoutUpdated" | "ItemsPropertiesUpdated")) {
            continue;
        }
        smol::Timer::after(REFRESH_INTERVAL).await;
        while poll_once(sigs.next()).await.flatten().is_some() {}
        match menu::reload(&w.conn, &w.owner, &w.path, &w.resolver, &w.theme_path).await {
            Ok(root) if root != last => {
                last = root.clone();
                let _ = w.tx.send(Ev::Menu { seq: w.seq, item: w.item.clone(), root: Box::new(root) }).await;
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("tray: menu refresh of {}: {e}", w.item),
        }
    }
}

// ── pixmap cache housekeeping ─────────────────────────────────────────────────

fn referenced(state: &TrayState) -> HashSet<PathBuf> {
    fn node(n: &MenuNode, out: &mut HashSet<PathBuf>) {
        out.extend(n.icon.clone());
        n.children.iter().for_each(|c| node(c, out));
    }
    let mut keep = HashSet::new();
    for i in &state.items {
        keep.extend([i.icon.clone(), i.attention_icon.clone(), i.overlay_icon.clone(), i.tooltip.icon.clone()].into_iter().flatten());
    }
    if let Some(m) = &state.menu {
        node(&m.root, &mut keep);
    }
    keep
}

/// Animated icons produce a new pixmap file per frame; once there are many, delete the
/// ones no state refers to (and that are not brand new).
fn prune_cache(dir: &Path, state: &TrayState) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let files: Vec<_> = read.filter_map(Result::ok).collect();
    if files.len() <= CACHE_PRUNE_ABOVE {
        return;
    }
    let keep = referenced(state);
    for f in files {
        let path = f.path();
        let fresh = f.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age < Duration::from_secs(60));
        if !keep.contains(&path) && !fresh {
            let _ = std::fs::remove_file(path);
        }
    }
}
