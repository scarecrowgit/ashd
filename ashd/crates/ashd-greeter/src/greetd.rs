//! The greetd side: the IPC conversation on a worker thread.
//!
//! greetd speaks length-prefixed JSON (native-endian `u32`) over `$GREETD_SOCK`; the
//! message types and the response reader come from the `greetd_ipc` crate. Requests are
//! framed here instead of with the crate's `write_to` so the password never sits in a
//! buffer that is not wiped (the JSON body is built in a pre-sized `Zeroizing` buffer).
//!
//! The worker owns the stream and runs one request at a time. The UI sends [`Cmd`]s and
//! receives [`Event`]s (in practice through a calloop channel):
//!
//! - [`Cmd::Create`] → `create_session`, then follows greetd's auth messages: `info` and
//!   `error` messages are shown ([`GreetdEvent::Message`]) and acknowledged here, `visible`
//!   / `secret` ones become a [`GreetdEvent::Prompt`] and wait for [`Cmd::Answer`]; a final
//!   `success` is [`GreetdEvent::Authenticated`], an `error` response is
//!   [`GreetdEvent::Failed`] (greetd has cancelled the session itself by then).
//! - [`Cmd::Cancel`] → `cancel_session` (no event).
//! - [`Cmd::Start`] → `start_session`: [`Event::Started`] on success, `Failed` otherwise.
//! - an I/O error or EOF ends the thread with [`Event::Disconnected`].

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::mpsc,
    thread::JoinHandle,
};

use greetd_ipc::{AuthMessageType, ErrorType, Request, Response, codec::SyncCodec};
use ashd_lockui::{GreetdEvent, PromptKind};
use zeroize::{Zeroize, Zeroizing};

/// UI → worker.
#[derive(Debug)]
pub enum Cmd {
    Create(String),
    Answer(Zeroizing<String>),
    Cancel,
    Start { cmd: Vec<String>, env: Vec<String> },
}

/// Worker → UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Greetd(GreetdEvent),
    /// `start_session` was accepted: greetd starts the session once we exit.
    Started,
    /// The socket is gone (greetd died / closed it).
    Disconnected(String),
}

/// Connect to greetd's socket.
pub fn connect(path: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Frame `req` (native-endian length + JSON) into `w`. The body is built in a
/// `Zeroizing` buffer with room for the largest request, so it never reallocates.
pub fn write_request<W: Write>(w: &mut W, req: &Request) -> Result<(), greetd_ipc::codec::Error> {
    let extra = match req {
        Request::PostAuthMessageResponse { response: Some(r) } => r.len() * 6,
        Request::CreateSession { username } => username.len() * 6,
        Request::StartSession { cmd, env } => cmd.iter().chain(env).map(|s| s.len() * 6 + 8).sum(),
        _ => 0,
    };
    let mut body: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(256 + extra));
    serde_json::to_writer(&mut *body, req)?;
    let len = u32::try_from(body.len()).map_err(|_| std::io::Error::other("request too large"))?;
    let mut framed: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(4 + body.len()));
    framed.extend_from_slice(&len.to_ne_bytes());
    framed.extend_from_slice(&body);
    w.write_all(&framed)?;
    w.flush()?;
    Ok(())
}

struct Worker<S> {
    stream: S,
    /// A greetd session exists (created, not failed / cancelled / started).
    open: bool,
    notify: Box<dyn Fn(Event) + Send>,
}

type IoResult<T> = Result<T, greetd_ipc::codec::Error>;

impl<S: Read + Write> Worker<S> {
    fn exchange(&mut self, mut req: Request) -> IoResult<Response> {
        let r = write_request(&mut self.stream, &req);
        // Wipe the password out of the request whatever happened.
        if let Request::PostAuthMessageResponse { response: Some(s) } = &mut req {
            s.zeroize();
        }
        r?;
        Response::read_from(&mut self.stream)
    }

    fn emit(&self, e: GreetdEvent) {
        (self.notify)(Event::Greetd(e));
    }

