//! End-to-end against a private `dbus-daemon` (never the user's session bus). Skipped
//! when `dbus-daemon` is not on PATH (outside the dev shell).

use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::Duration,
};

use futures_lite::StreamExt as _;
use ashd_svc_notifications::{CloseReason, NotificationImage, NotificationsState, Urgency, start_at};
use zbus::{Connection, MessageStream, message::Type};
use zvariant::{OwnedValue, Value};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Bus> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--print-address", "--nofork"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        Some(Bus { child, address: line.trim().to_string() })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn client(address: &str) -> Connection {
    zbus::connection::Builder::address(address).unwrap().build().await.unwrap()
}

async fn wait_state(
    rx: &async_channel::Receiver<NotificationsState>,
    what: &str,
    pred: impl Fn(&NotificationsState) -> bool,
) -> NotificationsState {
    let fut = async {
        loop {
            let s = rx.recv().await.expect("state channel closed");
            if pred(&s) {
                return s;
            }
        }
    };
    futures_lite::future::or(fut, async {
        smol::Timer::after(Duration::from_secs(10)).await;
        panic!("timeout waiting for state: {what}")
    })
    .await
}

fn hints(items: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
    items.into_iter().map(|(k, v)| (k.to_string(), v.try_to_owned().unwrap())).collect()
}

#[allow(clippy::too_many_arguments)]
async fn notify(
    c: &Connection,
    app: &str,
    replaces: u32,
    summary: &str,
    body: &str,
    actions: Vec<&str>,
    h: HashMap<String, OwnedValue>,
    timeout: i32,
) -> u32 {
    let reply = c
        .call_method(Some(NAME), PATH, Some(NAME), "Notify", &(app, replaces, "", summary, body, actions, h, timeout))
        .await
        .unwrap();
    reply.body().deserialize::<u32>().unwrap()
}

async fn next_signal(stream: &mut MessageStream, member: &str) -> zbus::Message {
    let fut = async {
        loop {
            let m = stream.next().await.unwrap().unwrap();
            if m.message_type() == Type::Signal && m.header().member().is_some_and(|x| x.as_str() == member) {
                return m;
            }
        }
    };
    futures_lite::future::or(fut, async {
        smol::Timer::after(Duration::from_secs(10)).await;
        panic!("timeout waiting for signal {member}")
    })
    .await
}

