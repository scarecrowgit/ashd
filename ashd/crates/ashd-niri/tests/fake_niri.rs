//! ashd-niri against a fake niri speaking the real IPC protocol over a unix socket:
//! event stream, batching, reconnect with backoff, actions/requests, error paths and the
//! blocking helpers. No compositor needed.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use async_channel::Receiver;
use niri_ipc::{
    Action, Event, KeyboardLayouts, LogicalOutput, Output, Reply, Request, Response, Transform, Window, WindowLayout,
    Workspace,
};
use ashd_niri::{NiriError, NiriHandle, NiriState, blocking, start_with};
use smol::Timer;

// ── fake compositor ───────────────────────────────────────────────────────────

#[derive(Default)]
struct Inner {
    /// Sent to every new event-stream client right after the handshake.
    initial: Vec<Event>,
    windows: Vec<Window>,
    outputs: HashMap<String, Output>,
    actions: Vec<Action>,
    /// Window ids for which `CloseWindow` fails (already gone).
    close_fails: Vec<u64>,
    /// Answer nothing at all to requests other than EventStream.
    mute: bool,
    streams: Vec<UnixStream>,
    stream_clients: usize,
}

#[derive(Clone)]
struct FakeNiri {
    path: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

fn output(name: &str, w: u32, h: u32) -> Output {
    Output {
        name: name.into(),
        make: "Make".into(),
        model: "Model".into(),
        serial: None,
        physical_size: None,
        modes: vec![],
        current_mode: None,
        is_custom_mode: false,
        vrr_supported: false,
        vrr_enabled: false,
        logical: Some(LogicalOutput { x: 0, y: 0, width: w, height: h, scale: 1.0, transform: Transform::Normal }),
        max_bpc: None,
    }
}

fn workspace(id: u64, idx: u8, out: &str, active: bool, focused: bool) -> Workspace {
    Workspace {
        id,
        idx,
        name: None,
        output: Some(out.into()),
        is_urgent: false,
        is_active: active,
        is_focused: focused,
        active_window_id: None,
        mode: Default::default(),
        is_hidden: false,
    }
}

fn window(id: u64, app: Option<&str>, focused: bool) -> Window {
    Window {
        id,
        title: Some(format!("window {id}")),
        app_id: app.map(Into::into),
        pid: None,
        workspace_id: Some(1),
        is_focused: focused,
        is_floating: false,
        is_urgent: false,
        layout: WindowLayout {
            pos_in_scrolling_layout: Some((id as usize, 1)),
            tile_size: (500.0, 900.0),
            window_size: (496, 896),
            tile_pos_in_workspace_view: None,
            window_offset_in_tile: (0.0, 0.0),
            pos_in_canvas: None,
            is_pinned: false,
        },
        focus_timestamp: None,
        hidden_from_workspace_id: None,
    }
}

impl FakeNiri {
    fn start(path: &Path) -> Self {
        let fake = Self { path: path.to_path_buf(), inner: Arc::default() };
        let listener = UnixListener::bind(path).unwrap();
        let inner = fake.inner.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { break };
                let inner = inner.clone();
                thread::spawn(move || serve(conn, inner));
            }
        });
        fake
    }

    fn with(&self, f: impl FnOnce(&mut Inner)) {
        f(&mut self.inner.lock().unwrap());
    }

    /// Push one event to every connected event stream.
    fn emit(&self, event: &Event) {
        self.emit_raw(&serde_json::to_string(event).unwrap());
    }

    fn emit_raw(&self, line: &str) {
        for s in &mut self.inner.lock().unwrap().streams {
            writeln!(s, "{line}").unwrap();
        }
    }

    /// Close every event stream, as a niri restart would.
    fn drop_streams(&self) {
        let streams = std::mem::take(&mut self.inner.lock().unwrap().streams);
        for s in streams {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    fn actions(&self) -> Vec<Action> {
        self.inner.lock().unwrap().actions.clone()
    }
}

fn reply(conn: &mut UnixStream, r: Reply) {
    let _ = writeln!(conn, "{}", serde_json::to_string(&r).unwrap());
}

fn serve(conn: UnixStream, inner: Arc<Mutex<Inner>>) {
    let mut writer = conn.try_clone().unwrap();
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let request: Request = serde_json::from_str(&line).expect("ashd sent a request niri cannot parse");
        line.clear();
        let mut g = inner.lock().unwrap();
        if let Request::EventStream = request {
            reply(&mut writer, Ok(Response::Handled));
            g.stream_clients += 1;
            for ev in g.initial.clone() {
                writeln!(writer, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
            }
            g.streams.push(writer);
            return;
        }
        if g.mute {
            continue;
        }
        let answer = match request {
            Request::Outputs => Ok(Response::Outputs(g.outputs.clone())),
            Request::Windows => Ok(Response::Windows(g.windows.clone())),
            Request::FocusedWindow => Ok(Response::FocusedWindow(g.windows.iter().find(|w| w.is_focused).cloned())),
            Request::Action(Action::CloseWindow { id: Some(id) }) if g.close_fails.contains(&id) => {
                Err(format!("window {id} does not exist"))
            }
            Request::Action(action) => {
                g.actions.push(action);
                Ok(Response::Handled)
            }
            Request::ReturnError => Err("this is an error".into()),
            other => Err(format!("fake niri does not know {other:?}")),
        };
        drop(g);
        reply(&mut writer, answer);
    }
}

fn dump() -> Vec<Event> {
    vec![
        Event::WorkspacesChanged {
            workspaces: vec![workspace(1, 1, "DP-1", true, true), workspace(2, 2, "DP-1", false, false)],
        },
        Event::WindowsChanged { windows: vec![window(1, Some("kitty"), true), window(2, Some("zen"), false)] },
        Event::KeyboardLayoutsChanged {
            keyboard_layouts: KeyboardLayouts { names: vec!["us".into(), "ru".into()], current_idx: 0 },
        },
        Event::OverviewOpenedOrClosed { is_open: false },
        Event::ConfigLoaded { failed: false },
    ]
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Wait until the newest published state satisfies `pred`.
fn wait_for(rx: &Receiver<NiriState>, what: &str, pred: impl Fn(&NiriState) -> bool) -> NiriState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let next = smol::block_on(smol::future::or(async { rx.recv().await.ok() }, async {
            Timer::after(left).await;
            None
        }));
        match next {
            Some(state) if pred(&state) => return state,
            Some(_) => {}
            None => panic!("timed out waiting for: {what}"),
        }
    }
}

