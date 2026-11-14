//! A mock tray application served over a zbus connection to a *private* bus: a
//! StatusNotifierItem (`org.kde.StatusNotifierItem`, or the `org.freedesktop` spelling)
//! plus a `com.canonical.dbusmenu`. Shared by `tests/bus.rs` and `examples/mock_tray.rs`
//! (`#[path]` include), so the bar can be driven in the nested session by exactly what the
//! tests exercise.
//!
//! The mock behaves like an app: every method call is appended to a log (`MockItem::log`),
//! clicking a checkable menu entry toggles it and emits `ItemsPropertiesUpdated` /
//! `LayoutUpdated`, and the `set_*` helpers change a property and emit the matching
//! `New*` signal.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use serde::Serialize;
use zbus::{
    Connection, fdo, interface,
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value},
};

pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub const ITEM_PATH: &str = "/StatusNotifierItem";
pub const MENU_PATH: &str = "/MenuBar";

pub type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// A solid `edge × edge` ARGB32 pixmap (network byte order: A R G B).
pub fn solid(edge: i32, argb: [u8; 4]) -> (i32, i32, Vec<u8>) {
    (edge, edge, argb.repeat((edge * edge) as usize))
}

// ── menu model ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct MItem {
    pub id: i32,
    pub label: String,
    pub separator: bool,
    pub enabled: bool,
    pub visible: bool,
    /// `("checkmark" | "radio", state)`
    pub toggle: Option<(&'static str, i32)>,
    pub icon_name: String,
    pub children: Vec<MItem>,
    /// A submenu whose children are only delivered after `AboutToShow(id)`.
    pub lazy: bool,
    pub shortcut: Option<Vec<String>>,
}

impl MItem {
    pub fn new(id: i32, label: &str) -> Self {
        Self { id, label: label.into(), separator: false, enabled: true, visible: true, toggle: None, icon_name: String::new(), children: vec![], lazy: false, shortcut: None }
    }
    pub fn separator(id: i32) -> Self {
        Self { separator: true, ..Self::new(id, "") }
    }
    pub fn check(mut self, on: bool) -> Self {
        self.toggle = Some(("checkmark", on as i32));
        self
    }
    pub fn radio(mut self, on: bool) -> Self {
        self.toggle = Some(("radio", on as i32));
        self
    }
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }
    pub fn sub(mut self, children: Vec<MItem>) -> Self {
        self.children = children;
        self
    }
    pub fn lazy(mut self, children: Vec<MItem>) -> Self {
        self.children = children;
        self.lazy = true;
        self
    }
    pub fn icon(mut self, name: &str) -> Self {
        self.icon_name = name.into();
        self
    }
    pub fn shortcut(mut self, keys: &[&str]) -> Self {
        self.shortcut = Some(keys.iter().map(|k| k.to_string()).collect());
        self
    }

    fn find(&self, id: i32) -> Option<&MItem> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    fn find_mut(&mut self, id: i32) -> Option<&mut MItem> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }

    fn props(&self) -> HashMap<String, OwnedValue> {
        let mut m: HashMap<String, OwnedValue> = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            m.insert(k.to_string(), v.try_to_owned().unwrap());
        };
        if self.separator {
            put("type", Value::from("separator".to_string()));
        } else {
            put("label", Value::from(self.label.clone()));
        }
        if !self.enabled {
            put("enabled", Value::from(false));
        }
        if !self.visible {
            put("visible", Value::from(false));
        }
        if let Some((kind, state)) = self.toggle {
            put("toggle-type", Value::from(kind.to_string()));
            put("toggle-state", Value::from(state));
        }
        if !self.icon_name.is_empty() {
            put("icon-name", Value::from(self.icon_name.clone()));
        }
        if !self.children.is_empty() || self.lazy {
            put("children-display", Value::from("submenu".to_string()));
        }
        if let Some(keys) = &self.shortcut {
            put("shortcut", Value::from(vec![keys.clone()]));
        }
        m
    }
}

