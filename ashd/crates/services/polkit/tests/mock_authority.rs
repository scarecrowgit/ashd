//! The agent against a mock authority (peer-to-peer D-Bus, no bus daemon) and a fake
//! helper on a Unix socket — never the real polkit, never the real helper.

mod common;

use std::{collections::HashMap, sync::atomic::Ordering, time::Duration};

use common::*;
use ashd_svc_polkit::{AGENT_PATH, Command, HelperTarget, Phase, ProcessRole};
use zeroize::Zeroizing;

fn pw(s: &str) -> Zeroizing<String> {
    Zeroizing::new(s.to_string())
}

fn script_for(user: &str, password: &str) -> Script {
    Script { passwords: HashMap::from([(user.to_string(), password.to_string())]), ..Default::default() }
}

#[test]
fn registers_for_the_session() {
    let f = fixture(Script::default());
    let s = wait_for(&f.rx, "registered", |s| s.available);
    assert!(s.enabled);
    assert_eq!(s.session_id.as_deref(), Some("c7"));
    assert!(s.pending.is_none());
    wait_log(&f.log, &format!("Register(unix-session,c7,en_US.UTF-8,{AGENT_PATH})"));
}

#[test]
fn session_is_looked_up_in_logind_when_not_given() {
    let helper = FakeHelper::start(Script::default());
    let mut opts = options(helper.target());
    opts.session_id = None;
    let f = fixture_with(helper, opts, true);
    let s = wait_for(&f.rx, "registered", |s| s.available);
    assert_eq!(s.session_id.as_deref(), Some("c9"));
    wait_log(&f.log, &format!("Register(unix-session,c9,en_US.UTF-8,{AGENT_PATH})"));
}

#[test]
fn success() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "hunter2"));
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(
        &f.server,
        "org.freedesktop.policykit.exec",
        "cookie-1",
        &[uid],
        &[("polkit.subject-pid", &std::process::id().to_string()), ("program", "/run/current-system/sw/bin/true")],
    );
    let s = wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    let req = s.pending.unwrap();
    assert_eq!(req.action_id, "org.freedesktop.policykit.exec");
    assert_eq!(req.message, "Authentication is needed to run `true' as the super user");
    assert_eq!(req.details.get("program").map(String::as_str), Some("/run/current-system/sw/bin/true"));
    assert_eq!(req.prompt.as_deref(), Some("Password: "));
    assert!(!req.echo);
    assert_eq!(req.selected, Some(uid));
    assert_eq!(req.identities.len(), 1);
    assert_eq!(req.identities[0].name, name);
    // the requesting process is resolved from /proc (this test binary)
    assert_eq!(req.processes.len(), 1);
    assert_eq!(req.processes[0].role, ProcessRole::Subject);
    assert!(req.processes[0].exe.as_deref().is_some_and(|e| e.contains("mock_authority")), "{:?}", req.processes);
    assert_eq!(s.queue_len, 0);
    assert!(call.pending());

    f.handle.respond(pw("hunter2"));
    wait_for(&f.rx, "checking", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Checking) || s.pending.is_none());
    assert_eq!(call.result(), Ok(()));
    let s = wait_for(&f.rx, "done", |s| s.pending.is_none() && s.succeeded == 1);
    assert_eq!(s.queue_len, 0);
    assert_eq!(f.helper.authenticated(), vec![(name, "cookie-1".to_string())]);
}

#[test]
fn wrong_password_then_success() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "right"));
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-2", &[uid], &[]);
    let s = wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    let id = s.pending.unwrap().id;
    f.handle.respond_to(id, pw("wrong"));
    let s = wait_for(&f.rx, "retry prompt", |s| {
        s.pending.as_ref().is_some_and(|p| p.attempts == 1 && p.phase == Phase::Prompting)
    });
    let req = s.pending.unwrap();
    assert_eq!(req.error.as_deref(), Some("Wrong password. Try again."));
    assert_eq!(req.id, id, "same request, new conversation");
    assert!(call.pending());
    assert_eq!(f.helper.script.connections.load(Ordering::SeqCst), 2);

    f.handle.respond_to(id, pw("right"));
    assert_eq!(call.result(), Ok(()));
    wait_for(&f.rx, "done", |s| s.pending.is_none() && s.succeeded == 1);
}