fn block<T>(f: impl std::future::Future<Output = T>) -> T {
    smol::block_on(f)
}

fn setup() -> (tempfile::TempDir, FakeNiri) {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeNiri::start(&dir.path().join("niri.sock"));
    fake.with(|g| {
        g.initial = dump();
        g.outputs.insert("DP-1".into(), output("DP-1", 1920, 1080));
    });
    (dir, fake)
}

// ── event stream ──────────────────────────────────────────────────────────────

#[test]
fn follows_the_event_stream() {
    let (_dir, fake) = setup();
    let (_handle, rx) = start_with(Some(fake.path.clone()));

    // The very first snapshot is "not connected yet".
    let first = rx.recv_blocking().unwrap();
    assert!(!first.connected && first.windows.is_empty());

    // Then the whole initial dump arrives in one connected snapshot, outputs included.
    let s = wait_for(&rx, "initial dump", |s| s.connected);
    assert_eq!(s.workspaces.len(), 2);
    assert_eq!(s.windows.len(), 2, "never 'connected but empty'");
    assert_eq!(s.focused_window().map(|w| w.id), Some(1));
    assert_eq!(s.current_layout(), Some("us"));
    assert_eq!(s.outputs.keys().collect::<Vec<_>>(), vec!["DP-1"]);
    assert_eq!(s.outputs["DP-1"].logical.as_ref().map(|l| l.width), Some(1920));
    assert_eq!(s.strip(1).columns.len(), 2);

    // Live events.
    fake.emit(&Event::WindowOpenedOrChanged { window: window(3, Some("kitty"), true) });
    let s = wait_for(&rx, "window 3", |s| s.windows.contains_key(&3));
    assert_eq!(s.focused_window().map(|w| w.id), Some(3));
    assert!(s.connected);

    // Junk and events from the future are skipped without dropping the stream.
    fake.emit_raw(r#"{"EventFromTheFuture":{"x":1}}"#);
    fake.emit_raw("this is not json");
    fake.emit_raw("");
    fake.emit(&Event::OverviewOpenedOrClosed { is_open: true });
    let s = wait_for(&rx, "overview", |s| s.overview_open);
    assert!(s.connected && s.windows.len() == 3);

    // Events about unknown subjects must not kill the thread (niri-ipc would panic).
    fake.emit(&Event::WindowClosed { id: 4242 });
    fake.emit(&Event::WorkspaceActivated { id: 4242, focused: true });
    fake.emit(&Event::CastStopped { stream_id: 7 });
    fake.emit(&Event::WindowClosed { id: 3 });
    let s = wait_for(&rx, "window 3 closed", |s| !s.windows.contains_key(&3));
    assert!(s.connected);
    assert_eq!(s.windows.len(), 2);

    // A new workspace set refreshes the outputs.
    fake.with(|g| {
        g.outputs.insert("HDMI-A-1".into(), output("HDMI-A-1", 2560, 1440));
    });
    fake.emit(&Event::WorkspacesChanged { workspaces: vec![workspace(1, 1, "DP-1", true, true)] });
    let s = wait_for(&rx, "outputs refresh", |s| s.outputs.contains_key("HDMI-A-1"));
    assert_eq!(s.workspaces.len(), 1);
}

#[test]
fn reconnects_after_niri_goes_away_and_rebuilds_state() {
    let (_dir, fake) = setup();
    let (_handle, rx) = start_with(Some(fake.path.clone()));
    wait_for(&rx, "connected", |s| s.connected && s.windows.len() == 2);

    // niri restarts with a different session.
    fake.with(|g| {
        g.initial = vec![
            Event::WorkspacesChanged { workspaces: vec![workspace(9, 1, "DP-1", true, true)] },
            Event::WindowsChanged { windows: vec![window(77, Some("foot"), true)] },
        ];
    });
    fake.drop_streams();
    let s = wait_for(&rx, "disconnected", |s| !s.connected);
    assert_eq!(s.windows.len(), 2, "the last known state stays readable while disconnected");

    let started = Instant::now();
    let s = wait_for(&rx, "reconnected", |s| s.connected);
    assert!(started.elapsed() < Duration::from_secs(3), "first retry is quick");
    assert_eq!(s.windows.keys().copied().collect::<Vec<_>>(), vec![77], "no stale windows after reconnect");
    assert_eq!(s.workspaces.keys().copied().collect::<Vec<_>>(), vec![9]);
}

#[test]
fn connects_when_niri_starts_later() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("late.sock");
    let (handle, rx) = start_with(Some(path.clone()));
    let s = rx.recv_blocking().unwrap();
    assert!(!s.connected);
    // Requests fail cleanly while niri is down.
    assert!(matches!(block(handle.request(Request::Version)), Err(NiriError::Io(_))));

    thread::sleep(Duration::from_millis(400));
    let fake = FakeNiri::start(&path);
    fake.with(|g| g.initial = dump());
    let s = wait_for(&rx, "connected after late start", |s| s.connected);
    assert_eq!(s.windows.len(), 2);
    // ... and so do requests.
    assert!(block(handle.action(Action::FocusWorkspaceUp {})).is_ok());
}

