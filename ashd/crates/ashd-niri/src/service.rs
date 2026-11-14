use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use futures_lite::{AsyncBufReadExt, AsyncWriteExt, Future, future, io::BufReader};
use niri_ipc::{Action, Event, Reply, Request, Response};
use smol::{LocalExecutor, Timer, net::unix::UnixStream};

use crate::{LiveViews, NiriError, NiriState, SOCKET_ENV};

/// Live view snapshots ([`NiriHandle::subscribe_views`]) are published at most this often
/// (one 240 Hz frame). niri already limits view events to one per output frame; this
/// also folds together the events of several outputs animating at once.
pub const VIEWS_MIN_INTERVAL: Duration = Duration::from_micros(4167);

/// Events that only move a view (one per output frame while it animates). They update
/// the live views and do not republish [`NiriState`].
pub fn is_live_view_event(event: &Event) -> bool {
    matches!(event, Event::CanvasViewChanged { .. } | Event::StripViewChanged { .. })
}

/// Broadcast of the latest [`LiveViews`] to any number of subscribers (each gets a
/// bounded(1) channel fed with `force_send`: a slow subscriber only sees the newest).
#[derive(Default)]
struct ViewsHub {
    inner: Mutex<HubInner>,
}

#[derive(Default)]
struct HubInner {
    latest: Arc<LiveViews>,
    subs: Vec<Sender<Arc<LiveViews>>>,
}

impl ViewsHub {
    fn lock(&self) -> std::sync::MutexGuard<'_, HubInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn subscribe(&self) -> Receiver<Arc<LiveViews>> {
        let (tx, rx) = async_channel::bounded(1);
        let mut inner = self.lock();
        let _ = tx.force_send(inner.latest.clone());
        inner.subs.push(tx);
        rx
    }

    /// Hand `views` to every live subscriber; unchanged values are dropped.
    fn publish(&self, views: LiveViews) {
        let mut inner = self.lock();
        if *inner.latest == views {
            return;
        }
        inner.latest = Arc::new(views);
        let latest = inner.latest.clone();
        inner.subs.retain(|tx| tx.force_send(latest.clone()).is_ok());
    }
}

/// A single request/answer exchange (connect, write, read) must finish within this.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// First and longest pause between reconnect attempts (doubling in between).
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// A connection that lived this long counts as healthy: the next retry starts fast again.
const HEALTHY_AFTER: Duration = Duration::from_secs(2);
/// Read buffer of the event stream: the initial dump (all windows) arrives as one batch.
const EVENT_BUF: usize = 256 * 1024;
/// How long after the first events of a connection we wait for the rest of the initial dump.
const SETTLE: Duration = Duration::from_millis(40);

struct Cmd {
    request: Request,
    /// `None` = fire-and-forget: errors are logged by the ashd-niri thread.
    reply: Option<Sender<Result<Response, NiriError>>>,
}

/// Cheap, cloneable handle to the `ashd-niri` thread.
#[derive(Clone)]
pub struct NiriHandle {
    tx: Sender<Cmd>,
    views: Arc<ViewsHub>,
}

impl NiriHandle {
    /// Live canvas/strip view state. The receiver yields the current value right away,
    /// then a new snapshot whenever a view changed (at most once per
    /// [`VIEWS_MIN_INTERVAL`], newest only). Each call is an independent subscription;
    /// drop the receiver to unsubscribe. Only panels that draw view positions should
    /// subscribe — everything else reads modes and windows from [`NiriState`].
    pub fn subscribe_views(&self) -> Receiver<Arc<LiveViews>> {
        self.views.subscribe()
    }

    /// Fire-and-forget action (errors are logged). Use from GPUI listeners.
    pub fn send_action(&self, action: Action) {
        let name = format!("{action:?}");
        if self.tx.try_send(Cmd { request: Request::Action(action), reply: None }).is_err() {
            tracing::warn!("niri action {name}: ashd-niri thread is gone");
        }
    }

    /// Run an action and wait for niri's answer.
    pub async fn action(&self, action: Action) -> Result<(), NiriError> {
        match self.request(Request::Action(action)).await? {
            Response::Handled => Ok(()),
            other => Err(NiriError::Unexpected(Box::new(other))),
        }
    }

    /// Any niri request (`Request::Outputs`, `Request::FocusedOutput`, …).
    ///
    /// Do not send `Request::EventStream` here: the answer is only the acknowledgement
    /// and the connection is closed. The event stream is [`start`]'s job.
    pub async fn request(&self, request: Request) -> Result<Response, NiriError> {
        let (tx, rx) = async_channel::bounded(1);
        self.tx.send(Cmd { request, reply: Some(tx) }).await.map_err(|_| NiriError::Gone)?;
        rx.recv().await.map_err(|_| NiriError::Gone)?
    }

