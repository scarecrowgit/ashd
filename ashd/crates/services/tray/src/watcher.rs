//! The `org.kde.StatusNotifierWatcher` object (served on every session; it only answers
//! callers while this connection owns the well-known name).

use std::sync::{Arc, Mutex, MutexGuard};

use async_channel::Sender;
use zbus::{Connection, fdo, interface, message::Header, names::BusName, object_server::SignalEmitter, zvariant::ObjectPath};

use crate::{WATCHER_PATH, model::parse_service, service::Ev};

/// What the watcher object answers with. Shared with the service loop, which removes
/// items whose owner left the bus.
#[derive(Debug, Default)]
pub(crate) struct WatcherData {
    /// `"<bus name><object path>"`, as hosts parse it (split at the first `/`).
    pub items: Vec<String>,
    pub hosts: Vec<String>,
}

pub(crate) type Shared = Arc<Mutex<WatcherData>>;

pub(crate) fn lock(s: &Shared) -> MutexGuard<'_, WatcherData> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct Watcher {
    pub data: Shared,
    pub events: Sender<Ev>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// `service` is a bus name (`:1.42`, `org.kde.StatusNotifierItem-4242-1`; the object
    /// is then `/StatusNotifierItem`) or an object path (the bus name is the sender), and
    /// some apps send `name/path`.
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let (mut bus, path) = parse_service(&service);
        if bus.is_empty() {
            bus = header.sender().map(|s| s.to_string()).ok_or_else(|| fdo::Error::InvalidArgs("no sender".into()))?;
        }
        if BusName::try_from(bus.as_str()).is_err() || ObjectPath::try_from(path.as_str()).is_err() {
            return Err(fdo::Error::InvalidArgs(format!("`{service}` is neither a bus name nor an object path")));
        }
        let key = format!("{bus}{path}");
        let is_new = {
            let mut d = lock(&self.data);
            if d.items.contains(&key) {
                false
            } else {
                d.items.push(key.clone());
                true
            }
        };
        if is_new {
            let _ = self.events.send(Ev::Registered { key: key.clone(), announce: false }).await;
            Self::status_notifier_item_registered(&emitter, &key).await?;
            self.registered_status_notifier_items_changed(&emitter).await?;
        }
        Ok(())
    }

    async fn register_status_notifier_host(&self, service: String, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> fdo::Result<()> {
        let first = {
            let mut d = lock(&self.data);
            if !d.hosts.contains(&service) {
                d.hosts.push(service);
            }
            d.hosts.len() == 1
        };
        if first {
            Self::status_notifier_host_registered(&emitter).await?;
        }
        Ok(())
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        lock(&self.data).items.clone()
    }

    /// Always true: this service is a host itself.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    pub async fn status_notifier_item_registered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn status_notifier_item_unregistered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Add `key` to the watcher's list and announce it (service-side discovery).
pub(crate) async fn announce_registered(conn: &Connection, data: &Shared, key: &str) {
    let added = {
        let mut d = lock(data);
        if d.items.iter().any(|k| k == key) {
            false
        } else {
            d.items.push(key.to_string());
            true
        }
    };
    if added {
        emit(conn, key, true).await;
    }
}

/// Remove `key` from the watcher's list and announce it.
pub(crate) async fn announce_unregistered(conn: &Connection, data: &Shared, key: &str) {
    let removed = {
        let mut d = lock(data);
        let before = d.items.len();
        d.items.retain(|k| k != key);
        d.items.len() != before
    };
    if removed {
        emit(conn, key, false).await;
    }
}

async fn emit(conn: &Connection, key: &str, registered: bool) {
    let Ok(iface) = conn.object_server().interface::<_, Watcher>(WATCHER_PATH).await else { return };
    let em = iface.signal_emitter();
    let r = if registered {
        Watcher::status_notifier_item_registered(em, key).await
    } else {
        Watcher::status_notifier_item_unregistered(em, key).await
    };
    if let Err(e) = r {
        tracing::debug!("tray: watcher signal for {key}: {e}");
    }
    let guard = iface.get().await;
    if let Err(e) = guard.registered_status_notifier_items_changed(em).await {
        tracing::debug!("tray: watcher property change: {e}");
    }
}

/// `StatusNotifierHostRegistered` (we became the watcher, hosts may start registering).
pub(crate) async fn announce_host(conn: &Connection, data: &Shared, host: &str) {
    lock(data).hosts.push(host.to_string());
    let Ok(iface) = conn.object_server().interface::<_, Watcher>(WATCHER_PATH).await else { return };
    if let Err(e) = Watcher::status_notifier_host_registered(iface.signal_emitter()).await {
        tracing::debug!("tray: host signal: {e}");
    }
}