#[test]
fn multiple_identities_default_to_me_and_can_be_switched() {
    let (uid, name) = me();
    // root plus this user (unless the tests run as root)
    let mut script = script_for("root", "toor");
    script.passwords.insert(name.clone(), "mine".into());
    let f = fixture(script);
    wait_for(&f.rx, "registered", |s| s.available);
    let uids = if uid == 0 { vec![0, 65534] } else { vec![0, uid] };
    let call = begin(&f.server, "a.b", "cookie-3", &uids, &[]);
    let s = wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    let req = s.pending.unwrap();
    assert_eq!(req.identities.len(), 2);
    assert_eq!(req.identities[0].name, "root");
    assert_eq!(req.selected, Some(uid), "the current user is the default");
    assert_eq!(f.helper.script.sessions.lock().unwrap().as_slice(), std::slice::from_ref(&name));

    if uid != 0 {
        f.handle.select_identity(0);
        let s = wait_for(&f.rx, "root prompt", |s| {
            s.pending.as_ref().is_some_and(|p| p.selected == Some(0) && p.phase == Phase::Prompting)
        });
        assert_eq!(s.pending.unwrap().selected_identity().map(|i| i.name.as_str()), Some("root"));
        // not in the list: ignored
        f.handle.select_identity(4_000_000);
        f.handle.respond(pw("toor"));
        assert_eq!(call.result(), Ok(()));
        assert_eq!(f.helper.authenticated(), vec![("root".to_string(), "cookie-3".to_string())]);
        assert_eq!(f.helper.script.sessions.lock().unwrap().as_slice(), [name, "root".to_string()]);
    } else {
        f.handle.cancel();
        assert_eq!(call.result(), Err(CANCELLED.into()));
    }
}

#[test]
fn info_and_error_messages_reach_the_state() {
    let (uid, name) = me();
    let mut script = script_for(&name, "right");
    script.info = Some("Touch your security key\n".into());
    script.error = Some("Sorry, that didn't work".into());
    let f = fixture(script);
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-4", &[uid], &[]);
    let s = wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    assert_eq!(s.pending.unwrap().info.as_deref(), Some("Touch your security key\n"));
    f.handle.respond(pw("nope"));
    let s = wait_for(&f.rx, "pam error", |s| s.pending.as_ref().is_some_and(|p| p.attempts == 1 && p.phase == Phase::Prompting));
    assert_eq!(s.pending.unwrap().error.as_deref(), Some("Sorry, that didn't work"), "PAM_ERROR_MSG wins over the generic text");
    f.handle.cancel();
    assert_eq!(call.result(), Err(CANCELLED.into()));
}

#[test]
fn authority_cancel_closes_the_request() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "x"));
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-5", &[uid], &[]);
    wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    cancel(&f.server, "cookie-5");
    assert_eq!(call.result(), Err(CANCELLED.into()));
    wait_for(&f.rx, "closed", |s| s.pending.is_none());
    // an unknown cookie is harmless
    cancel(&f.server, "nope");
}

#[test]
fn user_cancel_and_stale_ids() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "x"));
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-6", &[uid], &[]);
    let s = wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    let id = s.pending.unwrap().id;
    // a command for another request id does nothing
    f.handle.cancel_request(id + 100);
    std::thread::sleep(Duration::from_millis(100));
    assert!(call.pending());
    f.handle.cancel_request(id);
    assert_eq!(call.result(), Err(CANCELLED.into()));
    wait_for(&f.rx, "closed", |s| s.pending.is_none());
    assert!(f.helper.authenticated().is_empty());
}