#[test]
fn a_niri_that_never_answers_the_handshake_does_not_wedge_anything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mute.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let mut keep = Vec::new();
        for c in listener.incoming().flatten() {
            keep.push(c); // accept, never answer
        }
    });
    let (_handle, rx) = start_with(Some(path));
    let s = rx.recv_blocking().unwrap();
    assert!(!s.connected);
    thread::sleep(Duration::from_millis(300));
    assert!(rx.try_recv().is_err(), "still waiting, no bogus state");
}

#[test]
fn no_socket_means_permanently_disconnected_but_alive() {
    let (handle, rx) = start_with(None);
    let s = rx.recv_blocking().unwrap();
    assert!(!s.connected && s.workspaces.is_empty());
    assert!(matches!(block(handle.action(Action::FocusWorkspaceUp {})), Err(NiriError::NoSocket)));
    assert!(matches!(block(handle.request(Request::Version)), Err(NiriError::NoSocket)));
    handle.send_action(Action::FocusWorkspaceDown {}); // logged, no panic
}

#[test]
fn disconnected_handle_reports_gone() {
    let h = NiriHandle::disconnected();
    assert!(matches!(block(h.action(Action::FocusWorkspaceUp {})), Err(NiriError::Gone)));
    assert!(matches!(block(h.request(Request::Outputs)), Err(NiriError::Gone)));
    h.send_action(Action::FocusWorkspaceUp {});
}

