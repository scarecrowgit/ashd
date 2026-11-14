//! `ashd-greeter`: the graphical login screen. A normal Wayland client (one fullscreen
//! surface per output, drawn with ashd-gfx / ashd-lockui in the lock screen's look) that
//! speaks to greetd on `$GREETD_SOCK`; greetd runs it inside a tiny compositor
//! (`modules/desktop/session.nix`).
//!
//! # Flow
//!
//! 1. The user name field starts with the last successful user (`$ASHD_GREETER_STATE`,
//!    default `/var/cache/ashd-greeter/state.json`), else `default_user` of
//!    `/etc/ashd/greeter.json`, else it is empty and focused.
//! 2. Enter opens a greetd conversation (`create_session`); greetd's questions appear in
//!    the prompt field (secret → dots, visible → clear text, info / error → message lines).
//! 3. After the last answer greetd says `success`: the screen fades out, the greeter
//!    sends `start_session` (`--cmd` or the selected session of the config, default
//!    `niri-session`) and exits 0. greetd then starts the session once the compositor
//!    that hosted the greeter has quit.
//! 4. Wrong password: shake + error line, the conversation restarts on the next Enter.
//!
//! # Failure behaviour
//!
//! Without greetd (`GREETD_SOCK` unset or unusable, connection lost) the screen shows
//! the error, still offers Reboot / Shut down, and the process exits 1 after
//! [`FATAL_DELAY`] — a long pause instead of a restart loop. The same wait precedes the
//! exit-1 of startup errors (no Wayland, no GPU).
//!
//! `--demo` runs without greetd against a built-in fake (any user, password "demo"; power
//! buttons only log) for visual testing.
//!
//! Exit status: 0 session started, 1 failure, 2 usage.

mod app;
mod config;
mod fake;
mod greetd;
mod keyboard;
mod logind;

use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use ashd_lockui::GreeterUi;
use smithay_client_toolkit::reexports::{
    calloop::{self, EventLoop},
    calloop_wayland_source::WaylandSource,
};
use wayland_client::{Connection, globals::registry_queue_init};

use crate::{
    app::{App, AppInit, Exit, Msg},
    config::{Config, SessionDef, State},
};

/// How long a screen that cannot log anybody in stays up before exiting 1.
pub const FATAL_DELAY: Duration = Duration::from_secs(20);

const USAGE: &str = "usage: ashd-greeter [--demo] [--cmd COMMAND] [--theme ash|ink|midnight|moss|dusk] [--config FILE]