#[test]
fn full_round_trip() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not found: skipping");
        return;
    };
    smol::block_on(async {
        let (handle, rx) = start_at(Some(bus.address.clone()));
        wait_state(&rx, "available", |s| s.available).await;
        let c = client(&bus.address).await;
        c.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "AddMatch",
            &("type='signal',interface='org.freedesktop.Notifications'",)).await.unwrap();
        let mut signals = MessageStream::from(&c);

        // server information & capabilities
        let info = c.call_method(Some(NAME), PATH, Some(NAME), "GetServerInformation", &()).await.unwrap();
        let (name, _vendor, _ver, spec): (String, String, String, String) = info.body().deserialize().unwrap();
        assert_eq!((name.as_str(), spec.as_str()), ("ashd", "1.2"));
        let caps = c.call_method(Some(NAME), PATH, Some(NAME), "GetCapabilities", &()).await.unwrap();
        let caps: Vec<String> = caps.body().deserialize().unwrap();
        assert!(caps.contains(&"actions".to_string()) && caps.contains(&"body-markup".to_string()));

        // normal notification with markup + actions
        let a = notify(&c, "App", 0, "Hello", "<b>bold</b> &amp; plain", vec!["default", "Open", "snooze", "Snooze"], hints(vec![]), -1).await;
        let s = wait_state(&rx, "one notification", |s| s.notifications.len() == 1).await;
        let n = &s.notifications[0];
        assert_eq!((n.id, n.summary.as_str(), n.body.as_str(), n.urgency), (a, "Hello", "bold & plain", Urgency::Normal));
        assert_eq!(n.actions.len(), 2);
        assert_eq!(n.timeout(), Some(Duration::from_millis(6000)));

        // replace keeps the id
        let a2 = notify(&c, "App", a, "Hello again", "", vec![], hints(vec![]), 3000).await;
        assert_eq!(a2, a);
        let s = wait_state(&rx, "replaced", |s| s.notifications.len() == 1 && s.notifications[0].summary == "Hello again").await;
        assert_eq!(s.notifications[0].seq, 2);

        // critical + image-data
        let st = zvariant::StructureBuilder::new()
            .add_field(1i32).add_field(1i32).add_field(4i32).add_field(true).add_field(8i32).add_field(4i32)
            .add_field(vec![1u8, 2, 3, 4]).build().unwrap();
        let b = notify(&c, "Bat", 0, "Battery 5%", "", vec![],
            hints(vec![("urgency", Value::U8(2)), ("image-data", Value::Structure(st))]), 1000).await;
        let s = wait_state(&rx, "critical", |s| s.notifications.len() == 2).await;
        let nb = s.notifications.iter().find(|n| n.id == b).unwrap();
        assert_eq!(nb.urgency, Urgency::Critical);
        assert_eq!(nb.timeout(), None);
        assert!(matches!(&nb.image, Some(NotificationImage::Rgba { width: 1, height: 1, .. })));

        // CloseNotification -> NotificationClosed(id, 3)
        c.call_method(Some(NAME), PATH, Some(NAME), "CloseNotification", &(a,)).await.unwrap();
        let m = next_signal(&mut signals, "NotificationClosed").await;
        let (id, reason): (u32, u32) = m.body().deserialize().unwrap();
        assert_eq!((id, reason), (a, CloseReason::Closed as u32));
        wait_state(&rx, "closed", |s| s.notifications.len() == 1).await;

        // dismiss -> reason 2
        handle.dismiss(b);
        let m = next_signal(&mut signals, "NotificationClosed").await;
        let (id, reason): (u32, u32) = m.body().deserialize().unwrap();
        assert_eq!((id, reason), (b, 2));

        // expire -> reason 1, entry stays; action -> ActionInvoked + closed(2)
        let d = notify(&c, "App", 0, "Act", "", vec!["default", "Open"], hints(vec![]), -1).await;
        wait_state(&rx, "d", |s| s.notifications.iter().any(|n| n.id == d)).await;
        handle.invoke_action(d, "default");
        let m = next_signal(&mut signals, "ActionInvoked").await;
        let (id, key): (u32, String) = m.body().deserialize().unwrap();
        assert_eq!((id, key.as_str()), (d, "default"));
        let m = next_signal(&mut signals, "NotificationClosed").await;
        let (id, reason): (u32, u32) = m.body().deserialize().unwrap();
        assert_eq!((id, reason), (d, 2));

        let e = notify(&c, "App", 0, "Expiring", "", vec![], hints(vec![]), -1).await;
        wait_state(&rx, "e", |s| s.notifications.iter().any(|n| n.id == e)).await;
        handle.expire(e);
        let m = next_signal(&mut signals, "NotificationClosed").await;
        let (id, reason): (u32, u32) = m.body().deserialize().unwrap();
        assert_eq!((id, reason), (e, 1));
        let s = wait_state(&rx, "expired flag", |s| s.notifications.iter().any(|n| n.id == e && n.expired)).await;
        assert_eq!(s.notifications.len(), 1);

        // dnd is just state
        handle.set_dnd(true);
        wait_state(&rx, "dnd", |s| s.dnd).await;
        handle.dismiss_all();
        wait_state(&rx, "empty", |s| s.notifications.is_empty()).await;
    });
}

#[test]
fn waits_for_the_name_when_taken_and_takes_over() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not found: skipping");
        return;
    };
    smol::block_on(async {
        let other = client(&bus.address).await;
        other.request_name(NAME).await.unwrap();
        let (_handle, rx) = start_at(Some(bus.address.clone()));
        // Give it time to connect and queue; it must report unavailable.
        smol::Timer::after(Duration::from_millis(600)).await;
        let mut last = None;
        while let Ok(s) = rx.try_recv() {
            last = Some(s);
        }
        assert!(last.as_ref().is_some_and(|s| !s.available), "state: {last:?}");
        other.release_name(NAME).await.unwrap();
        wait_state(&rx, "available after the other daemon left", |s| s.available).await;
    });
}

#[test]
fn survives_without_a_bus() {
    smol::block_on(async {
        let (handle, rx) = start_at(Some("unix:path=/nonexistent/ashd-test-bus".into()));
        let s = rx.recv().await.unwrap();
        assert!(!s.available);
        handle.set_dnd(true);
        let s = wait_state(&rx, "dnd without bus", |s| s.dnd).await;
        assert!(!s.available);
    });
}
