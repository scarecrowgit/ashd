//! Server and client over real unix sockets: framing, error paths, stale sockets,
//! permissions, limits.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use ashd_ipc::{
    Client, Incoming, IpcError, MAX_LINE, OsdRequest, PROTOCOL_VERSION, PanelOp, Request, Response, Server,
    ThemeRequest, VolumeChange,
};
use ashd_tokens::ThemeName;

fn pong() -> Response {
    Response::Pong { version: "t".into(), protocol: PROTOCOL_VERSION, pid: 1 }
}

/// Spawn `server` with a "shell" thread that calls `handler` for each request.
fn serve(server: Server, handler: impl Fn(Incoming) + Send + 'static) {
    let (tx, rx) = async_channel::unbounded::<Incoming>();
    smol::spawn(server.run(tx)).detach();
    thread::spawn(move || {
        smol::block_on(async move {
            while let Ok(inc) = rx.recv().await {
                handler(inc);
            }
        })
    });
}

/// A server whose shell answers `ping` with a pong and everything else with `ok`.
fn simple_server(dir: &Path) -> PathBuf {
    let path = dir.join("ashd/shell.sock");
    serve(Server::bind(&path).unwrap(), |inc| {
        let resp = if inc.request == Request::Ping { pong() } else { Response::Ok };
        inc.respond(resp);
    });
    path
}

fn raw(path: &Path) -> (UnixStream, BufReader<UnixStream>) {
    let s = UnixStream::connect(path).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let r = BufReader::new(s.try_clone().unwrap());
    (s, r)
}

fn read_response(r: &mut BufReader<UnixStream>) -> Response {
    let mut line = String::new();
    assert!(r.read_line(&mut line).unwrap() > 0, "connection closed without an answer");
    serde_json::from_str(line.trim_end()).unwrap_or_else(|e| panic!("bad response {line:?}: {e}"))
}

fn error_text(r: Response) -> String {
    match r {
        Response::Error(e) => e,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn roundtrip_and_second_shell_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ashd/shell.sock");
    let server = Server::bind(&path).unwrap();
    assert_eq!(Server::bind(&path).err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
    serve(server, |inc| {
        let resp = if inc.request == Request::Ping { pong() } else { Response::error("nope") };
        inc.respond(resp);
    });
    let mut client = Client::connect_to(&path).unwrap();
    for _ in 0..3 {
        assert_eq!(client.request(&Request::Ping).unwrap(), pong());
    }
    assert_eq!(client.ping().unwrap(), ("t".to_string(), PROTOCOL_VERSION, 1));
    let r = client.request(&Request::Theme(ThemeRequest::Get)).unwrap();
    assert!(matches!(r.into_result(), Err(IpcError::Remote(m)) if m == "nope"));
    // The first (live) shell is unharmed by the refused bind.
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
}

#[test]
fn the_shell_sees_every_request_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let (seen_tx, seen_rx) = mpsc::channel();
    serve(Server::bind(&path).unwrap(), move |inc| {
        seen_tx.send(inc.request.clone()).unwrap();
        inc.respond(Response::Ok);
    });
    let requests = vec![
        Request::Status,
        Request::Dismiss,
        Request::Lock,
        Request::Osd(OsdRequest::Volume(VolumeChange::Step(-5))),
        Request::Osd(OsdRequest::MicMute),
        Request::Launcher { op: PanelOp::Open, mode: None },
        Request::Theme(ThemeRequest::Set(ThemeName::Moss)),
    ];
    let mut client = Client::connect_to(&path).unwrap();
    for req in &requests {
        assert_eq!(client.request(req).unwrap(), Response::Ok);
        assert_eq!(&seen_rx.recv_timeout(Duration::from_secs(2)).unwrap(), req);
    }
}

#[test]
fn partial_lines_are_buffered() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    s.write_all(b"\"pi").unwrap();
    thread::sleep(Duration::from_millis(100));
    s.write_all(b"ng").unwrap();
    thread::sleep(Duration::from_millis(100));
    s.write_all(b"\"").unwrap();
    thread::sleep(Duration::from_millis(100));
    s.write_all(b"\n").unwrap();
    assert_eq!(read_response(&mut r), pong());
}

