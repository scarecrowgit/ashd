//! D-Bus side: the `org.freedesktop.Notifications` object, hint parsing, and the
//! connection manager (connect, claim the name, re-claim after loss, reconnect).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_channel::Sender;
use futures_lite::StreamExt as _;
use zbus::{
    Connection,
    fdo::{DBusProxy, RequestNameReply},
    object_server::SignalEmitter,
};
use zvariant::{OwnedValue, Value};

use crate::{
    Action, CloseReason, NotificationsState, Urgency,
    core::{Core, Event, NewNotification},
    image::{self, ImageSpec, RawImage},
    markup,
};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

/// Bounds on what one app can make us store.
const MAX_ACTIONS: usize = 16;
const MAX_BODY_CHARS: usize = 8192;
const MAX_SUMMARY_CHARS: usize = 512;

/// State shared between the command loop, the connection manager and the D-Bus handlers.
pub struct Shared {
    core: Mutex<Core>,
    state_tx: Sender<NotificationsState>,
    conn: Mutex<Option<Connection>>,
}

impl Shared {
    pub fn new(state_tx: Sender<NotificationsState>) -> Self {
        Self { core: Mutex::new(Core::new()), state_tx, conn: Mutex::new(None) }
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publish the current snapshot (the consumer always sees the newest one).
    pub fn publish(&self) {
        let snap = self.core().snapshot();
        let _ = self.state_tx.force_send(snap);
    }

    /// Mutate the store, then publish.
    pub fn mutate<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        let (r, snap) = {
            let mut core = self.core();
            let r = f(&mut core);
            (r, core.snapshot())
        };
        let _ = self.state_tx.force_send(snap);
        r
    }

    fn set_available(&self, available: bool) {
        self.mutate(|c| c.set_available(available));
    }

    fn connection(&self) -> Option<Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Emit the signals; silently skipped while there is no bus connection.
    pub async fn emit(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        let Some(conn) = self.connection() else { return };
        let emitter = match SignalEmitter::new(&conn, PATH) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("signal emitter: {e}");
                return;
            }
        };
        for ev in events {
            let res = match ev {
                Event::Closed(id, reason) => Server::notification_closed(&emitter, id, reason as u32).await,
                Event::Action(id, key) => Server::action_invoked(&emitter, id, key).await,
                Event::Replied(id, text) => Server::notification_replied(&emitter, id, text).await,
            };
            if let Err(e) = res {
                tracing::warn!("cannot emit notification signal: {e}");
            }
        }
    }
}