    /// Follow greetd's responses until one needs the user (prompt) or ends the conversation.
    fn drive(&mut self, mut resp: Response) -> IoResult<()> {
        loop {
            match resp {
                Response::Success => {
                    self.emit(GreetdEvent::Authenticated);
                    return Ok(());
                }
                Response::Error { error_type, description } => {
                    self.open = false;
                    self.emit(GreetdEvent::Failed { auth: matches!(error_type, ErrorType::AuthError), text: description });
                    return Ok(());
                }
                Response::AuthMessage { auth_message_type, auth_message } => match auth_message_type {
                    AuthMessageType::Info | AuthMessageType::Error => {
                        self.emit(GreetdEvent::Message { error: matches!(auth_message_type, AuthMessageType::Error), text: auth_message });
                        resp = self.exchange(Request::PostAuthMessageResponse { response: None })?;
                    }
                    AuthMessageType::Visible | AuthMessageType::Secret => {
                        let kind = if matches!(auth_message_type, AuthMessageType::Secret) { PromptKind::Secret } else { PromptKind::Visible };
                        self.emit(GreetdEvent::Prompt { kind, text: auth_message });
                        return Ok(());
                    }
                },
            }
        }
    }

    fn cancel(&mut self) -> IoResult<()> {
        if self.open {
            self.open = false;
            // Success, or an error because there was nothing to cancel: either is fine.
            let _ = self.exchange(Request::CancelSession)?;
        }
        Ok(())
    }

    fn handle(&mut self, cmd: Cmd) -> IoResult<()> {
        match cmd {
            Cmd::Create(username) => {
                self.cancel()?;
                self.open = true;
                let resp = self.exchange(Request::CreateSession { username })?;
                self.drive(resp)
            }
            Cmd::Answer(pw) => {
                if !self.open {
                    return Ok(());
                }
                let resp = self.exchange(Request::PostAuthMessageResponse { response: Some(pw.to_string()) })?;
                self.drive(resp)
            }
            Cmd::Cancel => self.cancel(),
            Cmd::Start { cmd, env } => {
                if !self.open {
                    self.emit(GreetdEvent::Failed { auth: false, text: "No authenticated session to start".into() });
                    return Ok(());
                }
                match self.exchange(Request::StartSession { cmd, env })? {
                    Response::Success => {
                        self.open = false;
                        (self.notify)(Event::Started);
                    }
                    Response::Error { description, .. } => {
                        self.open = false;
                        self.emit(GreetdEvent::Failed { auth: false, text: description });
                    }
                    Response::AuthMessage { .. } => {
                        self.emit(GreetdEvent::Failed { auth: false, text: "greetd asked another question after login".into() });
                    }
                }
                Ok(())
            }
        }
    }
}

/// Run the conversation on a new thread over `stream`. `notify` is called from that thread.
/// Dropping the returned sender ends the thread.
pub fn spawn<S>(stream: S, notify: impl Fn(Event) + Send + 'static) -> (mpsc::Sender<Cmd>, JoinHandle<()>)
where
    S: Read + Write + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Cmd>();
    let handle = std::thread::Builder::new()
        .name("ashd-greeter-greetd".into())
        .spawn(move || {
            let mut w = Worker { stream, open: false, notify: Box::new(notify) };
            for cmd in rx {
                if let Err(e) = w.handle(cmd) {
                    tracing::warn!("greetd connection lost: {e}");
                    (w.notify)(Event::Disconnected(describe(&e)));
                    return;
                }
            }
        })
        .expect("spawn greetd thread");
    (tx, handle)
}

