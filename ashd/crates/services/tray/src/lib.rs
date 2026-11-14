//! System tray: `org.kde.StatusNotifierWatcher` + a StatusNotifierItem host + a DBusMenu
//! client (zbus 5, implemented directly on the protocols). No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<TrayState>)`,
//! newest-snapshot channel; commands are fire-and-forget and their effect comes back as a
//! new [`TrayState`]. Owner: tray.
//!
//! * **Watcher or host.** The service requests `org.kde.StatusNotifierWatcher` *without*
//!   `ReplaceExisting` and *queued*: when another process already owns the name it never
//!   steals it — it registers itself (`org.kde.StatusNotifierHost-<pid>`) with that
//!   watcher, consumes its item list and signals ([`TrayState::is_watcher`] is `false`)
//!   and, when that process goes away, the bus promotes this connection to the owner and
//!   the service takes over (apps re-register on their own when the name appears; items
//!   already on the bus are also discovered by their `org.kde.StatusNotifierItem-*` names).
//!   `RegisterStatusNotifierItem` accepts both a bus name and an object path.
//! * **Items.** Per item one task: `GetAll` on `org.kde.StatusNotifierItem` (or the
//!   `org.freedesktop.StatusNotifierItem` variant), refreshed (coalesced) on every `New*`
//!   signal and `PropertiesChanged`. Icon names are resolved to files off the service
//!   loop (`IconThemePath` first, then the icon theme of the session), `IconPixmap`
//!   (ARGB32, network byte order) is converted to RGBA, the size nearest to
//!   [`PIXMAP_TARGET`] is chosen and written as a PNG into [`cache_dir`] (hash-named, so
//!   UIs may cache by path). An item whose owner leaves the bus is dropped.
//! * **Menus.** [`Handle::open_menu`] fetches the item's `com.canonical.dbusmenu` layout
//!   lazily (`AboutToShow`, `GetLayout`, lazily-filled submenus grafted in) with timeouts
//!   and publishes it as [`TrayState::menu`] until [`Handle::close_menu`]; `LayoutUpdated`
//!   / `ItemsPropertiesUpdated` refresh it while open. [`Handle::menu_click`] sends the
//!   `clicked` event. An item without a usable dbusmenu falls back to the item's own
//!   `ContextMenu` method.
//!
//! Layout: `model` (types of the wire values → plain data, pure and unit-tested), `icons`
//! (name → file, pixmap → PNG), `watcher` (the watcher D-Bus object), `service` (the bus
//! loop). Testing: `tests/bus.rs` runs mock items + a mock dbusmenu on a private
//! `dbus-daemon` (never the user's bus); the mock (`tests/support/mock.rs`) is also the
//! `mock_tray` example that feeds the bar in the nested session.

mod icons;
mod menu;
mod model;
mod service;
mod watcher;

use std::{path::PathBuf, time::Duration};

use async_channel::{Receiver, Sender};

pub use icons::cache_dir;
pub use model::{Pixmap, parse_service, pixmap_to_rgba};

pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// Object path used when an item registers with a bus name only.
pub const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";

/// Preferred pixmap edge: the pixmap nearest to this (bigger wins ties) is used. The bar
/// draws 14–16 px icons, 32 keeps them sharp at 2× scale.
pub const PIXMAP_TARGET: i32 = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Idle: the UI may tuck the icon away.
    Passive,
    #[default]
    Active,
    /// Wants attention: the attention icon, always visible.
    NeedsAttention,
}

impl Status {
    /// SNI spelling; anything unknown is `Active`.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "passive" => Self::Passive,
            "needsattention" => Self::NeedsAttention,
            _ => Self::Active,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Category {
    #[default]
    ApplicationStatus,
    Communications,
    SystemServices,
    Hardware,
}

impl Category {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "communications" => Self::Communications,
            "systemservices" => Self::SystemServices,
            "hardware" => Self::Hardware,
            _ => Self::ApplicationStatus,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolTip {
    pub title: String,
    pub description: String,
    /// Resolved `IconName` or decoded pixmap of the tooltip.
    pub icon: Option<PathBuf>,
}

/// One tray icon. `id` is the key for every command (`"<bus name><object path>"`, e.g.
/// `":1.42/StatusNotifierItem"`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayItem {
    pub id: String,
    /// Bus name as registered (unique or well-known).
    pub bus_name: String,
    pub object_path: String,
    /// The SNI `Id` property (`"discord"`, `"nm-applet"`).
    pub app_id: String,
    pub title: String,
    pub category: Category,
    pub status: Status,
    /// `IconName` as sent (may be empty or a path).
    pub icon_name: String,
    pub icon_theme_path: String,
    /// The normal icon: `IconName` resolved to a file, else the decoded `IconPixmap`.
    pub icon: Option<PathBuf>,
    /// The icon shown while [`Status::NeedsAttention`].
    pub attention_icon: Option<PathBuf>,
    /// Small badge drawn over the icon (rarely used).
    pub overlay_icon: Option<PathBuf>,
    pub tooltip: ToolTip,
    /// Left click means "open the menu" (the app has no `Activate`).
    pub item_is_menu: bool,
    /// dbusmenu object path; `None` when the item has no menu.
    pub menu_path: Option<String>,
}

impl TrayItem {
    /// The icon to draw for the current status (attention icon while it needs attention
    /// and has one).
    pub fn display_icon(&self) -> Option<&PathBuf> {
        if self.status == Status::NeedsAttention
            && let Some(a) = &self.attention_icon
        {
            return Some(a);
        }
        self.icon.as_ref().or(self.attention_icon.as_ref())
    }

