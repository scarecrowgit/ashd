//! The Wayland side: one connection, `ext-data-control-v1` (preferred) or
//! `zwlr-data-control-unstable-v1`, behind a small protocol-agnostic facade.
//!
//! Everything here runs on the service thread (blocking `poll`), never on a UI thread.
//! The connection comes from `WAYLAND_DISPLAY`: callers decide *which* compositor that is.

use std::{
    io::Read as _,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::{Arc, Mutex},
    time::Duration,
};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

/// Marker MIME type added to every source we create: an incoming offer carrying it is our
/// own re-set and must not be recorded again.
pub const OWN_MARKER: &str = "application/x-ashd-clipboard";
/// KDE's password-manager convention: `secret` means "do not record".
pub const SENSITIVE_HINT: &str = "x-kde-passwordManagerHint";

/// Which protocol the compositor gave us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// `ext-data-control-v1`
    Ext,
    /// `zwlr-data-control-unstable-v1`
    Wlr,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Ext => "ext-data-control-v1",
            Protocol::Wlr => "zwlr-data-control-unstable-v1",
        }
    }
}

type Mimes = Arc<Mutex<Vec<String>>>;

/// An offer of a selection (someone else's clipboard content).
#[derive(Clone, Debug)]
pub enum Offer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl Offer {
    /// MIME types the owner offers (complete once the `selection` event was delivered).
    pub fn mimes(&self) -> Vec<String> {
        let data: Option<&Mimes> = match self {
            Offer::Ext(o) => o.data::<Mimes>(),
            Offer::Wlr(o) => o.data::<Mimes>(),
        };
        data.map(|m| m.lock().map(|m| m.clone()).unwrap_or_default()).unwrap_or_default()
    }

    fn receive(&self, mime: &str, fd: BorrowedFd<'_>) {
        match self {
            Offer::Ext(o) => o.receive(mime.to_string(), fd),
            Offer::Wlr(o) => o.receive(mime.to_string(), fd),
        }
    }

    pub fn destroy(&self) {
        match self {
            Offer::Ext(o) => o.destroy(),
            Offer::Wlr(o) => o.destroy(),
        }
    }
}

/// Bytes we serve while we own the selection.
pub struct Served {
    pub reps: Vec<(String, Arc<Vec<u8>>)>,
}

#[derive(Clone)]
enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

#[derive(Clone)]
enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

/// Something the compositor told us.
pub enum Event {
    /// The regular clipboard changed (`None`: cleared).
    Selection(Option<Offer>),
    /// The primary selection changed.
    Primary(Option<Offer>),
    /// The compositor withdrew the device (seat gone).
    Finished,
}

#[derive(Default)]
struct State {
    events: Vec<Event>,
}

// ── registry / seat / manager: no events we care about ───────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ExtDataControlManagerV1, _: <ExtDataControlManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrDataControlManagerV1, _: <ZwlrDataControlManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// Write `data` to `fd` on a throw-away thread (a slow or stuck reader must never stall the
/// service thread); gives up after a few seconds without progress.
fn serve(data: Arc<Vec<u8>>, fd: OwnedFd) {
    let spawn = std::thread::Builder::new().name("ashd-svc-clipboard-send".into()).spawn(move || {
        let mut off = 0;
        while off < data.len() {
            let ready = {
                let mut fds = [PollFd::new(fd.as_fd(), PollFlags::POLLOUT)];
                poll(&mut fds, PollTimeout::try_from(Duration::from_secs(5)).unwrap_or(PollTimeout::NONE))
            };
            match ready {
                Ok(0) => return,
                Ok(_) => match nix::unistd::write(&fd, &data[off..]) {
                    Ok(0) => return,
                    Ok(n) => off += n,
                    Err(nix::errno::Errno::EINTR | nix::errno::Errno::EAGAIN) => {}
                    Err(_) => return,
                },
                Err(nix::errno::Errno::EINTR) => {}
                Err(_) => return,
            }
        }
    });
    if let Err(e) = spawn {
        tracing::warn!("cannot spawn clipboard writer: {e}");
    }
}