struct Server {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let (req, spec) = parse_notify(app_name, replaces_id, app_icon, summary, body, actions, &hints, expire_timeout);
        // Decode / look up the picture off the service thread.
        let image = smol::unblock(move || image::resolve(spec)).await;
        let (id, events) = self.shared.mutate(|c| c.notify(NewNotification { image, ..req }));
        self.shared.emit(events).await;
        id
    }

    async fn close_notification(&self, id: u32) {
        let events = self.shared.mutate(|c| c.close(id, CloseReason::Closed));
        self.shared.emit(events).await;
    }

    fn get_capabilities(&self) -> Vec<String> {
        capabilities()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("ashd".into(), "ashd".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_replied(emitter: &SignalEmitter<'_>, id: u32, text: String) -> zbus::Result<()>;
}

pub fn capabilities() -> Vec<String> {
    let mut caps = vec!["body", "body-markup", "actions", "icon-static", "persistence"];
    if crate::ADVERTISE_INLINE_REPLY {
        caps.push("inline-reply");
    }
    caps.into_iter().map(String::from).collect()
}

// ── connection management ─────────────────────────────────────────────────────

/// Keep a connection alive: connect, serve, claim the name (queued if somebody else owns
/// it: `available` stays false until the owner goes away), re-claim after `NameLost`.
pub async fn manage(shared: Arc<Shared>, address: Option<String>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match serve(&shared, address.as_deref()).await {
            Ok(()) => backoff = Duration::from_secs(1),
            Err(e) => tracing::warn!("notifications: {e}"),
        }
        *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
        shared.set_available(false);
        smol::Timer::after(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

enum Ev {
    Acquired(Option<zbus::fdo::NameAcquired>),
    Lost(Option<zbus::fdo::NameLost>),
}

async fn serve(shared: &Arc<Shared>, address: Option<&str>) -> zbus::Result<()> {
    let builder = match address {
        Some(a) => zbus::connection::Builder::address(a)?,
        None => zbus::connection::Builder::session()?,
    };
    let conn = builder.serve_at(PATH, Server { shared: shared.clone() })?.build().await?;
    let dbus = DBusProxy::new(&conn).await?;
    // Streams first, so no signal can slip between the request and the subscription.
    let mut acquired = dbus.receive_name_acquired().await?;
    let mut lost = dbus.receive_name_lost().await?;
    *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.clone());

    claim(shared, &conn).await?;
    loop {
        let ev = futures_lite::future::or(async { Ev::Acquired(acquired.next().await) }, async {
            Ev::Lost(lost.next().await)
        })
        .await;
        match ev {
            Ev::Acquired(None) | Ev::Lost(None) => return Ok(()),
            Ev::Acquired(Some(sig)) => {
                if sig.args().is_ok_and(|a| a.name.as_str() == NAME) {
                    tracing::info!("notifications: acquired {NAME}");
                    shared.set_available(true);
                }
            }
            Ev::Lost(Some(sig)) => {
                if sig.args().is_ok_and(|a| a.name.as_str() == NAME) {
                    tracing::warn!("notifications: lost {NAME}, queueing for it again");
                    shared.set_available(false);
                    claim(shared, &conn).await?;
                }
            }
        }
    }
}

async fn claim(shared: &Arc<Shared>, conn: &Connection) -> zbus::Result<()> {
    // No DoNotQueue: if another daemon owns the name we wait in the queue and the bus
    // hands it to us (NameAcquired) when that daemon exits.
    // Empty flags: never ReplaceExisting (zbus's default flags would steal the name from a
    // running daemon), never DoNotQueue.
    let flags = std::iter::empty::<zbus::fdo::RequestNameFlags>().collect();
    let reply = conn.request_name_with_flags(NAME, flags).await?;
    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => shared.set_available(true),
        RequestNameReply::InQueue | RequestNameReply::Exists => {
            tracing::warn!("notifications: {NAME} is owned by another daemon, waiting");
            shared.set_available(false);
        }
    }
    Ok(())
}

// ── Notify parsing ────────────────────────────────────────────────────────────

fn unwrap<'a, 'b>(v: &'a Value<'b>) -> &'a Value<'b> {
    match v {
        Value::Value(inner) => unwrap(inner),
        v => v,
    }
}

fn hint_str(h: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match unwrap(h.get(key)?) {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn hint_int(h: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    match unwrap(h.get(key)?) {
        Value::U8(v) => Some((*v).into()),
        Value::I16(v) => Some((*v).into()),
        Value::U16(v) => Some((*v).into()),
        Value::I32(v) => Some((*v).into()),
        Value::U32(v) => Some((*v).into()),
        Value::I64(v) => Some(*v),
        Value::U64(v) => i64::try_from(*v).ok(),
        Value::F64(v) => Some(*v as i64),
        _ => None,
    }
}

fn hint_bool(h: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match unwrap(h.get(key)?) {
        Value::Bool(b) => Some(*b),
        _ => hint_int(h, key).map(|v| v != 0),
    }
}

fn hint_image(h: &HashMap<String, OwnedValue>, key: &str) -> Option<RawImage> {
    let Value::Structure(s) = unwrap(h.get(key)?) else { return None };
    let f = s.fields();
    if f.len() < 7 {
        return None;
    }
    let int = |i: usize| match unwrap(&f[i]) {
        Value::I32(v) => Some(*v),
        _ => None,
    };
    let Value::Bool(has_alpha) = unwrap(&f[3]) else { return None };
    let data = match unwrap(&f[6]) {
        Value::Array(a) => {
            let a = a.try_clone().ok()?;
            Vec::<u8>::try_from(a).ok()?
        }
        _ => return None,
    };
    Some(RawImage {
        width: int(0)?,
        height: int(1)?,
        rowstride: int(2)?,
        has_alpha: *has_alpha,
        bits_per_sample: int(4)?,
        channels: int(5)?,
        data,
    })
}

fn truncate(mut s: String, max_chars: usize) -> String {
    if let Some((ix, _)) = s.char_indices().nth(max_chars) {
        s.truncate(ix);
    }
    s
}

/// Turn the raw `Notify` arguments into a store request plus the image to resolve.
#[allow(clippy::too_many_arguments)]
pub fn parse_notify(
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: &HashMap<String, OwnedValue>,
    expire_timeout: i32,
) -> (NewNotification, ImageSpec) {
    let urgency = match hint_int(hints, "urgency") {
        Some(0) => Urgency::Low,
        Some(2) => Urgency::Critical,
        _ => Urgency::Normal,
    };
    let desktop_entry = hint_str(hints, "desktop-entry").or_else(|| hint_str(hints, "desktop_entry")).filter(|s| !s.is_empty());
    let mut parsed_actions: Vec<Action> = actions
        .chunks(2)
        .filter(|c| c.len() == 2 && !c[0].is_empty())
        .take(MAX_ACTIONS)
        .map(|c| Action { key: c[0].clone(), label: markup::plain(&c[1]) })
        .collect();
    let reply_action = parsed_actions.iter().position(|a| a.key == "inline-reply");
    let reply_hint = hint_str(hints, "x-kde-reply-placeholder-text").filter(|s| !s.is_empty());
    let reply_placeholder = match (reply_action, reply_hint) {
        (_, Some(p)) => Some(p),
        (Some(ix), None) => Some(std::mem::take(&mut parsed_actions[ix].label)),
        (None, None) => None,
    };
    let sync_tag = ["synchronous", "x-canonical-private-synchronous", "x-dunst-stack-tag"]
        .iter()
        .find_map(|k| hint_str(hints, k))
        .filter(|s| !s.is_empty());
    let progress = hint_int(hints, "value").map(|v| v.clamp(0, 100) as u8);
    let app_name = truncate(markup::plain(&app_name), 128);

    let spec = ImageSpec {
        data: hint_image(hints, "image-data").or_else(|| hint_image(hints, "image_data")),
        path: hint_str(hints, "image-path").or_else(|| hint_str(hints, "image_path")),
        app_icon,
        legacy_data: hint_image(hints, "icon_data"),
        desktop_entry: desktop_entry.clone(),
        app_name: app_name.clone(),
    };
    let req = NewNotification {
        app_name,
        replaces_id,
        summary: truncate(markup::plain(&summary), MAX_SUMMARY_CHARS),
        body: truncate(markup::strip_markup(&body), MAX_BODY_CHARS),
        actions: parsed_actions,
        urgency,
        image: None,
        category: hint_str(hints, "category"),
        desktop_entry,
        expire_timeout_ms: u32::try_from(expire_timeout).ok().filter(|_| expire_timeout >= 0),
        resident: hint_bool(hints, "resident").unwrap_or(false),
        transient: hint_bool(hints, "transient").unwrap_or(false),
        progress,
        reply_placeholder,
        sync_tag,
        received: chrono::Local::now(),
    };
    (req, spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(items: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        items.into_iter().map(|(k, v)| (k.to_string(), v.try_to_owned().unwrap())).collect()
    }

    fn parse(h: &HashMap<String, OwnedValue>, actions: &[&str], timeout: i32) -> (NewNotification, ImageSpec) {
        parse_notify(
            "App".into(),
            0,
            "icon-name".into(),
            "Sum".into(),
            "Body <b>x</b> &amp; y".into(),
            actions.iter().map(|s| s.to_string()).collect(),
            h,
            timeout,
        )
    }

    #[test]
    fn urgency_flags_and_timeout() {
        let h = hints(vec![
            ("urgency", Value::U8(2)),
            ("transient", Value::Bool(true)),
            ("resident", Value::I32(1)),
            ("category", Value::from("email.arrived")),
            ("desktop-entry", Value::from("org.example.App")),
            ("value", Value::I32(250)),
        ]);
        let (r, _) = parse(&h, &[], -1);
        assert_eq!(r.urgency, Urgency::Critical);
        assert!(r.transient && r.resident);
        assert_eq!(r.category.as_deref(), Some("email.arrived"));
        assert_eq!(r.desktop_entry.as_deref(), Some("org.example.App"));
        assert_eq!(r.progress, Some(100));
        assert_eq!(r.expire_timeout_ms, None);
        assert_eq!(r.body, "Body x & y");
        assert_eq!(parse(&h, &[], 0).0.expire_timeout_ms, Some(0));
        assert_eq!(parse(&h, &[], 1500).0.expire_timeout_ms, Some(1500));
        assert_eq!(parse(&h, &[], -5).0.expire_timeout_ms, None);
        let (low, _) = parse(&hints(vec![("urgency", Value::U8(0))]), &[], -1);
        assert_eq!(low.urgency, Urgency::Low);
        let (dflt, _) = parse(&hints(vec![]), &[], -1);
        assert_eq!(dflt.urgency, Urgency::Normal);
    }

    #[test]
    fn actions_pair_up_and_inline_reply_is_split_off() {
        let h = hints(vec![]);
        let (r, _) = parse(&h, &["default", "Open", "snooze", "Snooze", "odd"], -1);
        assert_eq!(r.actions.len(), 2);
        assert_eq!(r.actions[1], Action { key: "snooze".into(), label: "Snooze".into() });
        assert_eq!(r.reply_placeholder, None);
        let (r, _) = parse(&h, &["inline-reply", "Reply here"], -1);
        assert_eq!(r.reply_placeholder.as_deref(), Some("Reply here"));
        let h = hints(vec![("x-kde-reply-placeholder-text", Value::from("Type…"))]);
        let (r, _) = parse(&h, &["inline-reply", "Reply"], -1);
        assert_eq!(r.reply_placeholder.as_deref(), Some("Type…"));
    }

    #[test]
    fn sync_tag_hints() {
        let h = hints(vec![("x-canonical-private-synchronous", Value::from("volume"))]);
        assert_eq!(parse(&h, &[], -1).0.sync_tag.as_deref(), Some("volume"));
    }

    #[test]
    fn image_data_struct_is_parsed() {
        let bytes: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let st = zvariant::StructureBuilder::new()
            .add_field(2i32)
            .add_field(1i32)
            .add_field(8i32)
            .add_field(true)
            .add_field(8i32)
            .add_field(4i32)
            .add_field(bytes.clone())
            .build()
            .unwrap();
        let h = hints(vec![("image-data", Value::Structure(st)), ("image-path", Value::from("/x.png"))]);
        let (_, spec) = parse(&h, &[], -1);
        let raw = spec.data.unwrap();
        assert_eq!((raw.width, raw.height, raw.rowstride, raw.has_alpha, raw.channels), (2, 1, 8, true, 4));
        assert_eq!(raw.data, bytes);
        assert_eq!(spec.path.as_deref(), Some("/x.png"));
    }

    #[test]
    fn inputs_are_bounded() {
        let (r, _) = parse_notify("a".repeat(500), 0, String::new(), "s".repeat(2000), "b".repeat(20_000), vec![], &hints(vec![]), -1);
        assert_eq!(r.app_name.chars().count(), 128);
        assert_eq!(r.summary.chars().count(), MAX_SUMMARY_CHARS);
        assert_eq!(r.body.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn capabilities_list() {
        let c = capabilities();
        for want in ["body", "body-markup", "actions", "icon-static", "persistence"] {
            assert!(c.iter().any(|x| x == want), "{want}");
        }
    }
}