// ── actions and requests ──────────────────────────────────────────────────────

#[test]
fn actions_and_requests_reach_niri() {
    let (_dir, fake) = setup();
    let (handle, _rx) = start_with(Some(fake.path.clone()));

    block(handle.action(Action::FocusWorkspaceUp {})).unwrap();
    handle.send_action(ashd_niri::spawn_action(["kitty", "--single-instance"]));
    handle.send_action(ashd_niri::spawn_sh_action("echo hi"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while fake.actions().len() < 3 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let mut got: Vec<String> = fake.actions().iter().map(|a| format!("{a:?}")).collect();
    // Fire-and-forget actions run on their own connections: only the awaited first one
    // is ordered.
    got[1..].sort();
    let mut want: Vec<String> = [
        Action::FocusWorkspaceUp {},
        Action::Spawn { command: vec!["kitty".into(), "--single-instance".into()] },
        Action::SpawnSh { command: "echo hi".into() },
    ]
    .iter()
    .map(|a| format!("{a:?}"))
    .collect();
    want[1..].sort();
    assert_eq!(got, want);

    match block(handle.request(Request::Outputs)).unwrap() {
        Response::Outputs(o) => assert!(o.contains_key("DP-1")),
        other => panic!("{other:?}"),
    }
    // niri's own error text is passed through.
    match block(handle.request(Request::ReturnError)) {
        Err(NiriError::Niri(m)) => assert_eq!(m, "this is an error"),
        other => panic!("{other:?}"),
    }
    // `action` insists on Handled.
    assert!(matches!(
        block(handle.action(Action::CloseWindow { id: Some(1) })),
        Ok(())
    ));
    fake.with(|g| g.close_fails.push(5));
    assert!(matches!(block(handle.action(Action::CloseWindow { id: Some(5) })), Err(NiriError::Niri(_))));
}

#[test]
fn concurrent_requests_do_not_block_each_other() {
    let (_dir, fake) = setup();
    let (handle, _rx) = start_with(Some(fake.path.clone()));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let h = handle.clone();
            thread::spawn(move || {
                for _ in 0..20 {
                    block(h.action(Action::FocusWorkspaceUp {})).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(fake.actions().len(), 160);
}

#[test]
fn a_hung_niri_makes_requests_time_out() {
    let (_dir, fake) = setup();
    fake.with(|g| g.mute = true);
    let (handle, _rx) = start_with(Some(fake.path.clone()));
    let started = Instant::now();
    let err = block(handle.request(Request::Outputs)).unwrap_err();
    assert!(matches!(&err, NiriError::Io(e) if e.kind() == std::io::ErrorKind::TimedOut), "{err:?}");
    assert!(started.elapsed() >= Duration::from_secs(4) && started.elapsed() < Duration::from_secs(8));
}

// ── blocking helpers ──────────────────────────────────────────────────────────

fn closed_ids(fake: &FakeNiri) -> Vec<u64> {
    fake.actions()
        .into_iter()
        .map(|a| match a {
            Action::CloseWindow { id: Some(id) } => id,
            other => panic!("unexpected action {other:?}"),
        })
        .collect()
}

#[test]
fn app_quit_closes_every_window_of_the_focused_app_focused_one_last() {
    let (_dir, fake) = setup();
    fake.with(|g| {
        g.windows = vec![
            window(1, Some("kitty"), false),
            window(2, Some("kitty"), true),
            window(3, Some("zen"), false),
            window(4, Some("kitty"), false),
            window(5, None, false),
            window(6, Some("Kitty"), false),
        ];
    });
    let mut conn = blocking::Connection::connect_to(&fake.path).unwrap();
    assert_eq!(blocking::close_focused_app_windows_on(&mut conn).unwrap(), 3);
    assert_eq!(closed_ids(&fake), vec![1, 4, 2]);
}

#[test]
fn app_quit_with_nothing_focused_does_nothing() {
    let (_dir, fake) = setup();
    fake.with(|g| g.windows = vec![window(1, Some("kitty"), false)]);
    let mut conn = blocking::Connection::connect_to(&fake.path).unwrap();
    assert_eq!(blocking::close_focused_app_windows_on(&mut conn).unwrap(), 0);
    assert!(fake.actions().is_empty());
}

#[test]
fn app_quit_on_a_window_without_app_id_closes_only_that_window() {
    let (_dir, fake) = setup();
    fake.with(|g| g.windows = vec![window(1, None, true), window(2, None, false), window(3, Some(""), false)]);
    let mut conn = blocking::Connection::connect_to(&fake.path).unwrap();
    assert_eq!(blocking::close_focused_app_windows_on(&mut conn).unwrap(), 1);
    assert_eq!(closed_ids(&fake), vec![1]);

    // Empty app id counts as "no app id" too.
    fake.with(|g| {
        g.actions.clear();
        g.windows = vec![window(3, Some(""), true), window(4, Some(""), false)];
    });
    assert_eq!(blocking::close_focused_app_windows_on(&mut conn).unwrap(), 1);
    assert_eq!(closed_ids(&fake), vec![3]);
}

#[test]
fn app_quit_skips_windows_that_vanished() {
    let (_dir, fake) = setup();
    fake.with(|g| {
        g.windows = vec![window(1, Some("kitty"), true), window(2, Some("kitty"), false), window(3, Some("kitty"), false)];
        g.close_fails = vec![2];
    });
    let mut conn = blocking::Connection::connect_to(&fake.path).unwrap();
    assert_eq!(blocking::close_focused_app_windows_on(&mut conn).unwrap(), 2);
    assert_eq!(closed_ids(&fake), vec![3, 1]);
}

#[test]
fn blocking_connection_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(blocking::Connection::connect_to(dir.path().join("none.sock")), Err(NiriError::Io(_))));

    let (_dir, fake) = setup();
    let mut conn = blocking::Connection::connect_to(&fake.path).unwrap();
    assert!(matches!(conn.request(&Request::ReturnError), Err(NiriError::Niri(m)) if m == "this is an error"));
    // The connection is reusable after an error reply.
    assert!(matches!(conn.request(&Request::Outputs), Ok(Response::Outputs(_))));
    assert!(conn.action(Action::FocusWorkspaceUp {}).is_ok());

    // niri hangs up.
    let path = dir.path().join("hangup.sock");
    let l = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for c in l.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(c).read_line(&mut line);
        }
    });
    let mut conn = blocking::Connection::connect_to(&path).unwrap();
    assert!(matches!(conn.request(&Request::Version), Err(NiriError::Gone)));
}

