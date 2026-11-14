//! Notifications: the `org.freedesktop.Notifications` server (spec 1.2) on the session
//! bus, via zbus 5. No GPUI.
//!
//! Contract (ARCHITECTURE.md "Services"): `start() -> (Handle, Receiver<NotificationsState>)`.
//! The state is the full ordered history (oldest first, capped at [`core::HISTORY_CAP`]).
//! The toasts panel shows entries that are not `expired` and whose `seq` is newer than what
//! it has seen; the notification centre lists `notifications`. Owner: notifications.
//!
//! Lifecycle: a toast whose timer ran out calls [`Handle::expire`] (`expired = true`,
//! `NotificationClosed(.., 1)`, the entry stays in the history — transient ones are
//! dropped); the user closing one calls [`Handle::dismiss`] (removed, reason 2); an app's
//! `CloseNotification` removes it with reason 3. Replacing (`replaces_id`, or the
//! `synchronous` / `x-canonical-private-synchronous` / `x-dunst-stack-tag` hint) keeps the
//! id, bumps `seq`, moves the entry to the end and revives it (`expired = false`).
//!
//! Testing: only ever run the server on a private bus (the nested session starts one; the
//! tests start their own `dbus-daemon`); on the live session bus it would fight the user's
//! notification daemon. Use [`start_at`] with the private bus address.

mod bus;
pub mod core;
pub mod image;
pub mod markup;

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_channel::{Receiver, Sender};

/// Toast lifetime when the app does not ask for one (the timer bar of det-05).
pub const DEFAULT_TIMEOUT_MS: u32 = 6000;

/// [`LocalNotification::icon`] prefix of a ashd glyph name.
pub const ASHD_ICON_PREFIX: &str = "ashd:";

