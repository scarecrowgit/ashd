//! `ashd-lock`: screen locker. ext-session-lock-v1 via smithay-client-toolkit (one lock
//! surface per output), UI from ashd-lockui drawn with ashd-gfx, authentication with
//! PAM (`pam-client`, service `ashd-lock`), passwords in `zeroize::Zeroizing`.
//! Spawned by ashd-shell (`Request::Lock`, logind `Lock` signal) through niri.
//!
//! # Safety properties
//!
//! - **Only a successful authentication unlocks.** `unlock_and_destroy` is sent in exactly
//!   one place, after PAM (or, in debug `test-auth` builds, the test password) accepted
//!   the password and the 300 ms fade finished.
//! - **Dying never unlocks.** SIGTERM, SIGINT, SIGKILL, a panic or any error exit leave
//!   the compositor locked: ext-session-lock-v1 keeps the session locked when the locker
//!   disconnects without `unlock_and_destroy` (niri then shows its own locked screen).
//!   No signal handler is installed on purpose; recover by running `ashd-lock` again (a
//!   new locker may take over a lock whose client died) and unlocking.
//! - If the compositor refuses the lock (another locker holds it) we get `finished`
//!   before `locked` and exit with status 1.
//! - Debug builds refuse to run unless `ASHD_NESTED=1` (the headless nested test session)
//!   so a development build can never lock a live desktop by accident. In that mode
//!   logind writes (LockedHint, suspend/reboot/power-off) are logged dry runs.
//!
//! Exit status: 0 unlocked (or the compositor ended the lock), 1 lock refused / error,
//! 2 usage / refused to start.

mod app;
mod keyboard;
mod logind;
mod pam;
mod status;

use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use ashd_lockui::LockUi;
use smithay_client_toolkit::reexports::{
    calloop::{self, EventLoop},
    calloop_wayland_source::WaylandSource,
};
use wayland_client::{Connection, globals::registry_queue_init};

use crate::app::{App, AppInit, Exit};

const USAGE: &str = "usage: ashd-lock [--theme ash|ink|midnight|moss|dusk]

Locks the Wayland session (ext-session-lock-v1) until the current user's password is
accepted by PAM (service `ashd-lock`). Killing ashd-lock does NOT unlock the session.";

enum Msg {
    Auth(pam::AuthEvent),
    CanSuspend(bool),
}

fn nested() -> bool {
    std::env::var("ASHD_NESTED").as_deref() == Ok("1")
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ashd_lock=info,ashd_gfx=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut theme = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("ashd-lock {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--theme" => match args.next().as_deref().and_then(ashd_tokens::ThemeName::from_label) {
                Some(t) => theme = Some(t),
                None => {
                    eprintln!("ashd-lock: --theme needs ash|ink|midnight|moss|dusk\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("ashd-lock: unknown argument `{other}`\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    // Development builds only ever run inside the headless nested session.
    #[cfg(debug_assertions)]
    if !nested() {
        eprintln!(
            "ashd-lock: debug build refuses to start without ASHD_NESTED=1 (run it only inside the nested \
             test session, `cargo xtask nested` — never on a live desktop)"
        );
        return ExitCode::from(2);
    }
    #[cfg(feature = "test-auth")]
    tracing::warn!("test-auth build: PAM is replaced by $ASHD_LOCK_TEST_PASSWORD");

    match run(theme) {
        Ok(Exit::Unlocked) => ExitCode::SUCCESS,
        Ok(Exit::FinishedByCompositor) => {
            eprintln!("ashd-lock: the compositor ended the lock");
            ExitCode::SUCCESS
        }
        Ok(Exit::Refused) => {
            eprintln!("ashd-lock: the compositor refused to lock the session (is another screen locker running?)");
            ExitCode::from(1)
        }
        Err(e) => {
            // Any error after `locked` leaves the session locked (fail secure).
            eprintln!("ashd-lock: {e:#}");
            ExitCode::from(1)
        }
    }
}

struct User {
    login: String,
    display: String,
}

fn current_user() -> anyhow::Result<User> {
    let uid = nix::unistd::getuid();
    let u = nix::unistd::User::from_uid(uid)?.with_context(|| format!("no passwd entry for uid {uid}"))?;
    let gecos = u.gecos.to_string_lossy();
    let full = gecos.split(',').next().unwrap_or("").trim().to_string();
    Ok(User { display: if full.is_empty() { u.name.clone() } else { full }, login: u.name })
}

fn run(theme: Option<ashd_tokens::ThemeName>) -> anyhow::Result<Exit> {
    let user = current_user()?;
    let theme = theme.unwrap_or_else(ashd_tokens::state::current_theme);
    let mut ui = LockUi::new(theme, user.login.clone());
    ui.display_name = user.display;
    ui.host = rustix::system::uname().nodename().to_string_lossy().into_owned();
    ui.compositor = if std::env::var_os("NIRI_SOCKET").is_some() {
        "niri".into()
    } else {
        std::env::var("XDG_CURRENT_DESKTOP").map(|d| d.to_lowercase()).unwrap_or_else(|_| "wayland".into())
    };
    ui.status = status::read();

    let conn = Connection::connect_to_env().context("connect to the Wayland display")?;
    let (globals, event_queue) = registry_queue_init(&conn).context("Wayland registry")?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().context("event loop")?;

    let (msg_tx, msg_rx) = calloop::channel::channel::<Msg>();
    event_loop
        .handle()
        .insert_source(msg_rx, |ev, _, app: &mut App| match ev {
            calloop::channel::Event::Msg(Msg::Auth(a)) => app.on_auth(a),
            calloop::channel::Event::Msg(Msg::CanSuspend(v)) => {
                app.ui.can_suspend = v;
                app.mark_all_dirty();
            }
            calloop::channel::Event::Closed => {}
        })
        .map_err(|e| anyhow::anyhow!("channel source: {e}"))?;
    let auth_reply = msg_tx.clone();
    let auth_tx = pam::spawn_worker(user.login, move |ev| {
        let _ = auth_reply.send(Msg::Auth(ev));
    })?;
    let suspend_reply = msg_tx;
    let (logind, logind_thread) = logind::start(nested(), move |v| {
        let _ = suspend_reply.send(Msg::CanSuspend(v));
    });

    let mut app = App::new(AppInit {
        conn: conn.clone(),
        globals,
        qh,
        loop_handle: event_loop.handle(),
        ui,
        auth_tx,
        logind,
    })?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;

    app.lock()?;
    tracing::info!("lock requested");

    let mut last_status = Instant::now();
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
        if now.duration_since(last_status) >= Duration::from_secs(60) {
            last_status = now;
            app.ui.status = status::read();
            app.mark_all_dirty();
        }
        if let Some(exit) = app.exit_requested() {
            break exit;
        }
        if app.ui.unlock_ready(now) && app.is_locked() {
            app.unlock();
            tracing::info!("session unlocked");
            break Exit::Unlocked;
        }
        if !app.has_lock() {
            break Exit::Refused;
        }
    };
    if exit != Exit::Refused {
        app.logind.send(logind::Command::SetLockedHint(false));
    }
    let App { logind, .. } = app;
    logind.finish(logind_thread, Duration::from_secs(2));
    Ok(exit)
}