#[test]
fn helper_crash_is_reported_and_retry_works() {
    let (uid, name) = me();
    let script = script_for(&name, "right");
    script.crash_next.store(1, Ordering::SeqCst);
    let f = fixture(script);
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-7", &[uid], &[]);
    let s = wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    let id = s.pending.unwrap().id;
    f.handle.respond(pw("right"));
    let s = wait_for(&f.rx, "unavailable", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Unavailable));
    let err = s.pending.unwrap().error.unwrap();
    assert!(err.contains("closed the connection"), "{err}");
    assert!(call.pending(), "the request stays open for a retry");
    // responses are ignored while no prompt is open
    f.handle.respond(pw("right"));
    f.handle.retry(id);
    wait_for(&f.rx, "prompt again", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting && p.error.is_none()));
    f.handle.respond(pw("right"));
    assert_eq!(call.result(), Ok(()));
}

#[test]
fn unreachable_or_mute_helper_fails_instead_of_hanging() {
    let (uid, _) = me();
    let helper = FakeHelper::start(Script::default());
    let opts = options(HelperTarget::Socket("/nonexistent/ashd/agent-helper.sock".into()));
    let f = fixture_with(helper, opts, false);
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-8", &[uid], &[]);
    let s = wait_for(&f.rx, "unavailable", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Unavailable));
    assert!(s.pending.unwrap().error.unwrap().contains("cannot reach"));
    f.handle.cancel();
    assert_eq!(call.result(), Err(CANCELLED.into()));

    let f = fixture(Script { mute: true, ..Default::default() });
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-9", &[uid], &[]);
    let s = wait_for(&f.rx, "timeout", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Unavailable));
    assert!(s.pending.unwrap().error.unwrap().contains("did not answer"));
    f.handle.cancel();
    assert_eq!(call.result(), Err(CANCELLED.into()));
}

#[test]
fn two_requests_are_queued() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "pw"));
    wait_for(&f.rx, "registered", |s| s.available);
    let a = begin(&f.server, "first.action", "cookie-a", &[uid], &[]);
    wait_for(&f.rx, "first", |s| s.pending.as_ref().is_some_and(|p| p.action_id == "first.action"));
    let b = begin(&f.server, "second.action", "cookie-b", &[uid], &[]);
    let s = wait_for(&f.rx, "queued + prompt", |s| {
        s.queue_len == 1 && s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting)
    });
    assert_eq!(s.pending.as_ref().unwrap().action_id, "first.action", "one dialog at a time");
    assert_eq!(f.helper.script.connections.load(Ordering::SeqCst), 1, "no conversation for the queued one");
    f.handle.respond(pw("pw"));
    assert_eq!(a.result(), Ok(()));
    let s = wait_for(&f.rx, "second", |s| {
        s.pending.as_ref().is_some_and(|p| p.action_id == "second.action" && p.phase == Phase::Prompting)
    });
    assert_eq!(s.queue_len, 0);
    assert!(b.pending());
    f.handle.cancel();
    assert_eq!(b.result(), Err(CANCELLED.into()));
    wait_for(&f.rx, "empty", |s| s.pending.is_none() && s.queue_len == 0);
}

#[test]
fn a_queued_request_can_be_cancelled_by_the_authority() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "pw"));
    wait_for(&f.rx, "registered", |s| s.available);
    let a = begin(&f.server, "first", "c-a", &[uid], &[]);
    wait_for(&f.rx, "first", |s| s.pending.is_some());
    let b = begin(&f.server, "second", "c-b", &[uid], &[]);
    wait_for(&f.rx, "queued", |s| s.queue_len == 1);
    cancel(&f.server, "c-b");
    assert_eq!(b.result(), Err(CANCELLED.into()));
    wait_for(&f.rx, "dequeued", |s| s.queue_len == 0 && s.pending.as_ref().is_some_and(|p| p.action_id == "first"));
    f.handle.cancel();
    assert_eq!(a.result(), Err(CANCELLED.into()));
}