    /// Tooltip title, else title, else the app id: what a tooltip shows.
    pub fn label(&self) -> &str {
        [&self.tooltip.title, &self.title, &self.app_id]
            .into_iter()
            .map(|s| s.as_str())
            .find(|s| !s.is_empty())
            .unwrap_or("")
    }

    pub fn has_menu(&self) -> bool {
        self.menu_path.is_some()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuKind {
    #[default]
    Normal,
    Separator,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Toggle {
    #[default]
    None,
    Check(bool),
    Radio(bool),
}

/// One dbusmenu node. The root (id 0) carries the top-level entries in `children`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuNode {
    pub id: i32,
    /// Mnemonic underscores stripped (`_Quit` → `Quit`, `__` → `_`).
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub kind: MenuKind,
    pub toggle: Toggle,
    /// `icon-name` resolved to a file, or `icon-data` (PNG) stored in [`cache_dir`].
    pub icon: Option<PathBuf>,
    /// First `shortcut` rendered as text (`Ctrl+Q`).
    pub shortcut: Option<String>,
    /// `children-display = submenu` (children may still be empty for a lazy submenu the
    /// app never filled).
    pub submenu: bool,
    pub children: Vec<MenuNode>,
}

impl MenuNode {
    /// Opens a submenu when activated.
    pub fn is_submenu(&self) -> bool {
        self.submenu || !self.children.is_empty()
    }

    /// Visible children, in order.
    pub fn visible_children(&self) -> impl Iterator<Item = &MenuNode> {
        self.children.iter().filter(|c| c.visible)
    }

    /// Depth-first lookup by id.
    pub fn find(&self, id: i32) -> Option<&MenuNode> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }
}

/// The menu opened with [`Handle::open_menu`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenMenu {
    /// Increments with every `open_menu`; refreshes of the same menu keep it. UIs open
    /// their popup when they see a seq they have not shown yet.
    pub seq: u64,
    /// [`TrayItem::id`] of the item it belongs to.
    pub item: String,
    pub root: MenuNode,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayState {
    /// Connected to the session bus.
    pub available: bool,
    /// This process owns `org.kde.StatusNotifierWatcher` (else it is a host of someone
    /// else's watcher).
    pub is_watcher: bool,
    /// In registration order.
    pub items: Vec<TrayItem>,
    pub menu: Option<OpenMenu>,
}

impl TrayState {
    pub fn item(&self, id: &str) -> Option<&TrayItem> {
        self.items.iter().find(|i| i.id == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

impl Orientation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `Activate(x, y)` (screen coordinates; the item may use them to place a window).
    Activate { id: String, x: i32, y: i32 },
    SecondaryActivate { id: String, x: i32, y: i32 },
    /// The item's own `ContextMenu(x, y)` (apps that draw their menu themselves).
    ContextMenu { id: String, x: i32, y: i32 },
    Scroll { id: String, delta: i32, orientation: Orientation },
    /// Fetch the dbusmenu of `id` and publish it as [`TrayState::menu`].
    OpenMenu { id: String },
    /// Forget the open menu (sends the `closed` event).
    CloseMenu,
    /// dbusmenu `Event(menu_id, "clicked")` on the open menu of `item`.
    MenuClick { item: String, menu_id: i32 },
    /// dbusmenu `AboutToShow(menu_id)` + a layout refresh (lazy submenus).
    MenuAboutToShow { item: String, menu_id: i32 },
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        if self.tx.try_send(cmd).is_err() {
            tracing::debug!("tray service is not running");
        }
    }
    pub fn activate(&self, id: &str, x: i32, y: i32) {
        self.send(Command::Activate { id: id.into(), x, y });
    }
    pub fn secondary_activate(&self, id: &str, x: i32, y: i32) {
        self.send(Command::SecondaryActivate { id: id.into(), x, y });
    }
    pub fn context_menu(&self, id: &str, x: i32, y: i32) {
        self.send(Command::ContextMenu { id: id.into(), x, y });
    }
    pub fn scroll(&self, id: &str, delta: i32, orientation: Orientation) {
        self.send(Command::Scroll { id: id.into(), delta, orientation });
    }
    pub fn open_menu(&self, id: &str) {
        self.send(Command::OpenMenu { id: id.into() });
    }
    pub fn close_menu(&self) {
        self.send(Command::CloseMenu);
    }
    pub fn menu_click(&self, item: &str, menu_id: i32) {
        self.send(Command::MenuClick { item: item.into(), menu_id });
    }
    pub fn menu_about_to_show(&self, item: &str, menu_id: i32) {
        self.send(Command::MenuAboutToShow { item: item.into(), menu_id });
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Where and how the service runs (tests, the nested session).
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Bus address; `None` = the session bus (`$DBUS_SESSION_BUS_ADDRESS`).
    pub address: Option<String>,
    /// Where decoded pixmaps go; `None` = [`cache_dir`].
    pub cache_dir: Option<PathBuf>,
}

/// Spawn the `ashd-svc-tray` thread on the session bus.
pub fn start() -> (Handle, Receiver<TrayState>) {
    start_with(Config::default())
}

/// Like [`start`], on an explicit bus address.
pub fn start_at(address: Option<String>) -> (Handle, Receiver<TrayState>) {
    start_with(Config { address, cache_dir: None })
}

pub fn start_with(config: Config) -> (Handle, Receiver<TrayState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-tray".into())
        .spawn(move || smol::block_on(service::run(rx, state_tx, config)))
        .expect("spawn ashd-svc-tray");
    (Handle { tx }, state_rx)
}

pub(crate) const RETRY_MIN: Duration = Duration::from_millis(500);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(10);