#[test]
fn pipelined_requests_are_answered_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    s.write_all(b"\"ping\"\n\"status\"\n\"ping\"\n").unwrap();
    assert_eq!(read_response(&mut r), pong());
    assert_eq!(read_response(&mut r), Response::Ok);
    assert_eq!(read_response(&mut r), pong());
}

#[test]
fn blank_lines_are_ignored_and_crlf_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    s.write_all(b"\n   \n\"ping\"\r\n").unwrap();
    assert_eq!(read_response(&mut r), pong());
}

#[test]
fn garbage_gets_an_error_and_the_connection_survives() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    for bad in [&b"not json\n"[..], b"{\"nope\":1}\n", b"\xff\xfe\xfd\n", b"\"pong\"\n", b"{\"osd\":\n"] {
        s.write_all(bad).unwrap();
        assert!(error_text(read_response(&mut r)).starts_with("bad request"), "for {bad:?}");
    }
    s.write_all(b"\"ping\"\n").unwrap();
    assert_eq!(read_response(&mut r), pong());
}

#[test]
fn unterminated_last_line_is_served_after_half_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    s.write_all(b"\"ping\"").unwrap(); // no newline, like `printf '"ping"' | nc -U`
    s.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(read_response(&mut r), pong());
    let mut rest = String::new();
    assert_eq!(r.read_line(&mut rest).unwrap(), 0, "server closes after the last answer");
}

#[test]
fn a_line_of_exactly_max_line_bytes_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    let mut line = b"\"ping\"".to_vec();
    line.resize(MAX_LINE, b' ');
    line.push(b'\n');
    s.write_all(&line).unwrap();
    assert_eq!(read_response(&mut r), pong());
}

#[test]
fn oversized_lines_get_an_error_and_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut s, mut r) = raw(&path);
    let writer = thread::spawn(move || {
        // The server may close while we are still writing: ignore write errors.
        let chunk = vec![b'a'; 8192];
        for _ in 0..40 {
            if s.write_all(&chunk).is_err() {
                break;
            }
        }
        s
    });
    let msg = error_text(read_response(&mut r));
    assert!(msg.contains("longer than"), "{msg}");
    drop(writer.join());
    // The server still works for others.
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
}

#[test]
fn client_disconnect_mid_request_does_not_hurt_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let (got_tx, got_rx) = mpsc::channel();
    serve(Server::bind(&path).unwrap(), move |inc| {
        if inc.request == Request::Status {
            // Slow answer: the client is long gone by then.
            got_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(200));
        }
        inc.respond(pong());
    });
    // 1. Disconnect while the shell is still working on the request.
    let (mut s, r) = raw(&path);
    s.write_all(b"\"status\"\n").unwrap();
    got_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    drop((s, r));
    // 2. Disconnect in the middle of a line.
    let (mut s, r) = raw(&path);
    s.write_all(b"{\"launcher\":{\"op\":\"op").unwrap();
    drop((s, r));
    // 3. Connect and say nothing.
    drop(UnixStream::connect(&path).unwrap());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
}

#[test]
fn dropping_the_incoming_answers_with_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    serve(Server::bind(&path).unwrap(), drop);
    let mut c = Client::connect_to(&path).unwrap();
    let msg = error_text(c.request(&Request::Ping).unwrap());
    assert!(msg.contains("dropped"), "{msg}");
    // ... and the connection is still usable.
    let msg = error_text(c.request(&Request::Ping).unwrap());
    assert!(msg.contains("dropped"), "{msg}");
}

#[test]
fn a_closed_shell_channel_answers_shutting_down() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let (tx, rx) = async_channel::unbounded::<Incoming>();
    smol::spawn(Server::bind(&path).unwrap().run(tx)).detach();
    // The shell answers one request, then goes away.
    thread::spawn(move || {
        smol::block_on(async move {
            if let Ok(inc) = rx.recv().await {
                inc.respond(pong());
            }
        })
    });
    let mut c = Client::connect_to(&path).unwrap();
    assert_eq!(c.request(&Request::Ping).unwrap(), pong());
    thread::sleep(Duration::from_millis(100));
    let msg = error_text(c.request(&Request::Ping).unwrap());
    assert!(msg.contains("shutting down"), "{msg}");
}