#[test]
fn responses_are_validated() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "pw"));
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-10", &[uid], &[]);
    wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    f.handle.respond(pw(&"x".repeat(600)));
    let s = wait_for(&f.rx, "too long", |s| s.pending.as_ref().is_some_and(|p| p.error.is_some()));
    let p = s.pending.unwrap();
    assert_eq!(p.phase, Phase::Prompting);
    assert_eq!(p.error.as_deref(), Some("The password is too long."));
    f.handle.respond(pw("pw"));
    assert_eq!(call.result(), Ok(()));
}

#[test]
fn reregisters_after_the_authority_restarts() {
    let (uid, name) = me();
    let f = fixture(script_for(&name, "pw"));
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-11", &[uid], &[]);
    wait_for(&f.rx, "prompt", |s| s.pending.is_some());
    // the authority goes away …
    let Fixture { server, conns, rx, handle, .. } = f;
    smol::block_on(server.close()).unwrap();
    assert!(call.result().is_err());
    wait_for(&rx, "lost", |s| !s.available && s.pending.is_none());
    // … and comes back: the agent registers with the new one
    let log2 = Log::default();
    let (server2, client2) = pair(&log2, false);
    conns.try_send(client2).unwrap();
    wait_for(&rx, "registered again", |s| s.available);
    wait_log(&log2, &format!("Register(unix-session,c7,en_US.UTF-8,{AGENT_PATH})"));
    // and serves requests there
    let call = begin(&server2, "a.b", "cookie-12", &[uid], &[]);
    wait_for(&rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    handle.respond(pw("pw"));
    assert_eq!(call.result(), Ok(()));
    // shutdown unregisters
    handle.shutdown();
    wait_log(&log2, &format!("Unregister(c7,{AGENT_PATH})"));
}

#[test]
fn requests_without_identities_fail() {
    let f = fixture(Script::default());
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-13", &[], &[]);
    assert_eq!(call.result(), Err("org.freedesktop.PolicyKit1.Error.Failed".into()));
}

#[test]
fn passwords_never_show_in_debug_output() {
    let c = Command::Respond { id: Some(3), response: pw("hunter2") };
    let s = format!("{c:?}");
    assert!(!s.contains("hunter2"), "{s}");
    assert!(s.contains("redacted"));
}

/// The setuid-helper transport, with `examples/polkit_fake_helper.rs` as the helper
/// (`cargo test` builds the examples next to the test binaries).
#[test]
fn exec_helper_transport() {
    let exe = std::env::current_exe().unwrap();
    let helper = exe.parent().and_then(|d| d.parent()).unwrap().join("examples/polkit_fake_helper");
    assert!(helper.exists(), "{} not built — run the whole `cargo test -p ashd-svc-polkit`", helper.display());
    let (uid, _) = me();
    let fake = FakeHelper::start(Script::default());
    let f = fixture_with(fake, options(HelperTarget::Exec(helper)), false);
    wait_for(&f.rx, "registered", |s| s.available);
    let call = begin(&f.server, "a.b", "cookie-14", &[uid], &[]);
    wait_for(&f.rx, "prompt", |s| s.pending.as_ref().is_some_and(|p| p.phase == Phase::Prompting));
    f.handle.respond(pw("wrong"));
    wait_for(&f.rx, "failed", |s| s.pending.as_ref().is_some_and(|p| p.attempts == 1 && p.phase == Phase::Prompting));
    f.handle.respond(pw(ashd_svc_polkit_fake_password()));
    assert_eq!(call.result(), Ok(()));
}

/// The password `examples/polkit_fake_helper.rs` accepts.
fn ashd_svc_polkit_fake_password() -> &'static str {
    "hunter2"
}