Graphical greetd greeter. Run by greetd (inside a compositor); talks to $GREETD_SOCK.

  --demo          run without greetd against a built-in fake (any user, password \"demo\");
                  power buttons only log
  --cmd COMMAND   session command for start_session (shell-quoted); replaces the
                  sessions of the config file
  --theme NAME    theme (default: /etc/ashd/theme, else ash)
  --config FILE   greeter.json (default: /etc/ashd/greeter.json)

Environment: GREETD_SOCK, ASHD_GREETER_STATE (last-user file), ASHD_ETC_DIR,
ASHD_FAKE_POWER=1 / ASHD_NESTED=1 (power buttons only log).";

#[derive(Default)]
struct Args {
    demo: bool,
    cmd: Option<Vec<String>>,
    theme: Option<ashd_tokens::ThemeName>,
    config: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--demo" => out.demo = true,
            "--cmd" => {
                let text = args.next().ok_or("--cmd needs a command")?;
                let cmd = shlex::split(&text).filter(|c| !c.is_empty()).ok_or("--cmd: cannot parse the command")?;
                out.cmd = Some(cmd);
            }
            "--theme" => {
                let name = args.next().ok_or("--theme needs a name")?;
                out.theme = Some(ashd_tokens::ThemeName::from_label(&name).ok_or("--theme needs ash|ink|midnight|moss|dusk")?);
            }
            "--config" => out.config = Some(PathBuf::from(args.next().ok_or("--config needs a file")?)),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(out)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ashd_greeter=info,ashd_gfx=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut raw = std::env::args().skip(1).peekable();
    match raw.peek().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("ashd-greeter {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let args = match parse_args(raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ashd-greeter: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let demo = args.demo;

    match run(args) {
        Ok(Outcome::Started) => ExitCode::SUCCESS,
        Ok(Outcome::Fatal) => ExitCode::from(1),
        Err(e) => {
            // Startup failures (no Wayland, no GPU, …) happen before anything is on
            // screen. greetd restarts a greeter that exits at once, which would spin; wait.
            tracing::error!("{e:#}");
            eprintln!("ashd-greeter: {e:#}");
            if !demo {
                eprintln!("ashd-greeter: waiting {} s before giving up so greetd does not restart in a loop", FATAL_DELAY.as_secs());
                std::thread::sleep(FATAL_DELAY);
            }
            ExitCode::from(1)
        }
    }
}

enum Outcome {
    Started,
    /// The fatal screen was shown and its delay passed.
    Fatal,
}

fn hostname() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

/// The greetd connection, or why there is none.
fn connect_greetd(demo: bool) -> Result<std::os::unix::net::UnixStream, String> {
    if demo {
        let (client, _server) = fake::FakeGreetd::demo().spawn();
        tracing::info!("demo mode: fake greetd, any user, password \"demo\"");
        return Ok(client);
    }
    let path = std::env::var_os("GREETD_SOCK").filter(|p| !p.is_empty()).ok_or_else(|| {
        "GREETD_SOCK is not set: this greeter must be started by greetd".to_string()
    })?;
    greetd::connect(std::path::Path::new(&path)).map_err(|e| format!("cannot connect to greetd at {}: {e}", PathBuf::from(&path).display()))
}

fn run(args: Args) -> anyhow::Result<Outcome> {
    let demo = args.demo;
    let cfg_path = args.config.clone().unwrap_or_else(Config::default_path);
    let cfg = Config::load(&cfg_path);
    let sessions: Vec<SessionDef> = match args.cmd {
        Some(cmd) => vec![SessionDef::from_cmd(cmd)],
        None => cfg.sessions,
    };
    let state_path = config::state_path(demo);
    let state = state_path.as_deref().map(State::load).unwrap_or_default();
    let user = state.user.clone().filter(|u| !u.trim().is_empty()).or(cfg.default_user);
    let theme = args.theme.unwrap_or_else(config::system_theme);

    let mut ui = GreeterUi::new(theme, sessions.iter().map(|s| s.name.clone()).collect(), user);
    ui.lock.host = hostname();
    if let Some(i) = state.session.as_deref().and_then(|n| sessions.iter().position(|s| s.name == n)) {
        ui.set_session(i);
    }

    let conn = Connection::connect_to_env().context("connect to the Wayland display (WAYLAND_DISPLAY)")?;
    let (globals, event_queue) = registry_queue_init(&conn).context("Wayland registry")?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().context("event loop")?;

    let (msg_tx, msg_rx) = calloop::channel::channel::<Msg>();
    event_loop
        .handle()
        .insert_source(msg_rx, |ev, _, app: &mut App| {
            if let calloop::channel::Event::Msg(m) = ev {
                app.on_msg(m);
            }
        })
        .map_err(|e| anyhow::anyhow!("channel source: {e}"))?;

    let greetd_error;
    let cmd_tx = match connect_greetd(demo) {
        Ok(stream) => {
            greetd_error = None;
            let tx = msg_tx.clone();
            let (cmd_tx, _worker) = greetd::spawn(stream, move |e| {
                let _ = tx.send(Msg::Greetd(e));
            });
            Some(cmd_tx)
        }
        Err(e) => {
            greetd_error = Some(e);
            None
        }
    };
    let suspend_tx = msg_tx;
    let logind = logind::start(demo || logind::dry_run_from_env(), move |v| {
        let _ = suspend_tx.send(Msg::CanSuspend(v));
    });

    let mut app = App::new(AppInit {
        conn: conn.clone(),
        globals,
        qh,
        loop_handle: event_loop.handle(),
        ui,
        cmd_tx,
        logind,
        sessions,
        state_path,
    })?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;
    if let Some(e) = greetd_error {
        app.go_fatal(&e);
    }
    app.ensure_surfaces();

    let exit = loop {
        let retry = app.render_dirty();
        let now = Instant::now();
        let wake = app.ui.next_wakeup(now);
        let mut timeout = wake.map_or(Duration::from_secs(1), |w| w.saturating_duration_since(now)).min(Duration::from_secs(1));
        if retry {
            timeout = timeout.min(Duration::from_millis(16));
        }
        event_loop.dispatch(Some(timeout), &mut app).context("event loop")?;
        let now = Instant::now();
        if wake.is_some_and(|w| now >= w) {
            app.ui.tick(now);
            app.mark_all_dirty();
        }
        app.poll_start(now);
        if let Some(Exit::Started) = app.exit {
            break Outcome::Started;
        }
        if app.fatal_since.is_some_and(|t| now.duration_since(t) >= FATAL_DELAY) {
            tracing::error!("giving up after {} s without a usable greetd", FATAL_DELAY.as_secs());
            break Outcome::Fatal;
        }
    };
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> impl Iterator<Item = String> {
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn arguments() {
        let a = parse_args(args(&["--demo", "--cmd", "niri-session --flag 'a b'", "--theme", "dusk", "--config", "/x/g.json"])).unwrap();
        assert!(a.demo);
        assert_eq!(a.cmd.unwrap(), ["niri-session", "--flag", "a b"]);
        assert_eq!(a.theme, Some(ashd_tokens::ThemeName::Dusk));
        assert_eq!(a.config, Some(PathBuf::from("/x/g.json")));
        assert!(parse_args(args(&[])).unwrap().cmd.is_none());
        for bad in [&["--cmd"][..], &["--cmd", ""], &["--theme", "nope"], &["--bogus"], &["--config"]] {
            assert!(parse_args(args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn without_greetd_sock_there_is_a_clear_error() {
        // Only meaningful when the test environment has no GREETD_SOCK; if a developer
        // runs the tests under a real greetd the variable is set and the check is skipped.
        if std::env::var_os("GREETD_SOCK").is_some() {
            return;
        }
        let Err(e) = connect_greetd(false) else { panic!("expected an error") };
        assert!(e.contains("GREETD_SOCK is not set"), "{e}");
    }
}
