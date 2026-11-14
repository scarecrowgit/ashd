//! An in-process greetd: speaks the real wire protocol over a `UnixStream` pair and runs
//! scripted PAM-like conversations. Used by `--demo` (visual testing without greetd) and
//! by the tests of the greetd client.

use std::{
    collections::HashMap,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

use greetd_ipc::{AuthMessageType, ErrorType, Request, Response, codec::SyncCodec};

/// One step of a conversation.
#[derive(Clone, Debug)]
pub enum Step {
    /// A question; `expect` is the only accepted answer (`None` accepts anything).
    Prompt { secret: bool, text: String, expect: Option<String> },
    #[cfg_attr(not(test), allow(dead_code))]
    Info(String),
    #[cfg_attr(not(test), allow(dead_code))]
    Error(String),
}

impl Step {
    pub fn password(expect: &str) -> Self {
        Step::Prompt { secret: true, text: "Password: ".into(), expect: Some(expect.into()) }
    }
}

/// What the fake recorded.
#[derive(Debug, Default)]
pub struct Log {
    /// `(command, env)` of every accepted `start_session`.
    pub started: Vec<(Vec<String>, Vec<String>)>,
    /// Every `create_session` user name, in order.
    pub created: Vec<String>,
    pub cancels: usize,
}

pub struct FakeGreetd {
    flows: HashMap<String, Vec<Step>>,
    default_flow: Vec<Step>,
    /// Pause before answering an answer (PAM is not instant; shows the spinner).
    delay: Duration,
    log: Arc<Mutex<Log>>,
    session: Option<Session>,
}

struct Session {
    steps: Vec<Step>,
    pos: usize,
    /// The step greetd is waiting on.
    pending: bool,
}

impl FakeGreetd {
    pub fn new(default_flow: Vec<Step>) -> Self {
        Self { flows: HashMap::new(), default_flow, delay: Duration::ZERO, log: Arc::default(), session: None }
    }

    /// `--demo`: any user, password "demo", with a short pause so the spinner shows.
    pub fn demo() -> Self {
        Self::new(vec![Step::password("demo")]).with_delay(Duration::from_millis(900))
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn user(mut self, name: &str, flow: Vec<Step>) -> Self {
        self.flows.insert(name.into(), flow);
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn log(&self) -> Arc<Mutex<Log>> {
        self.log.clone()
    }

    fn advance(&mut self) -> Response {
        let Some(s) = self.session.as_mut() else {
            return Response::Error { error_type: ErrorType::Error, description: "no session".into() };
        };
        match s.steps.get(s.pos).cloned() {
            None => {
                s.pending = false;
                Response::Success
            }
            Some(step) => {
                s.pending = true;
                match step {
                    Step::Prompt { secret, text, .. } => Response::AuthMessage {
                        auth_message_type: if secret { AuthMessageType::Secret } else { AuthMessageType::Visible },
                        auth_message: text,
                    },
                    Step::Info(t) => Response::AuthMessage { auth_message_type: AuthMessageType::Info, auth_message: t },
                    Step::Error(t) => Response::AuthMessage { auth_message_type: AuthMessageType::Error, auth_message: t },
                }
            }
        }
    }

    pub fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::CreateSession { username } => {
                self.log.lock().unwrap().created.push(username.clone());
                let steps = self.flows.get(&username).unwrap_or(&self.default_flow).clone();
                self.session = Some(Session { steps, pos: 0, pending: false });
                self.advance()
            }
            Request::PostAuthMessageResponse { response } => {
                let Some(s) = self.session.as_mut().filter(|s| s.pending) else {
                    return Response::Error { error_type: ErrorType::Error, description: "no pending auth message".into() };
                };
                if !self.delay.is_zero() {
                    std::thread::sleep(self.delay);
                }
                if let Some(Step::Prompt { expect: Some(e), .. }) = s.steps.get(s.pos)
                    && response.as_deref() != Some(e.as_str())
                {
                    // like greetd: the failed session is gone
                    self.session = None;
                    return Response::Error { error_type: ErrorType::AuthError, description: "pam_authenticate: AUTH_ERR".into() };
                }
                s.pos += 1;
                self.advance()
            }
            Request::StartSession { cmd, env } => match &self.session {
                Some(s) if !s.pending && s.pos >= s.steps.len() => {
                    self.log.lock().unwrap().started.push((cmd, env));
                    self.session = None;
                    Response::Success
                }
                _ => Response::Error { error_type: ErrorType::Error, description: "session is not authenticated".into() },
            },
            Request::CancelSession => {
                self.log.lock().unwrap().cancels += 1;
                self.session = None;
                Response::Success
            }
        }
    }

    /// Serve `stream` until the client hangs up.
    pub fn serve(mut self, mut stream: UnixStream) {
        while let Ok(req) = Request::read_from(&mut stream) {
            let resp = self.handle(req);
            if resp.write_to(&mut stream).is_err() {
                break;
            }
        }
    }

    /// Start serving on a thread; returns the client end of the socket pair.
    pub fn spawn(self) -> (UnixStream, JoinHandle<()>) {
        let (client, server) = UnixStream::pair().expect("socketpair");
        let handle = std::thread::Builder::new().name("ashd-greeter-fake-greetd".into()).spawn(move || self.serve(server)).expect("spawn fake greetd");
        (client, handle)
    }
}