/// `(ia{sv}av)` — one dbusmenu layout node.
#[derive(Serialize, Type, zbus::zvariant::Value, zbus::zvariant::OwnedValue)]
pub struct Node {
    id: i32,
    props: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

fn node(item: &MItem, depth: i32, shown: &[i32]) -> Node {
    let expose = depth != 0 && (!item.lazy || shown.contains(&item.id));
    let children = if expose {
        item.children.iter().map(|c| OwnedValue::try_from(node(c, depth - 1, shown)).unwrap()).collect()
    } else {
        vec![]
    };
    Node { id: item.id, props: item.props(), children }
}

// ── shared state ──────────────────────────────────────────────────────────────

pub struct ItemState {
    pub id: String,
    pub title: String,
    pub category: String,
    pub status: String,
    pub icon_name: String,
    pub icon_theme_path: String,
    pub icon_pixmap: Pixmaps,
    pub attention_icon_name: String,
    pub attention_pixmap: Pixmaps,
    pub tooltip_title: String,
    pub tooltip_text: String,
    pub item_is_menu: bool,
    pub menu: Option<String>,
    pub root: MItem,
    pub revision: u32,
    /// Lazy submenus whose `AboutToShow` came.
    pub shown: Vec<i32>,
    /// `Activate(1,2)`, `Scroll(5,vertical)`, `Event(3,clicked)` … in order.
    pub log: Vec<String>,
    /// `Activate` answers with an error (an app that only has a menu).
    pub activate_fails: bool,
    /// `GetLayout` never answers (a hung app).
    pub hang_menu: bool,
}

pub type Shared = Arc<Mutex<ItemState>>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, ItemState> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

macro_rules! item_interface {
    ($name:ident, $iface:literal) => {
        pub struct $name(pub Shared);

        #[interface(name = $iface)]
        impl $name {
            fn activate(&self, x: i32, y: i32) -> fdo::Result<()> {
                let mut s = lock(&self.0);
                s.log.push(format!("Activate({x},{y})"));
                if s.activate_fails { Err(fdo::Error::UnknownMethod("no Activate".into())) } else { Ok(()) }
            }
            fn secondary_activate(&self, x: i32, y: i32) {
                lock(&self.0).log.push(format!("SecondaryActivate({x},{y})"));
            }
            fn context_menu(&self, x: i32, y: i32) {
                lock(&self.0).log.push(format!("ContextMenu({x},{y})"));
            }
            fn scroll(&self, delta: i32, orientation: String) {
                lock(&self.0).log.push(format!("Scroll({delta},{orientation})"));
            }
            #[zbus(property)]
            fn category(&self) -> String {
                lock(&self.0).category.clone()
            }
            #[zbus(property)]
            fn id(&self) -> String {
                lock(&self.0).id.clone()
            }
            #[zbus(property)]
            fn title(&self) -> String {
                lock(&self.0).title.clone()
            }
            #[zbus(property)]
            fn status(&self) -> String {
                lock(&self.0).status.clone()
            }
            #[zbus(property)]
            fn window_id(&self) -> u32 {
                0
            }
            #[zbus(property)]
            fn icon_theme_path(&self) -> String {
                lock(&self.0).icon_theme_path.clone()
            }
            #[zbus(property)]
            fn menu(&self) -> OwnedObjectPath {
                let p = lock(&self.0).menu.clone().unwrap_or_else(|| "/NO_DBUSMENU".into());
                OwnedObjectPath::try_from(p).unwrap()
            }
            #[zbus(property)]
            fn item_is_menu(&self) -> bool {
                lock(&self.0).item_is_menu
            }
            #[zbus(property)]
            fn icon_name(&self) -> String {
                lock(&self.0).icon_name.clone()
            }
            #[zbus(property)]
            fn icon_pixmap(&self) -> Pixmaps {
                lock(&self.0).icon_pixmap.clone()
            }
            #[zbus(property)]
            fn overlay_icon_name(&self) -> String {
                String::new()
            }
            #[zbus(property)]
            fn overlay_icon_pixmap(&self) -> Pixmaps {
                Vec::new()
            }
            #[zbus(property)]
            fn attention_icon_name(&self) -> String {
                lock(&self.0).attention_icon_name.clone()
            }
            #[zbus(property)]
            fn attention_icon_pixmap(&self) -> Pixmaps {
                lock(&self.0).attention_pixmap.clone()
            }
            #[zbus(property)]
            fn tool_tip(&self) -> (String, Pixmaps, String, String) {
                let s = lock(&self.0);
                (String::new(), Vec::new(), s.tooltip_title.clone(), s.tooltip_text.clone())
            }
            #[zbus(signal)]
            async fn new_title(em: &SignalEmitter<'_>) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn new_icon(em: &SignalEmitter<'_>) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn new_attention_icon(em: &SignalEmitter<'_>) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn new_overlay_icon(em: &SignalEmitter<'_>) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn new_tool_tip(em: &SignalEmitter<'_>) -> zbus::Result<()>;
            #[zbus(signal)]
            async fn new_status(em: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
        }
    };
}

item_interface!(KdeItem, "org.kde.StatusNotifierItem");
item_interface!(FdoItem, "org.freedesktop.StatusNotifierItem");

pub struct DbusMenu(pub Shared);

#[interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    async fn get_layout(&self, parent_id: i32, recursion_depth: i32, _property_names: Vec<String>) -> fdo::Result<(u32, Node)> {
        let hang = lock(&self.0).hang_menu;
        if hang {
            smol::Timer::after(std::time::Duration::from_secs(60)).await;
        }
        let mut s = lock(&self.0);
        s.log.push(format!("GetLayout({parent_id})"));
        let root = s.root.find(parent_id).ok_or_else(|| fdo::Error::InvalidArgs(format!("no item {parent_id}")))?;
        Ok((s.revision, node(root, recursion_depth, &s.shown)))
    }
    fn about_to_show(&self, id: i32) -> bool {
        let mut s = lock(&self.0);
        s.log.push(format!("AboutToShow({id})"));
        if !s.shown.contains(&id) {
            s.shown.push(id);
        }
        false
    }
    async fn event(
        &self,
        id: i32,
        event_id: String,
        _data: OwnedValue,
        _timestamp: u32,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let revision = {
            let mut s = lock(&self.0);
            s.log.push(format!("Event({id},{event_id})"));
            if event_id != "clicked" {
                return Ok(());
            }
            let mut updated = None;
            if let Some(item) = s.root.find_mut(id)
                && let Some((kind, state)) = item.toggle
            {
                item.toggle = Some((kind, if state == 1 { 0 } else { 1 }));
                updated = Some((id, item.props()));
            }
            if updated.is_none() {
                return Ok(());
            }
            s.revision += 1;
            (s.revision, updated)
        };
        let (revision, updated) = revision;
        Self::items_properties_updated(&em, updated.into_iter().collect(), vec![]).await?;
        Self::layout_updated(&em, revision, 0).await?;
        Ok(())
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".into()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        "normal".into()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(signal)]
    async fn layout_updated(em: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn items_properties_updated(
        em: &SignalEmitter<'_>,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

// ── the mock app ──────────────────────────────────────────────────────────────

/// How the item is registered with the watcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// `RegisterStatusNotifierItem("org.kde.StatusNotifierItem-<pid>-<n>")`
    BusName,
    /// `RegisterStatusNotifierItem("/StatusNotifierItem")` (the sender is the bus name)
    Path,
}

pub struct MockItem {
    pub conn: Connection,
    pub shared: Shared,
    pub bus_name: String,
    pub path: String,
}

pub struct ItemBuilder {
    state: ItemState,
    path: String,
    fdo: bool,
}

impl ItemBuilder {
    pub fn new(id: &str) -> Self {
        let state = ItemState {
            id: id.into(),
            title: String::new(),
            category: "ApplicationStatus".into(),
            status: "Active".into(),
            icon_name: String::new(),
            icon_theme_path: String::new(),
            icon_pixmap: vec![],
            attention_icon_name: String::new(),
            attention_pixmap: vec![],
            tooltip_title: String::new(),
            tooltip_text: String::new(),
            item_is_menu: false,
            menu: None,
            root: MItem::new(0, "").sub(vec![]),
            revision: 1,
            shown: vec![],
            log: vec![],
            activate_fails: false,
            hang_menu: false,
        };
        Self { state, path: ITEM_PATH.into(), fdo: false }
    }
    pub fn title(mut self, t: &str) -> Self {
        self.state.title = t.into();
        self
    }
    pub fn category(mut self, c: &str) -> Self {
        self.state.category = c.into();
        self
    }
    pub fn status(mut self, s: &str) -> Self {
        self.state.status = s.into();
        self
    }
    pub fn icon_name(mut self, n: &str) -> Self {
        self.state.icon_name = n.into();
        self
    }
    pub fn icon_theme_path(mut self, p: &str) -> Self {
        self.state.icon_theme_path = p.into();
        self
    }
    pub fn pixmaps(mut self, p: Pixmaps) -> Self {
        self.state.icon_pixmap = p;
        self
    }
    pub fn attention_icon_name(mut self, n: &str) -> Self {
        self.state.attention_icon_name = n.into();
        self
    }
    pub fn tooltip(mut self, title: &str, text: &str) -> Self {
        self.state.tooltip_title = title.into();
        self.state.tooltip_text = text.into();
        self
    }
    pub fn item_is_menu(mut self, on: bool) -> Self {
        self.state.item_is_menu = on;
        self
    }
    pub fn hang_menu(mut self) -> Self {
        self.state.hang_menu = true;
        self
    }
    pub fn activate_fails(mut self) -> Self {
        self.state.activate_fails = true;
        self
    }
    pub fn menu(mut self, items: Vec<MItem>) -> Self {
        self.state.menu = Some(MENU_PATH.into());
        self.state.root = MItem::new(0, "").sub(items);
        self
    }
    /// A custom object path (apps like Electron/appindicator use their own).
    pub fn path(mut self, p: &str) -> Self {
        self.path = p.into();
        self
    }
    /// Serve `org.freedesktop.StatusNotifierItem` instead of the KDE spelling.
    pub fn fdo(mut self) -> Self {
        self.fdo = true;
        self
    }

    pub async fn serve(self, address: &str) -> zbus::Result<MockItem> {
        static N: AtomicU32 = AtomicU32::new(1);
        let bus_name = format!("org.kde.StatusNotifierItem-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let has_menu = self.state.menu.is_some();
        let shared: Shared = Arc::new(Mutex::new(self.state));
        let mut b = zbus::connection::Builder::address(address)?;
        b = if self.fdo { b.serve_at(self.path.as_str(), FdoItem(shared.clone()))? } else { b.serve_at(self.path.as_str(), KdeItem(shared.clone()))? };
        if has_menu {
            b = b.serve_at(MENU_PATH, DbusMenu(shared.clone()))?;
        }
        let conn = b.name(bus_name.as_str())?.build().await?;
        Ok(MockItem { conn, shared, bus_name, path: self.path })
    }
}

impl MockItem {
    /// Call `RegisterStatusNotifierItem` on whoever owns the watcher name.
    pub async fn register(&self, form: Form) -> zbus::Result<()> {
        let arg = match form {
            Form::BusName => self.bus_name.clone(),
            Form::Path => self.path.clone(),
        };
        self.conn
            .call_method(Some(WATCHER_NAME), WATCHER_PATH, Some("org.kde.StatusNotifierWatcher"), "RegisterStatusNotifierItem", &(arg,))
            .await
            .map(|_| ())
    }

    pub fn log(&self) -> Vec<String> {
        lock(&self.shared).log.clone()
    }

    pub fn clear_log(&self) {
        lock(&self.shared).log.clear();
    }

    async fn emit(&self, member: &'static str, arg: Option<&str>) -> zbus::Result<()> {
        let iface = if self.is_fdo().await { "org.freedesktop.StatusNotifierItem" } else { "org.kde.StatusNotifierItem" };
        match arg {
            Some(a) => self.conn.emit_signal(None::<&str>, self.path.as_str(), iface, member, &(a,)).await,
            None => self.conn.emit_signal(None::<&str>, self.path.as_str(), iface, member, &()).await,
        }
    }

    async fn is_fdo(&self) -> bool {
        self.conn.object_server().interface::<_, FdoItem>(self.path.as_str()).await.is_ok()
    }

    pub async fn set_status(&self, status: &str) -> zbus::Result<()> {
        lock(&self.shared).status = status.into();
        self.emit("NewStatus", Some(status)).await
    }
    pub async fn set_title(&self, title: &str) -> zbus::Result<()> {
        lock(&self.shared).title = title.into();
        self.emit("NewTitle", None).await
    }
    pub async fn set_tooltip(&self, title: &str, text: &str) -> zbus::Result<()> {
        {
            let mut s = lock(&self.shared);
            s.tooltip_title = title.into();
            s.tooltip_text = text.into();
        }
        self.emit("NewToolTip", None).await
    }
    pub async fn set_icon_name(&self, name: &str) -> zbus::Result<()> {
        lock(&self.shared).icon_name = name.into();
        self.emit("NewIcon", None).await
    }
    pub async fn set_pixmaps(&self, p: Pixmaps) -> zbus::Result<()> {
        lock(&self.shared).icon_pixmap = p;
        self.emit("NewIcon", None).await
    }
    pub async fn set_attention_icon(&self, name: &str) -> zbus::Result<()> {
        lock(&self.shared).attention_icon_name = name.into();
        self.emit("NewAttentionIcon", None).await
    }

    /// The app rebuilds its menu: `LayoutUpdated`.
    pub async fn replace_menu(&self, items: Vec<MItem>) -> zbus::Result<()> {
        let revision = {
            let mut s = lock(&self.shared);
            s.root = MItem::new(0, "").sub(items);
            s.revision += 1;
            s.revision
        };
        self.conn.emit_signal(None::<&str>, MENU_PATH, "com.canonical.dbusmenu", "LayoutUpdated", &(revision, 0i32)).await
    }

    pub fn toggle_state(&self, id: i32) -> Option<i32> {
        lock(&self.shared).root.find_mut(id).and_then(|i| i.toggle.map(|t| t.1))
    }

    /// Let the app leave the bus.
    pub async fn quit(self) {
        drop(self.conn);
    }
}

pub fn object_path(p: &str) -> ObjectPath<'_> {
    ObjectPath::try_from(p).unwrap()
}

// ── a foreign watcher (another shell owning the name) ─────────────────────────

pub struct ForeignWatcher {
    pub items: Arc<Mutex<Vec<String>>>,
    pub hosts: Arc<Mutex<Vec<String>>>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl ForeignWatcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let key = if service.starts_with('/') { format!("{sender}{service}") } else { format!("{service}/StatusNotifierItem") };
        self.items.lock().unwrap().push(key.clone());
        Self::status_notifier_item_registered(&em, &key).await?;
        Ok(())
    }
    fn register_status_notifier_host(&self, service: String) {
        self.hosts.lock().unwrap().push(service);
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(signal)]
    async fn status_notifier_item_registered(em: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(em: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;
}

impl ForeignWatcher {
    /// Own `org.kde.StatusNotifierWatcher` like another shell would (no replacing allowed).
    pub async fn start(address: &str) -> zbus::Result<(Connection, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>)> {
        let items = Arc::new(Mutex::new(Vec::new()));
        let hosts = Arc::new(Mutex::new(Vec::new()));
        let conn = zbus::connection::Builder::address(address)?
            .serve_at(WATCHER_PATH, ForeignWatcher { items: items.clone(), hosts: hosts.clone() })?
            .name(WATCHER_NAME)?
            .build()
            .await?;
        Ok((conn, items, hosts))
    }
}