    /// A handle whose thread does not exist: every call fails with `NiriError::Gone`.
    /// For tests and for running panels without a compositor.
    pub fn disconnected() -> Self {
        let (tx, _rx) = async_channel::bounded(1);
        Self { tx, views: Arc::default() }
    }
}

/// Start following niri at `$NIRI_SOCKET`. Never fails: without a socket (or while niri
/// is down) the published state has `connected == false` and the thread keeps retrying
/// (250 ms doubling to 5 s) — niri may be started later or restarted.
///
/// The receiver yields the latest full [`NiriState`] (bounded(1) + `force_send`: a
/// slow consumer only ever sees the newest snapshot). A snapshot is published per event
/// *batch*, not per event: the initial dump of a busy session arrives as one snapshot.
pub fn start() -> (NiriHandle, Receiver<NiriState>) {
    start_with(std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty()).map(PathBuf::from))
}

/// [`start`] with an explicit socket path (tests; `None` = no compositor).
pub fn start_with(socket: Option<PathBuf>) -> (NiriHandle, Receiver<NiriState>) {
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
    let (state_tx, state_rx) = async_channel::bounded::<NiriState>(1);
    let hub = Arc::new(ViewsHub::default());
    let thread_hub = hub.clone();
    let spawned = std::thread::Builder::new().name("ashd-niri".into()).spawn(move || {
        let ex = Rc::new(LocalExecutor::new());
        let (views_tx, views_rx) = async_channel::bounded::<LiveViews>(1);
        let main = future::zip(
            command_loop(ex.clone(), socket.clone(), cmd_rx),
            future::zip(event_loop(socket, state_tx, views_tx), views_loop(views_rx, thread_hub)),
        );
        smol::block_on(ex.run(main));
    });
    if let Err(e) = spawned {
        // Out of threads: behave like a compositor-less session (the closure, and with it
        // the channels, was dropped).
        tracing::error!("cannot spawn the ashd-niri thread: {e}");
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.force_send(NiriState::default());
        return (NiriHandle::disconnected(), rx);
    }
    (NiriHandle { tx: cmd_tx, views: hub }, state_rx)
}

/// Forward live view snapshots to the hub, at most one per [`VIEWS_MIN_INTERVAL`]. The
/// event loop `force_send`s into a bounded(1) channel, so after the pause the newest
/// snapshot (e.g. the settled `animating: false` one) is always the one published.
async fn views_loop(rx: Receiver<LiveViews>, hub: Arc<ViewsHub>) {
    while let Ok(views) = rx.recv().await {
        hub.publish(views);
        Timer::after(VIEWS_MIN_INTERVAL).await;
    }
}

/// Fail with `TimedOut` if `fut` takes longer than `dur`.
async fn timeout<T>(dur: Duration, fut: impl Future<Output = Result<T, NiriError>>) -> Result<T, NiriError> {
    future::or(fut, async {
        Timer::after(dur).await;
        Err(NiriError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "niri did not answer in time")))
    })
    .await
}

async fn command_loop(ex: Rc<LocalExecutor<'static>>, socket: Option<PathBuf>, rx: Receiver<Cmd>) {
    while let Ok(Cmd { request, reply }) = rx.recv().await {
        let socket = socket.clone();
        // One task per command: a slow request must not hold up the others.
        ex.spawn(async move {
            let res = match &socket {
                Some(path) => oneshot(path, &request).await,
                None => Err(NiriError::NoSocket),
            };
            match reply {
                Some(reply) => {
                    let _ = reply.try_send(res);
                }
                None => match res {
                    Ok(Response::Handled) => {}
                    Ok(other) => tracing::warn!("niri {request:?}: unexpected answer {other:?}"),
                    Err(e) => tracing::warn!("niri {request:?}: {e}"),
                },
            }
        })
        .detach();
    }
}

/// One request on a fresh connection, bounded by [`REQUEST_TIMEOUT`].
pub(crate) async fn oneshot(path: &Path, req: &Request) -> Result<Response, NiriError> {
    timeout(REQUEST_TIMEOUT, async {
        let stream = UnixStream::connect(path).await?;
        let mut writer = stream.clone();
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        let mut buf = String::new();
        if BufReader::new(stream).read_line(&mut buf).await? == 0 {
            return Err(NiriError::Gone);
        }
        let reply: Reply = serde_json::from_str(&buf)?;
        reply.map_err(NiriError::Niri)
    })
    .await
}

async fn refresh_outputs(path: &Path, state: &mut NiriState) {
    match oneshot(path, &Request::Outputs).await {
        Ok(Response::Outputs(outputs)) => state.outputs = outputs.into_iter().collect(),
        Ok(other) => tracing::warn!("niri outputs: unexpected {other:?}"),
        Err(e) => tracing::warn!("niri outputs: {e}"),
    }
}

/// The two outputs of the event loop: full state snapshots and live view snapshots.
struct Publisher {
    state: Sender<NiriState>,
    views: Sender<LiveViews>,
}