/// Advertise the `inline-reply` capability. Off: the toasts live on a passive layer
/// surface that cannot take keyboard input, so a reply field there would be a lie. Turn it
/// on once the notification centre (a modal, keyboard exclusive) hosts the reply field;
/// the service side (hint parsing, `NotificationReplied`, [`Handle::reply`]) is complete.
pub const ADVERTISE_INLINE_REPLY: bool = true;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NotificationImage {
    /// `image-path` hint / app_icon given as a file, or an icon-theme name resolved to its file.
    Path(PathBuf),
    /// Icon theme name that could not be resolved to a file.
    Name(String),
    /// `image-data` hint, converted to RGBA8 (max side [`image::MAX_SIDE`]).
    Rgba { width: u32, height: u32, data: Arc<Vec<u8>> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// D-Bus notification id (replaces_id target).
    pub id: u32,
    /// Monotonic arrival counter across the session (also bumped on replace).
    pub seq: u64,
    pub app_name: String,
    pub summary: String,
    /// Body with the spec's markup subset stripped to plain text.
    pub body: String,
    /// All actions the app sent, including `"default"` (the click action) and, if the app
    /// sent one, `"inline-reply"`; buttons come from [`Notification::button_actions`].
    pub actions: Vec<Action>,
    pub urgency: Urgency,
    pub image: Option<NotificationImage>,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
    /// Requested timeout: `None` = server default, `Some(0)` = never.
    pub expire_timeout_ms: Option<u32>,
    pub resident: bool,
    pub transient: bool,
    /// `value` hint (0..=100): a progress bar.
    pub progress: Option<u8>,
    /// Inline reply supported (`x-kde-reply-placeholder-text` hint or an `inline-reply`
    /// action); the text is the field's placeholder.
    pub reply_placeholder: Option<String>,
    /// The toast is over (timed out, or an action was taken on a resident notification);
    /// the entry stays in the history.
    pub expired: bool,
    /// Replace-by-tag hint (`synchronous`, `x-canonical-private-synchronous`, `x-dunst-stack-tag`).
    pub sync_tag: Option<String>,
    pub received: chrono::DateTime<chrono::Local>,
}

impl Notification {
    /// How long a toast stays: critical never expires, `Some(0)` means never, `None` the
    /// default [`DEFAULT_TIMEOUT_MS`].
    pub fn timeout(&self) -> Option<Duration> {
        if self.urgency == Urgency::Critical {
            return None;
        }
        match self.expire_timeout_ms {
            Some(0) => None,
            Some(ms) => Some(Duration::from_millis(ms.into())),
            None => Some(Duration::from_millis(DEFAULT_TIMEOUT_MS.into())),
        }
    }

    /// Whether this one may pop up as a toast given the do-not-disturb flag.
    pub fn toast_allowed(&self, dnd: bool) -> bool {
        !self.expired && (!dnd || self.urgency == Urgency::Critical)
    }

    /// The actions to draw as buttons: everything but `default` and `inline-reply`.
    pub fn button_actions(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(|a| a.key != "default" && a.key != "inline-reply")
    }

    pub fn has_default_action(&self) -> bool {
        self.actions.iter().any(|a| a.key == "default")
    }

    /// `NotificationClosed` has been sent already (non-resident and expired).
    pub(crate) fn closed_signalled(&self) -> bool {
        self.expired && !self.resident
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotificationsState {
    /// The bus name was acquired (false: another daemon owns it, or no session bus).
    pub available: bool,
    /// Oldest first.
    pub notifications: Vec<Notification>,
    /// Do-not-disturb: notifications are stored but toasts do not pop (critical still does).
    pub dnd: bool,
    /// Highest `seq` so far.
    pub last_seq: u64,
}

/// `NotificationClosed` reasons (spec).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    /// `CloseNotification` call from the app.
    Closed = 3,
    Undefined = 4,
}

/// A notification the shell itself raises (network link events, …): the same store, toast
/// path and history as a D-Bus `Notify`, without the bus round trip. Works with no session
/// bus at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalNotification {
    /// Header of the toast (`network`).
    pub app_name: String,
    pub summary: String,
    /// Plain text.
    pub body: String,
    /// An icon-theme name, a file path, or `ashd:<name>` for a ashd glyph (`ashd:ethernet`;
    /// stored as `NotificationImage::Name(name)`, never looked up in the icon theme).
    pub icon: Option<String>,
    pub urgency: Urgency,
    /// `None`: [`DEFAULT_TIMEOUT_MS`]; `Some(ZERO)`: until dismissed.
    pub timeout: Option<Duration>,
    /// Buttons (no `default`: there is no app to send `ActionInvoked` to).
    pub actions: Vec<Action>,
    /// Freedesktop category (`network.connected`, `network.disconnected`, `network.error`).
    pub category: Option<String>,
    /// Replace the previous local notification with this tag (one network toast, not a
    /// stack of them).
    pub tag: Option<String>,
}

impl LocalNotification {
    fn into_request(self) -> (core::NewNotification, image::ImageSpec) {
        let spec = image::ImageSpec {
            app_icon: self.icon.clone().unwrap_or_default(),
            app_name: self.app_name.clone(),
            ..Default::default()
        };
        let req = core::NewNotification {
            app_name: self.app_name,
            replaces_id: 0,
            summary: self.summary,
            body: self.body,
            actions: self.actions,
            urgency: self.urgency,
            image: None,
            category: self.category,
            desktop_entry: None,
            expire_timeout_ms: self.timeout.map(|d| d.as_millis().min(u32::MAX as u128) as u32),
            resident: false,
            transient: false,
            progress: None,
            reply_placeholder: None,
            sync_tag: self.tag,
            received: chrono::Local::now(),
        };
        (req, spec)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// [`Handle::post`].
    Post(Box<LocalNotification>),
    Dismiss(u32, CloseReason),
    DismissAll,
    InvokeAction { id: u32, key: String },
    Reply { id: u32, text: String },
    SetDnd(bool),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.try_send(cmd);
    }
    /// User dismissed (emits `NotificationClosed(id, 2)`).
    pub fn dismiss(&self, id: u32) {
        self.send(Command::Dismiss(id, CloseReason::Dismissed));
    }
    /// Toast timed out (emits `NotificationClosed(id, 1)` unless resident).
    pub fn expire(&self, id: u32) {
        self.send(Command::Dismiss(id, CloseReason::Expired));
    }
    pub fn dismiss_all(&self) {
        self.send(Command::DismissAll);
    }
    /// Emits `ActionInvoked(id, key)` (only for keys the app offered); the default action
    /// key is `"default"`. Closes the notification unless it is resident.
    pub fn invoke_action(&self, id: u32, key: &str) {
        self.send(Command::InvokeAction { id, key: key.into() });
    }
    /// Inline reply: emits `NotificationReplied(id, text)`.
    pub fn reply(&self, id: u32, text: &str) {
        self.send(Command::Reply { id, text: text.into() });
    }
    pub fn set_dnd(&self, on: bool) {
        self.send(Command::SetDnd(on));
    }
    /// Raise a shell-originated notification (toast + history), no D-Bus involved.
    pub fn post(&self, n: LocalNotification) {
        self.send(Command::Post(Box::new(n)));
    }
    pub fn disconnected() -> Self {
        Self { tx: async_channel::bounded(1).0 }
    }
}

/// Spawn the `ashd-svc-notifications` thread and claim the bus name on the session bus.
pub fn start() -> (Handle, Receiver<NotificationsState>) {
    start_at(None)
}

/// Like [`start`], on an explicit bus address (tests, the nested session).
pub fn start_at(address: Option<String>) -> (Handle, Receiver<NotificationsState>) {
    let (tx, rx) = async_channel::unbounded();
    let (state_tx, state_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("ashd-svc-notifications".into())
        .spawn(move || smol::block_on(run(rx, state_tx, address)))
        .expect("spawn ashd-svc-notifications");
    (Handle { tx }, state_rx)
}

async fn run(commands: Receiver<Command>, state: Sender<NotificationsState>, address: Option<String>) {
    let shared = Arc::new(bus::Shared::new(state));
    shared.publish();
    let conn_task = smol::spawn(bus::manage(shared.clone(), address));
    while let Ok(cmd) = commands.recv().await {
        if let Command::Post(n) = cmd {
            let (mut req, spec) = n.into_request();
            req.image = match spec.app_icon.strip_prefix(ASHD_ICON_PREFIX) {
                // a ashd glyph: never looked up in the icon theme
                Some(name) => Some(NotificationImage::Name(name.to_string())),
                None => smol::unblock(move || image::resolve(spec)).await,
            };
            let events = shared.mutate(|c| local_notify(c, req));
            shared.emit(events).await;
            continue;
        }
        let events = shared.mutate(|c| match cmd {
            Command::Post(_) => unreachable!("handled above"),
            Command::Dismiss(id, reason) => c.close(id, reason),
            Command::DismissAll => c.dismiss_all(),
            Command::InvokeAction { id, key } => c.invoke(id, &key),
            Command::Reply { id, text } => c.reply(id, text),
            Command::SetDnd(on) => {
                c.set_dnd(on);
                vec![]
            }
        });
        shared.emit(events).await;
    }
    conn_task.cancel().await;
}

/// Store a local notification: replaces the previous one with the same tag from the same
/// "app" (D-Bus notifications never share a tag with it: theirs come with their app name).
fn local_notify(c: &mut core::Core, req: core::NewNotification) -> Vec<core::Event> {
    c.notify(req).1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(urgency: Urgency, timeout: Option<u32>) -> Notification {
        Notification {
            id: 1,
            seq: 1,
            app_name: String::new(),
            summary: String::new(),
            body: String::new(),
            actions: vec![
                Action { key: "default".into(), label: "Open".into() },
                Action { key: "inline-reply".into(), label: "Reply".into() },
                Action { key: "snooze".into(), label: "Snooze".into() },
            ],
            urgency,
            image: None,
            category: None,
            desktop_entry: None,
            expire_timeout_ms: timeout,
            resident: false,
            transient: false,
            progress: None,
            reply_placeholder: None,
            expired: false,
            sync_tag: None,
            received: chrono::Local::now(),
        }
    }

    #[test]
    fn timeout_rules() {
        assert_eq!(n(Urgency::Normal, None).timeout(), Some(Duration::from_millis(6000)));
        assert_eq!(n(Urgency::Low, Some(2500)).timeout(), Some(Duration::from_millis(2500)));
        assert_eq!(n(Urgency::Normal, Some(0)).timeout(), None);
        assert_eq!(n(Urgency::Critical, None).timeout(), None);
        assert_eq!(n(Urgency::Critical, Some(3000)).timeout(), None);
    }

    #[test]
    fn dnd_suppresses_all_but_critical() {
        assert!(n(Urgency::Normal, None).toast_allowed(false));
        assert!(!n(Urgency::Normal, None).toast_allowed(true));
        assert!(n(Urgency::Critical, None).toast_allowed(true));
        let mut e = n(Urgency::Critical, None);
        e.expired = true;
        assert!(!e.toast_allowed(false));
    }

    #[test]
    fn buttons_skip_default_and_reply() {
        let keys: Vec<_> = n(Urgency::Normal, None).button_actions().map(|a| a.key.clone()).collect();
        assert_eq!(keys, ["snooze"]);
        assert!(n(Urgency::Normal, None).has_default_action());
    }

    /// No bus at all (an address nobody listens on): local posts still reach the state,
    /// and a tagged post replaces the previous one.
    #[test]
    fn local_posts_need_no_bus_and_replace_by_tag() {
        let (handle, rx) = start_at(Some("unix:path=/nonexistent/ashd-test-bus".into()));
        let wait = |pred: &dyn Fn(&NotificationsState) -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let s = smol::block_on(async {
                    futures_lite::future::or(async { rx.recv().await.ok() }, async {
                        smol::Timer::after(Duration::from_millis(200)).await;
                        None
                    })
                    .await
                });
                if let Some(s) = s
                    && pred(&s)
                {
                    return s;
                }
                assert!(std::time::Instant::now() < deadline, "timed out");
            }
        };
        let post = |summary: &str| LocalNotification {
            app_name: "network".into(),
            summary: summary.into(),
            body: "enp5s0".into(),
            icon: Some("ashd:ethernet".into()),
            category: Some("network.connected".into()),
            tag: Some("link".into()),
            ..Default::default()
        };
        handle.post(post("Ethernet connected"));
        let s = wait(&|s| !s.notifications.is_empty());
        let first = s.notifications[0].clone();
        assert_eq!((first.app_name.as_str(), first.summary.as_str()), ("network", "Ethernet connected"));
        assert_eq!(first.category.as_deref(), Some("network.connected"));
        assert_eq!(first.timeout(), Some(Duration::from_millis(DEFAULT_TIMEOUT_MS.into())));
        handle.post(post("Ethernet cable unplugged"));
        let s = wait(&|s| s.notifications.iter().any(|n| n.summary == "Ethernet cable unplugged"));
        assert_eq!(s.notifications.len(), 1, "the tag replaced the first one");
        assert_eq!(s.notifications[0].id, first.id);
        assert!(s.notifications[0].seq > first.seq, "a replacement toasts again");
    }
}