macro_rules! protocol_impl {
    ($variant:ident, $dev:ty, $dev_mod:ident, $src:ty, $src_mod:ident, $off:ty, $off_mod:ident) => {
        impl Dispatch<$dev, ()> for State {
            fn event(state: &mut Self, _: &$dev, event: $dev_mod::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
                match event {
                    // The offer object is delivered again by the selection event right after.
                    $dev_mod::Event::DataOffer { .. } => {}
                    $dev_mod::Event::Selection { id } => state.events.push(Event::Selection(id.map(Offer::$variant))),
                    $dev_mod::Event::PrimarySelection { id } => state.events.push(Event::Primary(id.map(Offer::$variant))),
                    $dev_mod::Event::Finished => state.events.push(Event::Finished),
                    _ => {}
                }
            }

            event_created_child!(State, $dev, [
                $dev_mod::EVT_DATA_OFFER_OPCODE => ($off, Mimes::default()),
            ]);
        }

        impl Dispatch<$off, Mimes> for State {
            fn event(_: &mut Self, _: &$off, event: $off_mod::Event, mimes: &Mimes, _: &Connection, _: &QueueHandle<Self>) {
                if let $off_mod::Event::Offer { mime_type } = event
                    && let Ok(mut m) = mimes.lock()
                {
                    m.push(mime_type);
                }
            }
        }

        impl Dispatch<$src, Arc<Served>> for State {
            fn event(_: &mut Self, source: &$src, event: $src_mod::Event, served: &Arc<Served>, _: &Connection, _: &QueueHandle<Self>) {
                match event {
                    $src_mod::Event::Send { mime_type, fd } => {
                        if let Some((_, data)) = served.reps.iter().find(|(m, _)| *m == mime_type) {
                            serve(data.clone(), fd);
                        }
                    }
                    $src_mod::Event::Cancelled => source.destroy(),
                    _ => {}
                }
            }
        }
    };
}

protocol_impl!(Ext, ExtDataControlDeviceV1, ext_data_control_device_v1, ExtDataControlSourceV1, ext_data_control_source_v1, ExtDataControlOfferV1, ext_data_control_offer_v1);
protocol_impl!(Wlr, ZwlrDataControlDeviceV1, zwlr_data_control_device_v1, ZwlrDataControlSourceV1, zwlr_data_control_source_v1, ZwlrDataControlOfferV1, zwlr_data_control_offer_v1);

/// A connected data-control client.
pub struct Wayland {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    manager: Manager,
    device: Device,
    protocol: Protocol,
}

fn bind_manager(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<(Manager, Protocol)> {
    if let Ok(m) = globals.bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ()) {
        return Some((Manager::Ext(m), Protocol::Ext));
    }
    // v2 adds the primary selection; v1 compositors still give us the clipboard.
    if let Ok(m) = globals.bind::<ZwlrDataControlManagerV1, _, _>(qh, 2..=2, ()) {
        return Some((Manager::Wlr(m), Protocol::Wlr));
    }
    if let Ok(m) = globals.bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=1, ()) {
        return Some((Manager::Wlr(m), Protocol::Wlr));
    }
    None
}

