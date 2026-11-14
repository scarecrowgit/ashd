//! The Wayland side: `ext-idle-notify-v1` on the default seat.
//!
//! One connection, one reader thread (`ashd-svc-idle-wl`) that dispatches events and
//! forwards them as [`Input`]s; requests (creating / destroying notification objects) are
//! sent from the service thread through the thread-safe `QueueHandle`. The compositor
//! decides what "idle" means — niri counts every input event and honours
//! `zwp_idle_inhibit_v1` (a fullscreen video inhibits `get_idle_notification`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use async_channel::Sender;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

use crate::{Backend, Input, model::Kind};

struct WlState {
    tx: Sender<Input>,
    generation: Arc<AtomicU64>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WlState {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_seat::WlSeat, ()> for WlState {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ExtIdleNotifierV1, ()> for WlState {
    fn event(_: &mut Self, _: &ExtIdleNotifierV1, _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ExtIdleNotificationV1, (u64, Kind)> for WlState {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        (generation, kind): &(u64, Kind),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Objects of a replaced configuration may still deliver queued events.
        if *generation != state.generation.load(Ordering::Acquire) {
            return;
        }
        let input = match event {
            ext_idle_notification_v1::Event::Idled => Input::Idled(*kind),
            ext_idle_notification_v1::Event::Resumed => Input::Resumed,
            _ => return,
        };
        let _ = state.tx.try_send(input);
    }
}

pub(crate) struct WlBackend {
    conn: Connection,
    qh: QueueHandle<WlState>,
    notifier: ExtIdleNotifierV1,
    seat: wl_seat::WlSeat,
    generation: Arc<AtomicU64>,
    notifications: Vec<ExtIdleNotificationV1>,
    stop: Arc<AtomicBool>,
}

impl WlBackend {
    /// Connect to `$WAYLAND_DISPLAY`, bind the notifier and the seat, start the reader.
    pub(crate) fn connect(tx: Sender<Input>) -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<WlState>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
        let qh = queue.handle();
        let notifier = globals
            .bind::<ExtIdleNotifierV1, _, _>(&qh, 1..=2, ())
            .map_err(|_| "the compositor does not offer ext_idle_notifier_v1".to_string())?;
        let seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=4, ()).map_err(|_| "no wl_seat".to_string())?;
        let generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut state = WlState { tx: tx.clone(), generation: generation.clone() };
        // The binds above are requests; make sure the compositor has them.
        queue.roundtrip(&mut state).map_err(|e| format!("wayland roundtrip: {e}"))?;
        let reader_conn = conn.clone();
        let reader_stop = stop.clone();
        std::thread::Builder::new()
            .name("ashd-svc-idle-wl".into())
            .spawn(move || {
                if let Err(e) = read_loop(&reader_conn, &mut queue, &mut state, &reader_stop) {
                    tracing::debug!("idle: wayland connection ended: {e}");
                    let _ = state.tx.try_send(Input::Lost);
                }
            })
            .map_err(|e| format!("spawn reader: {e}"))?;
        Ok(Self { conn, qh, notifier, seat, generation, notifications: Vec::new(), stop })
    }
}

impl Backend for WlBackend {
    fn set_timeouts(&mut self, lock: Option<Duration>, screen_off: Option<Duration>) {
        for n in self.notifications.drain(..) {
            n.destroy();
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        for (kind, after) in [(Kind::Lock, lock), (Kind::ScreenOff, screen_off)] {
            let Some(after) = after else { continue };
            let ms = u32::try_from(after.as_millis()).unwrap_or(u32::MAX);
            // `get_idle_notification` (not the `input` variant): honours idle inhibitors.
            self.notifications.push(self.notifier.get_idle_notification(ms, &self.seat, &self.qh, (generation, kind)));
        }
        if let Err(e) = self.conn.flush() {
            tracing::debug!("idle: flush: {e}");
        }
    }
}

impl Drop for WlBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for n in self.notifications.drain(..) {
            n.destroy();
        }
        self.notifier.destroy();
        let _ = self.conn.flush();
    }
}

/// Dispatch until the connection breaks or `stop` is set (checked every 250 ms).
fn read_loop(conn: &Connection, queue: &mut wayland_client::EventQueue<WlState>, state: &mut WlState, stop: &AtomicBool) -> Result<(), String> {
    while !stop.load(Ordering::Acquire) {
        queue.dispatch_pending(state).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = [PollFd::new(guard.connection_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::from(250u8)) {
            Ok(n) if n > 0 => {
                guard.read().map_err(|e| e.to_string())?;
            }
            Ok(_) | Err(nix::errno::Errno::EINTR) => drop(guard),
            Err(e) => return Err(format!("poll: {e}")),
        }
    }
    Ok(())
}