impl Publisher {
    fn is_closed(&self) -> bool {
        self.state.is_closed()
    }
}

/// Publish a snapshot. `false` once the consumer is gone.
fn publish(tx: &Publisher, state: &NiriState) -> bool {
    // force_send replaces the queued snapshot if the consumer has not taken it yet.
    tx.state.force_send(state.clone()).is_ok()
}

/// Queue a live view snapshot for [`views_loop`] (never blocks; newest wins).
fn publish_views(tx: &Publisher, state: &NiriState) {
    let _ = tx.views.force_send(live_views(state));
}

fn live_views(state: &NiriState) -> LiveViews {
    LiveViews { canvas: state.canvas_views.clone(), strip: state.strip_views.clone() }
}

async fn event_loop(socket: Option<PathBuf>, tx: Sender<NiriState>, views_tx: Sender<LiveViews>) {
    let tx = Publisher { state: tx, views: views_tx };
    let mut state = NiriState::default();
    if !publish(&tx, &state) {
        return;
    }
    let Some(path) = socket else {
        tracing::warn!("{SOCKET_ENV} is not set: niri state stays disconnected");
        return;
    };
    let mut backoff = BACKOFF_MIN;
    let mut warned = false;
    loop {
        let started = Instant::now();
        match follow(&path, &mut state, &tx).await {
            Ok(()) => tracing::info!("niri event stream closed"),
            Err(e) if !warned || started.elapsed() > HEALTHY_AFTER => tracing::warn!("niri event stream: {e}"),
            Err(e) => tracing::debug!("niri event stream: {e}"),
        }
        warned = true;
        if tx.is_closed() {
            return;
        }
        if state.connected {
            state.connected = false;
            if !publish(&tx, &state) {
                return;
            }
        }
        if started.elapsed() > HEALTHY_AFTER {
            backoff = BACKOFF_MIN;
            warned = false;
        }
        Timer::after(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Follow one event-stream connection until it ends. `Ok(())` = closed by niri or by the
/// consumer going away.
async fn follow(path: &Path, state: &mut NiriState, tx: &Publisher) -> Result<(), NiriError> {
    let stream = timeout(REQUEST_TIMEOUT, async { Ok(UnixStream::connect(path).await?) }).await?;
    let mut writer = stream.clone();
    let mut reader = BufReader::with_capacity(EVENT_BUF, stream);
    let mut line = serde_json::to_string(&Request::EventStream)?;
    line.push('\n');
    timeout(REQUEST_TIMEOUT, async {
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    })
    .await?;

    line.clear();
    let n = timeout(REQUEST_TIMEOUT, async { Ok(reader.read_line(&mut line).await?) }).await?;
    if n == 0 {
        return Err(NiriError::Gone);
    }
    let reply: Reply = serde_json::from_str(&line)?;
    reply.map_err(NiriError::Niri)?;

    // From here on `state` is rebuilt from niri's initial dump. Nothing is published until
    // that dump (one batch) is in, so consumers never see "connected but empty".
    *state = NiriState { connected: true, ..NiriState::default() };
    let (mut dirty, mut views_dirty, mut refresh, mut first) = (false, false, false, true);
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        match serde_json::from_str::<Event>(&line) {
            Ok(event) => {
                let live = is_live_view_event(&event);
                let views = live || matches!(event, Event::CanvasViewsChanged { .. } | Event::StripViewsChanged { .. });
                // Belt and braces: `apply` guards the known panics of niri-ipc's reducers.
                match catch_unwind(AssertUnwindSafe(|| state.apply(event))) {
                    Ok(outputs) => refresh |= outputs,
                    Err(_) => return Err(NiriError::Niri("the state reducer panicked; reconnecting".into())),
                }
                // Per-frame view events only reach the live-view subscribers.
                dirty |= !live;
                views_dirty |= views;
            }
            // A newer niri may send events this niri-ipc copy does not know yet.
            Err(e) => tracing::debug!("niri: skipping unknown event ({e}): {}", line.trim_end()),
        }
        // Batch boundary: nothing more buffered.
        if reader.buffer().is_empty() {
            // The initial dump is written event by event: wait a moment for the rest of it
            // so the first snapshot is complete. (`fill_buf` only waits, it loses no data.)
            if first {
                let more = future::or(async { reader.fill_buf().await.map(|b| !b.is_empty()).ok() }, async {
                    Timer::after(SETTLE).await;
                    Some(false)
                })
                .await;
                match more {
                    Some(true) => continue,
                    None => return Ok(()), // read error: let the next read_line report it
                    Some(false) => {}
                }
            }
            if refresh {
                refresh_outputs(path, state).await;
                refresh = false;
                dirty = true;
            }
            if views_dirty || first {
                publish_views(tx, state);
            }
            if (dirty || first) && !publish(tx, state) {
                return Ok(());
            }
            dirty = false;
            views_dirty = false;
            first = false;
        }
    }
}