// ── canvas / strip views (ashd fork) ─────────────────────────────────────────

fn canvas_view(ws: u64, x: f64, zoom: f64, animating: bool) -> niri_ipc::CanvasViewState {
    niri_ipc::CanvasViewState {
        workspace_id: ws,
        offset: (x, 0.0),
        zoom,
        target_offset: (x, 0.0),
        target_zoom: zoom,
        view_size: (1920.0, 1080.0),
        animating,
        regions: vec![],
    }
}

/// Wait until the newest live-view snapshot satisfies `pred`.
fn wait_views(
    rx: &Receiver<Arc<ashd_niri::LiveViews>>,
    what: &str,
    pred: impl Fn(&ashd_niri::LiveViews) -> bool,
) -> Arc<ashd_niri::LiveViews> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let next = smol::block_on(smol::future::or(async { rx.recv().await.ok() }, async {
            Timer::after(left).await;
            None
        }));
        match next {
            Some(v) if pred(&v) => return v,
            Some(_) => {}
            None => panic!("timed out waiting for: {what}"),
        }
    }
}

#[test]
fn view_events_reach_view_subscribers_only_and_are_coalesced() {
    let (_dir, fake) = setup();
    let (handle, rx) = start_with(Some(fake.path.clone()));
    let views = handle.subscribe_views();
    let other = handle.subscribe_views();
    wait_for(&rx, "connected", |s| s.connected && s.windows.len() == 2);

    // Mode change: a structural event (main state) followed by the new view sets.
    fake.emit(&Event::WorkspaceModeChanged { id: 1, mode: niri_ipc::WorkspaceMode::Canvas });
    fake.emit(&Event::CanvasViewsChanged { views: vec![canvas_view(1, 0.0, 1.0, true)] });
    fake.emit(&Event::StripViewsChanged { views: vec![] });
    let s = wait_for(&rx, "mode change", |s| s.is_canvas(1));
    assert!(!s.is_canvas(2));
    wait_views(&views, "canvas views", |v| v.canvas(1).is_some());
    while rx.try_recv().is_ok() {}

    // A burst of per-frame view events: no NiriState publish, a few view publishes, the
    // last one being the settled view.
    for i in 0..200 {
        fake.emit(&Event::CanvasViewChanged { view: canvas_view(1, i as f64, 0.62, i < 199) });
    }
    let last = wait_views(&views, "settled", |v| v.canvas(1).is_some_and(|c| !c.animating));
    assert_eq!(last.canvas(1).map(|c| c.offset.0), Some(199.0));
    // Every subscriber sees the newest value.
    wait_views(&other, "other subscriber settled", |v| v.canvas(1).is_some_and(|c| !c.animating));
    thread::sleep(Duration::from_millis(50));
    assert!(rx.try_recv().is_err(), "per-frame view events must not republish NiriState");

    // A structural event republishes the state, which carries the current views too.
    fake.emit(&Event::WorkspaceUrgencyChanged { id: 2, urgent: true });
    let s = wait_for(&rx, "urgency", |s| s.workspaces[&2].is_urgent);
    assert_eq!(s.canvas_view(1).map(|c| c.offset.0), Some(199.0));

    // Strip views behave the same.
    fake.emit(&Event::StripViewChanged {
        workspace_id: 2,
        view_offset: 10.0,
        target_view_offset: 20.0,
        working_area_width: 1500.0,
        column_xs: vec![0.0, 512.0],
    });
    let v = wait_views(&views, "strip view", |v| v.strip(2).is_some());
    assert_eq!(v.strip(2).map(|s| s.column_xs.len()), Some(2));
    // A subscriber that joins late gets the current value right away.
    let late = handle.subscribe_views();
    assert!(late.recv_blocking().unwrap().strip(2).is_some());
}