#[test]
fn concurrent_clients() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || {
                let mut c = Client::connect_to(&path).unwrap();
                for _ in 0..50 {
                    assert_eq!(c.request(&Request::Ping).unwrap(), pong());
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

#[test]
fn too_many_connections_are_turned_away() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let mut held = Vec::new();
    for _ in 0..Server::MAX_CONNECTIONS {
        let mut c = Client::connect_to(&path).unwrap();
        assert_eq!(c.request(&Request::Ping).unwrap(), pong()); // accepted & registered
        held.push(c);
    }
    let (_s, mut r) = raw(&path);
    assert!(error_text(read_response(&mut r)).contains("too many connections"));
    drop(held);
    // Slots are freed again.
    thread::sleep(Duration::from_millis(200));
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
}

// ── socket file handling ──────────────────────────────────────────────────────

#[test]
fn stale_socket_from_a_crashed_shell_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ashd/shell.sock");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // "Crash": a std listener dropped without removing its socket file.
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    assert!(matches!(Client::connect_to(&path), Err(IpcError::NotRunning(_))), "stale = not running");

    let server = Server::bind(&path).expect("stale socket must be replaced");
    serve(server, |inc| inc.respond(pong()));
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
}

#[test]
fn a_live_socket_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    for _ in 0..5 {
        assert_eq!(Server::bind(&path).err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
    }
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
}

#[test]
fn a_live_but_busy_listener_counts_as_live() {
    // A listener with a full accept queue never accepts: still alive.
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, bind, listen, socket_with};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("busy.sock");
    let fd = socket_with(AddressFamily::UNIX, SocketType::STREAM, SocketFlags::CLOEXEC, None).unwrap();
    bind(&fd, &SocketAddrUnix::new(&path).unwrap()).unwrap();
    listen(&fd, 0).unwrap();
    let mut fillers = Vec::new();
    for _ in 0..8 {
        let s = socket_with(AddressFamily::UNIX, SocketType::STREAM, SocketFlags::CLOEXEC | SocketFlags::NONBLOCK, None)
            .unwrap();
        let _ = rustix::net::connect(&s, &SocketAddrUnix::new(&path).unwrap());
        fillers.push(s);
    }
    assert_eq!(Server::bind(&path).err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
    // A client must not hang forever on it either.
    let started = Instant::now();
    let err = Client::connect_timeout(&path, Duration::from_millis(300)).err().expect("connect must time out");
    assert!(err.is_timeout(), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_regular_file_is_never_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shell.sock");
    std::fs::write(&path, "precious").unwrap();
    let err = Server::bind(&path).err().expect("must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
}

#[test]
fn directories_are_private_and_the_socket_is_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a/b/ashd/shell.sock");
    let _server = Server::bind(&path).unwrap();
    for d in ["a", "a/b", "a/b/ashd"] {
        let mode = std::fs::metadata(dir.path().join(d)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{d} must be 0700");
    }
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), rustix::process::getuid().as_raw());
}

#[test]
fn an_existing_parent_directory_is_left_alone() {
    // ASHD_SOCKET=/tmp/x.sock must not chmod /tmp.
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = dir.path().join("x.sock");
    let _server = Server::bind(&path).unwrap();
    let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
}

#[test]
fn dropping_the_server_removes_its_socket_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let server = Server::bind(&path).unwrap();
    assert_eq!(server.path(), path);
    drop(server);
    assert!(!path.exists(), "socket removed on drop");

    // If somebody else owns the path by then, the drop leaves it alone.
    let server = Server::bind(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let _other = UnixListener::bind(&path).unwrap();
    drop(server);
    assert!(path.exists(), "a replaced socket must survive the old server's drop");
}

#[test]
fn concurrent_binders_never_both_win() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    drop(UnixListener::bind(&path).unwrap()); // stale
    let (tx, rx) = mpsc::channel();
    for _ in 0..8 {
        let (path, tx) = (path.clone(), tx.clone());
        thread::spawn(move || {
            let r = Server::bind(&path);
            tx.send(r.map_err(|e| e.kind())).unwrap();
            // Keep the winner alive until all have reported.
            thread::sleep(Duration::from_millis(300));
        });
    }
    let results: Vec<_> = (0..8).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "{:?}", results.iter().map(|r| r.as_ref().err().copied()).collect::<Vec<_>>());
    assert!(results.iter().filter_map(|r| r.as_ref().err()).all(|k| *k == std::io::ErrorKind::AddrInUse));
}

// ── client against fake servers ───────────────────────────────────────────────