impl Wayland {
    /// Connect to `$WAYLAND_DISPLAY` and bind a data-control manager + the first seat.
    pub fn connect() -> Result<Wayland, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
        let qh = queue.handle();
        let (manager, protocol) = bind_manager(&globals, &qh)
            .ok_or_else(|| "the compositor offers neither ext-data-control-v1 nor zwlr-data-control".to_string())?;
        let seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ()).map_err(|e| format!("no wl_seat: {e}"))?;
        let device = match &manager {
            Manager::Ext(m) => Device::Ext(m.get_data_device(&seat, &qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(&seat, &qh, ())),
        };
        let mut wl = Wayland { conn, queue, qh, state: State::default(), manager, device, protocol };
        // Deliver the initial selection events.
        wl.queue.roundtrip(&mut wl.state).map_err(|e| format!("wayland roundtrip: {e}"))?;
        Ok(wl)
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Run the handlers of everything already read from the socket.
    pub fn dispatch_pending(&mut self) -> Result<(), String> {
        self.queue.dispatch_pending(&mut self.state).map(|_| ()).map_err(|e| format!("wayland dispatch: {e}"))
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.state.events)
    }

    /// Flush requests to the compositor (waits for a full socket buffer, bounded).
    pub fn flush(&self) -> Result<(), String> {
        for _ in 0..50 {
            match self.conn.flush() {
                Ok(()) => return Ok(()),
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(guard) = self.queue.prepare_read() {
                        let mut fds = [PollFd::new(guard.connection_fd(), PollFlags::POLLOUT)];
                        let _ = poll(&mut fds, PollTimeout::from(100u8));
                    }
                }
                Err(e) => return Err(format!("wayland flush: {e}")),
            }
        }
        Err("wayland flush: socket stayed full".into())
    }

    /// Block until the compositor sent something (read into the queue), `wake` became
    /// readable / hung up, or `timeout` passed. Returns `true` when `wake` fired.
    pub fn wait(&mut self, wake: Option<BorrowedFd<'_>>, timeout: PollTimeout) -> Result<bool, String> {
        let Some(guard) = self.queue.prepare_read() else { return Ok(false) };
        let mut fds = vec![PollFd::new(guard.connection_fd(), PollFlags::POLLIN)];
        fds.extend(wake.map(|w| PollFd::new(w, PollFlags::POLLIN)));
        match poll(&mut fds, timeout) {
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        let sock = fds[0].revents().unwrap_or(PollFlags::empty());
        let woke = fds.get(1).is_some_and(|f| f.revents().is_some_and(|r| !r.is_empty()));
        if sock.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("wayland read: {e}")),
            }
        } else {
            drop(guard);
        }
        Ok(woke)
    }

    /// Read one representation of `offer` through a pipe: at most `cap` bytes
    /// ([`Read::TooBig`] beyond), giving up after `idle` without data.
    pub fn read_offer(&self, offer: &Offer, mime: &str, cap: usize, idle: Duration) -> ReadResult {
        let (rd, wr) = match nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC) {
            Ok(p) => p,
            Err(e) => return ReadResult::Failed(format!("pipe: {e}")),
        };
        offer.receive(mime, wr.as_fd());
        drop(wr);
        if let Err(e) = self.flush() {
            return ReadResult::Failed(e);
        }
        let mut file = std::fs::File::from(rd);
        let mut data = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let timeout = PollTimeout::try_from(idle).unwrap_or(PollTimeout::NONE);
        loop {
            let ready = {
                let mut fds = [PollFd::new(file.as_fd(), PollFlags::POLLIN)];
                poll(&mut fds, timeout)
            };
            match ready {
                Ok(0) => return ReadResult::Failed("timed out waiting for the clipboard owner".into()),
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return ReadResult::Failed(format!("poll: {e}")),
            }
            match file.read(&mut buf) {
                Ok(0) => return ReadResult::Data(data),
                Ok(n) => {
                    if data.len() + n > cap {
                        return ReadResult::TooBig;
                    }
                    data.extend_from_slice(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return ReadResult::Failed(format!("read: {e}")),
            }
        }
    }

    /// Become the clipboard owner (`primary = false`) or primary owner serving `served`.
    /// The MIME types are offered in the given order, then the [`OWN_MARKER`].
    pub fn set_selection(&mut self, served: Served, primary: bool, marker: bool) {
        let mimes: Vec<String> = served.reps.iter().map(|(m, _)| m.clone()).collect();
        let served = Arc::new(served);
        match (&self.manager, &self.device) {
            (Manager::Ext(m), Device::Ext(d)) => {
                let src = m.create_data_source(&self.qh, served);
                for mime in &mimes {
                    src.offer(mime.clone());
                }
                if marker {
                    src.offer(OWN_MARKER.to_string());
                }
                if primary {
                    d.set_primary_selection(Some(&src));
                } else {
                    d.set_selection(Some(&src));
                }
            }
            (Manager::Wlr(m), Device::Wlr(d)) => {
                let src = m.create_data_source(&self.qh, served);
                for mime in &mimes {
                    src.offer(mime.clone());
                }
                if marker {
                    src.offer(OWN_MARKER.to_string());
                }
                if primary {
                    if d.version() >= 2 {
                        d.set_primary_selection(Some(&src));
                    }
                } else {
                    d.set_selection(Some(&src));
                }
            }
            _ => {}
        }
        let _ = self.flush();
    }
}

/// Outcome of [`Wayland::read_offer`].
#[derive(Debug, PartialEq, Eq)]
pub enum ReadResult {
    Data(Vec<u8>),
    /// More than the cap.
    TooBig,
    Failed(String),
}


#[cfg(test)]
impl Wayland {
    /// Dispatch events for about `ms` milliseconds (test clients that must serve `send`).
    pub fn pump(&mut self, ms: u64) -> Result<(), String> {
        let end = std::time::Instant::now() + Duration::from_millis(ms);
        loop {
            self.dispatch_pending()?;
            self.flush()?;
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            self.wait(None, PollTimeout::try_from(left.min(Duration::from_millis(50))).unwrap_or(PollTimeout::NONE))?;
        }
    }
}