#[test]
fn old_compositor_without_view_events_stays_in_strip_mode() {
    let (_dir, fake) = setup();
    // An old niri: workspaces and windows without the fork's fields.
    fake.with(|g| {
        g.initial = vec![];
    });
    let (handle, rx) = start_with(Some(fake.path.clone()));
    let views = handle.subscribe_views();
    let deadline = Instant::now() + Duration::from_secs(10);
    while fake.inner.lock().unwrap().streams.is_empty() {
        assert!(Instant::now() < deadline, "no event stream client");
        thread::sleep(Duration::from_millis(10));
    }
    fake.emit_raw(r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"name":null,"output":"winit","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null}]}}"#);
    fake.emit_raw(r#"{"WindowsChanged":{"windows":[{"id":1,"title":"~","app_id":"kitty","pid":4242,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1],"tile_size":[960.0,1080.0],"window_size":[956,1076],"tile_pos_in_workspace_view":[0.0,0.0],"window_offset_in_tile":[2.0,2.0]},"focus_timestamp":null}]}}"#);
    let s = wait_for(&rx, "windows", |s| s.connected && s.windows.len() == 1);
    assert!(!s.is_canvas(1));
    assert!(s.canvas_windows(1).is_empty());
    assert_eq!(s.canvas_bounds(1), None);
    assert_eq!(s.workspaces_on("winit").len(), 1);
    assert!(views.recv_blocking().unwrap().canvas.is_empty());
}