/// A fake server: for each accepted connection runs `script(conn_index, stream)`.
fn fake(path: &Path, script: impl Fn(usize, UnixStream) + Send + Sync + 'static) {
    let listener = UnixListener::bind(path).unwrap();
    thread::spawn(move || {
        let script = std::sync::Arc::new(script);
        for (i, conn) in listener.incoming().enumerate() {
            let Ok(conn) = conn else { break };
            let script = script.clone();
            thread::spawn(move || script(i, conn));
        }
    });
}

fn read_request_line(s: &mut UnixStream) -> String {
    let mut line = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
    line
}

#[test]
fn not_running_when_there_is_no_socket() {
    let dir = tempfile::tempdir().unwrap();
    let err = Client::connect_to(dir.path().join("nope.sock")).err().unwrap();
    assert!(err.is_not_running(), "{err:?}");
    assert!(err.to_string().contains("not running"));
}

#[test]
fn client_times_out_on_a_hung_shell_and_reconnects_afterwards() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.sock");
    fake(&path, |i, mut s| {
        let line = read_request_line(&mut s);
        assert!(line.contains("ping"));
        if i == 0 {
            thread::sleep(Duration::from_secs(3)); // hung
        } else {
            s.write_all(b"\"ok\"\n").unwrap();
        }
    });
    let mut c = Client::connect_timeout(&path, Duration::from_millis(200)).unwrap();
    let started = Instant::now();
    let err = c.request(&Request::Ping).unwrap_err();
    assert!(err.is_timeout(), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    // The late answer of the hung connection must not be mistaken for this one.
    assert_eq!(c.request(&Request::Ping).unwrap(), Response::Ok);
}

#[test]
fn client_reports_a_shell_that_hangs_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.sock");
    fake(&path, |i, mut s| {
        let _ = read_request_line(&mut s);
        if i == 0 {
            drop(s); // closes without answering
        } else {
            s.write_all(b"\"ok\"\n").unwrap();
        }
    });
    let mut c = Client::connect_to(&path).unwrap();
    assert!(matches!(c.request(&Request::Ping), Err(IpcError::Closed)));
    assert_eq!(c.request(&Request::Ping).unwrap(), Response::Ok, "reconnects");
}

#[test]
fn client_rejects_garbage_answers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.sock");
    fake(&path, |_, mut s| {
        let _ = read_request_line(&mut s);
        s.write_all(b"this is not json\n").unwrap();
    });
    let mut c = Client::connect_to(&path).unwrap();
    assert!(matches!(c.request(&Request::Ping), Err(IpcError::Protocol(_))));
}

#[test]
fn client_rejects_unterminated_and_oversized_answers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.sock");
    fake(&path, |i, mut s| {
        let _ = read_request_line(&mut s);
        match i {
            0 => s.write_all(b"\"ok\"").unwrap(), // dies mid-answer: no newline
            _ => {
                let big = vec![b'x'; MAX_LINE + 100];
                let _ = s.write_all(&big);
            }
        }
    });
    let mut c = Client::connect_to(&path).unwrap();
    assert!(matches!(c.request(&Request::Ping), Err(IpcError::Closed)));
    let err = c.request(&Request::Ping).unwrap_err();
    assert!(matches!(&err, IpcError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData), "{err:?}");
}

#[test]
fn client_serves_many_requests_on_one_connection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.sock");
    fake(&path, |_, mut s| {
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        let mut n = 0u32;
        while reader.read_line(&mut line).unwrap() > 0 {
            n += 1;
            let resp = serde_json::to_string(&Response::Pong { version: "f".into(), protocol: 1, pid: n }).unwrap();
            writeln!(s, "{resp}").unwrap();
            line.clear();
        }
    });
    let mut c = Client::connect_to(&path).unwrap();
    for n in 1..=20 {
        assert_eq!(c.ping().unwrap().2, n);
    }
}

#[test]
fn a_connection_that_never_sends_is_not_a_problem_for_others() {
    let dir = tempfile::tempdir().unwrap();
    let path = simple_server(dir.path());
    let (mut idle, _r) = raw(&path);
    idle.write_all(b"\"pi").unwrap(); // stalls mid-line
    assert_eq!(Client::connect_to(&path).unwrap().request(&Request::Ping).unwrap(), pong());
    let mut sink = Vec::new();
    idle.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    assert!(idle.read_to_end(&mut sink).is_err(), "still open, just idle");
}