fn describe(e: &greetd_ipc::codec::Error) -> String {
    match e {
        greetd_ipc::codec::Error::Eof => "greetd closed the connection".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, time::Duration};

    use serde_json::{Value, json};

    use super::*;
    use crate::fake::{FakeGreetd, Log, Step};

    fn framed(v: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(v).unwrap();
        let mut out = (body.len() as u32).to_ne_bytes().to_vec();
        out.extend(body);
        out
    }

    fn wire_of(req: &Request) -> Value {
        let mut buf = Vec::new();
        write_request(&mut buf, req).unwrap();
        let len = u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(len, buf.len() - 4, "native-endian length prefix covers exactly the JSON");
        serde_json::from_slice(&buf[4..]).unwrap()
    }

    #[test]
    fn requests_use_greetds_wire_format() {
        assert_eq!(wire_of(&Request::CreateSession { username: "alice".into() }), json!({"type": "create_session", "username": "alice"}));
        assert_eq!(
            wire_of(&Request::PostAuthMessageResponse { response: Some("pw \"quoted\" é".into()) }),
            json!({"type": "post_auth_message_response", "response": "pw \"quoted\" é"})
        );
        assert_eq!(wire_of(&Request::PostAuthMessageResponse { response: None }), json!({"type": "post_auth_message_response", "response": null}));
        assert_eq!(
            wire_of(&Request::StartSession { cmd: vec!["niri-session".into()], env: vec!["A=b".into()] }),
            json!({"type": "start_session", "cmd": ["niri-session"], "env": ["A=b"]})
        );
        assert_eq!(wire_of(&Request::CancelSession), json!({"type": "cancel_session"}));
    }

    #[test]
    fn our_framing_round_trips_through_the_crates_reader() {
        let reqs = [
            Request::CreateSession { username: "bob".into() },
            Request::PostAuthMessageResponse { response: Some("x".repeat(5000)) },
            Request::StartSession { cmd: vec!["a".into(), "b c".into()], env: vec![] },
            Request::CancelSession,
        ];
        let mut buf = Vec::new();
        for r in &reqs {
            write_request(&mut buf, r).unwrap();
        }
        let mut cur = Cursor::new(buf);
        for r in &reqs {
            let got = Request::read_from(&mut cur).unwrap();
            assert_eq!(format!("{got:?}"), format!("{r:?}"));
        }
        assert!(matches!(Request::read_from(&mut cur), Err(greetd_ipc::codec::Error::Eof)));
    }

    #[test]
    fn responses_decode_from_hand_framed_bytes() {
        let cases = [
            (json!({"type": "success"}), "Success"),
            (
                json!({"type": "error", "error_type": "auth_error", "description": "nope"}),
                "Error { error_type: AuthError, description: \"nope\" }",
            ),
            (
                json!({"type": "error", "error_type": "error", "description": "boom"}),
                "Error { error_type: Error, description: \"boom\" }",
            ),
            (
                json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password: "}),
                "AuthMessage { auth_message_type: Secret, auth_message: \"Password: \" }",
            ),
            (
                json!({"type": "auth_message", "auth_message_type": "visible", "auth_message": "Code:"}),
                "AuthMessage { auth_message_type: Visible, auth_message: \"Code:\" }",
            ),
            (
                json!({"type": "auth_message", "auth_message_type": "info", "auth_message": "hi"}),
                "AuthMessage { auth_message_type: Info, auth_message: \"hi\" }",
            ),
            (
                json!({"type": "auth_message", "auth_message_type": "error", "auth_message": "bad"}),
                "AuthMessage { auth_message_type: Error, auth_message: \"bad\" }",
            ),
        ];
        for (v, want) in cases {
            let got = Response::read_from(&mut Cursor::new(framed(&v))).unwrap();
            assert_eq!(format!("{got:?}"), want);
        }
    }

    // ── conversations against the fake ───────────────────────────────────────

    struct Rig {
        tx: mpsc::Sender<Cmd>,
        events: mpsc::Receiver<Event>,
        log: Arc<Mutex<Log>>,
    }

    use std::sync::{Arc, Mutex};

    fn rig(fake: FakeGreetd) -> Rig {
        let log = fake.log();
        let (client, _server) = fake.spawn();
        let (etx, events) = mpsc::channel();
        let (tx, _h) = spawn(client, move |e| {
            let _ = etx.send(e);
        });
        Rig { tx, events, log }
    }

    impl Rig {
        fn next(&self) -> Event {
            self.events.recv_timeout(Duration::from_secs(5)).expect("event")
        }
        fn prompt(&self, secret: bool, text: &str) {
            let kind = if secret { PromptKind::Secret } else { PromptKind::Visible };
            assert_eq!(self.next(), Event::Greetd(GreetdEvent::Prompt { kind, text: text.into() }));
        }
        fn answer(&self, s: &str) {
            self.tx.send(Cmd::Answer(Zeroizing::new(s.into()))).unwrap();
        }
        fn create(&self, u: &str) {
            self.tx.send(Cmd::Create(u.into())).unwrap();
        }
    }

    #[test]
    fn login_succeeds_and_the_session_starts() {
        let r = rig(FakeGreetd::new(vec![Step::password("hunter2")]));
        r.create("alice");
        r.prompt(true, "Password: ");
        r.answer("hunter2");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Authenticated));
        r.tx.send(Cmd::Start { cmd: vec!["niri-session".into()], env: vec!["X=1".into()] }).unwrap();
        assert_eq!(r.next(), Event::Started);
        let log = r.log.lock().unwrap();
        assert_eq!(log.created, ["alice"]);
        assert_eq!(log.started, [(vec!["niri-session".to_string()], vec!["X=1".to_string()])]);
    }

    #[test]
    fn wrong_password_fails_and_the_next_conversation_works() {
        let r = rig(FakeGreetd::new(vec![Step::password("right")]));
        r.create("alice");
        r.prompt(true, "Password: ");
        r.answer("wrong");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Failed { auth: true, text: "pam_authenticate: AUTH_ERR".into() }));
        // greetd cancelled the session itself: no cancel_session is sent before the retry
        r.create("alice");
        r.prompt(true, "Password: ");
        r.answer("right");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Authenticated));
        assert_eq!(r.log.lock().unwrap().cancels, 0);
        assert_eq!(r.log.lock().unwrap().created.len(), 2);
    }

    #[test]
    fn multi_step_conversation_with_messages() {
        let flow = vec![
            Step::Info("Your password expires in 3 days".into()),
            Step::password("pw"),
            Step::Error("Token is about to expire".into()),
            Step::Prompt { secret: false, text: "Verification code: ".into(), expect: Some("424242".into()) },
        ];
        let r = rig(FakeGreetd::new(vec![]).user("carol", flow));
        r.create("carol");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Message { error: false, text: "Your password expires in 3 days".into() }));
        r.prompt(true, "Password: ");
        r.answer("pw");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Message { error: true, text: "Token is about to expire".into() }));
        r.prompt(false, "Verification code: ");
        r.answer("424242");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Authenticated));
    }

    #[test]
    fn a_pam_stack_without_prompts_authenticates_at_once() {
        let r = rig(FakeGreetd::new(vec![]));
        r.create("guest");
        assert_eq!(r.next(), Event::Greetd(GreetdEvent::Authenticated));
    }

    #[test]
    fn cancel_ends_the_conversation_silently() {
        let r = rig(FakeGreetd::new(vec![Step::password("pw")]));
        r.create("alice");
        r.prompt(true, "Password: ");
        r.tx.send(Cmd::Cancel).unwrap();
        // a stray answer after the cancel is ignored (no hang, no event)…
        r.answer("pw");
        // …and a new conversation starts clean
        r.create("bob");
        r.prompt(true, "Password: ");
        let log = r.log.lock().unwrap();
        assert_eq!(log.cancels, 1);
        assert_eq!(log.created, ["alice", "bob"]);
    }

    #[test]
    fn creating_over_an_open_session_cancels_it_first() {
        let r = rig(FakeGreetd::new(vec![Step::password("pw")]));
        r.create("alice");
        r.prompt(true, "Password: ");
        r.create("bob");
        r.prompt(true, "Password: ");
        assert_eq!(r.log.lock().unwrap().cancels, 1);
    }

    #[test]
    fn starting_an_unauthenticated_session_is_reported() {
        let r = rig(FakeGreetd::new(vec![Step::password("pw")]));
        r.tx.send(Cmd::Start { cmd: vec!["x".into()], env: vec![] }).unwrap();
        assert!(matches!(r.next(), Event::Greetd(GreetdEvent::Failed { auth: false, .. })));
        r.create("alice");
        r.prompt(true, "Password: ");
        r.tx.send(Cmd::Start { cmd: vec!["x".into()], env: vec![] }).unwrap();
        let Event::Greetd(GreetdEvent::Failed { auth: false, text }) = r.next() else { panic!() };
        assert_eq!(text, "session is not authenticated", "greetd's own refusal is passed on");
        assert!(r.log.lock().unwrap().started.is_empty());
    }

    #[test]
    fn a_vanished_greetd_is_reported_once() {
        let (client, server) = UnixStream::pair().unwrap();
        let (etx, events) = mpsc::channel();
        let (tx, handle) = spawn(client, move |e| {
            let _ = etx.send(e);
        });
        drop(server);
        tx.send(Cmd::Create("alice".into())).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Disconnected(_)));
        handle.join().unwrap();
    }
}
