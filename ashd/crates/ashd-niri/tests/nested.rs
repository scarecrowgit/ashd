//! Integration test against the *nested* session's niri (never the live one).
//!
//! ```sh
//! nix-shell ashd/shell.nix
//! cd ashd && root=$(cargo xtask nested start); source "$root/env"
//! cargo test -p ashd-niri --test nested -- --ignored
//! cargo xtask nested stop "$root"
//! ```
//! Ignored by default; refuses to run unless `$NIRI_SOCKET` lives in a `ashd-nested.*`
//! directory.

use std::time::{Duration, Instant};

use ashd_niri::{
    blocking,
    niri_ipc::{Request, Response},
};

fn nested_socket() -> std::path::PathBuf {
    let sock = std::env::var_os("NIRI_SOCKET").expect("NIRI_SOCKET not set: source the nested session env");
    let sock = std::path::PathBuf::from(sock);
    assert!(
        sock.to_string_lossy().contains("ashd-nested."),
        "{} is not a nested-session socket; refusing to touch it",
        sock.display()
    );
    sock
}

#[test]
#[ignore = "needs the nested session (see the module docs)"]
fn follows_the_nested_niri() {
    let sock = nested_socket();
    let (handle, rx) = ashd_niri::start_with(Some(sock));
    let deadline = Instant::now() + Duration::from_secs(10);
    let state = loop {
        let s = rx.recv_blocking().expect("state channel closed");
        if s.connected {
            break s;
        }
        assert!(Instant::now() < deadline, "never connected to the nested niri");
    };
    assert!(!state.workspaces.is_empty(), "niri always has at least one workspace");
    assert!(state.focused_workspace().is_some());
    assert!(!state.outputs.is_empty(), "outputs are fetched after the first workspace event");
    for out in state.outputs.keys() {
        assert!(state.workspaces_on(out).iter().all(|w| w.output.as_deref() == Some(out.as_str())));
    }
    // A request through the handle.
    match smol::block_on(handle.request(Request::Version)).expect("version") {
        Response::Version(v) => assert!(!v.is_empty()),
        other => panic!("{other:?}"),
    }
    // ... and through the blocking helper.
    assert!(matches!(blocking::request(Request::Outputs), Ok(Response::Outputs(o)) if !o.is_empty()));
    // Nothing focused → nothing to close (the nested session starts empty).
    if state.focused_window().is_none() {
        assert_eq!(blocking::close_focused_app_windows().unwrap(), 0);
    }
}
