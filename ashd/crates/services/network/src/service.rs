//! The service loop: backend selection, the read-only tick loop and the NetworkManager
//! loop.

use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use futures_lite::StreamExt as _;
use smol::future::or;
use zbus::Connection;

use crate::{
    Options,
    engine::{Core, Tick},
    model::{Command, NetworkState, SAMPLE_INTERVAL, SLOW_INTERVAL},
    nm::{self, Commands, NmSnapshot, Overlay},
    sysfs,
};

/// How the NetworkManager backend gets its bus.
pub enum NmMode {
    /// Look for NetworkManager on the system bus, now and every 30 s.
    Auto,
    /// Never use NetworkManager.
    Off,
    /// Use this connection (a peer-to-peer one to a mock, in tests).
    Connection(Connection),
}

enum Exit {
    /// The command channel closed: the shell is gone.
    Closed,
    /// NetworkManager appeared / disappeared: pick the backend again.
    Rechoose,
}

fn wall() -> (String, u64) {
    let now = chrono::Local::now();
    (now.format("%Y-%m-%d").to_string(), now.timestamp().max(0) as u64)
}

pub(crate) async fn run(commands: Receiver<Command>, state: Sender<NetworkState>, opts: Options) {
    let fake = opts.fake_commands;
    if let Some(name) = opts.mock.as_deref() {
        crate::mock::run(name, commands, state).await;
        return;
    }
    let mut core = match opts.addrs {
        Some(a) => Core::new(opts.paths, a, opts.usage_file),
        None => Core::new(opts.paths, Box::new(sysfs::system_addrs), opts.usage_file),
    };
    let mut sampling = false;
    let mut nm_conn: Option<Connection> = match &opts.nm {
        NmMode::Connection(c) => Some(c.clone()),
        _ => None,
    };
    let auto = matches!(opts.nm, NmMode::Auto);
    loop {
        // Pick a backend.
        if auto && nm_conn.is_none() {
            nm_conn = Connection::system().await.ok();
        }
        let use_nm = match &nm_conn {
            // A given (peer-to-peer) connection has no bus to ask: it is NetworkManager.
            Some(_) if matches!(opts.nm, NmMode::Connection(_)) => true,
            Some(c) if matches!(opts.nm, NmMode::Auto) => nm::present(c).await,
            _ => false,
        };
        let exit = if use_nm {
            let c = nm_conn.clone().expect("checked above");
            match run_nm(&c, &commands, &state, &mut core, &mut sampling, fake, opts.safe_apply).await {
                Ok(exit) => exit,
                Err(e) => {
                    tracing::debug!("network: NetworkManager backend ended: {e}");
                    // Fall back to the read-only picture; try NM again later.
                    Exit::Rechoose
                }
            }
        } else {
            run_readonly(&commands, &state, &mut core, &mut sampling, auto.then_some(&nm_conn)).await
        };
        match exit {
            Exit::Closed => {
                core.usage.flush();
                return;
            }
            Exit::Rechoose => {
                // Brief pause so a flapping NM cannot spin the loop.
                smol::Timer::after(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Wait for a command or the deadline.
async fn next_event(commands: &Receiver<Command>, wake: Instant) -> Option<Option<Command>> {
    or(
        async { Some(commands.recv().await.ok()) },
        async {
            smol::Timer::at(wake).await;
            None
        },
    )
    .await
}

async fn run_readonly(
    commands: &Receiver<Command>,
    state: &Sender<NetworkState>,
    core: &mut Core,
    sampling: &mut bool,
    nm_watch: Option<&Option<Connection>>,
) -> Exit {
    let mut last = NetworkState::default();
    let mut last_probe = Instant::now();
    let publish = |core: &mut Core, sampling: bool, last: &mut NetworkState| {
        let (date, unix) = wall();
        let s = core.tick(&Tick { now: Instant::now(), date: &date, unix, sampling });
        if s != *last {
            *last = s.clone();
            let _ = state.force_send(s);
        }
    };
    publish(core, *sampling, &mut last);
    let mut wake = Instant::now() + interval(*sampling);
    loop {
        match next_event(commands, wake).await {
            Some(None) => return Exit::Closed,
            Some(Some(cmd)) => {
                match cmd {
                    Command::SetSampling(on) => *sampling = on,
                    Command::Refresh => {}
                    other => {
                        tracing::warn!("network: {other:?} needs NetworkManager (read-only backend)");
                        let mut s = last.clone();
                        s.error = Some("needs NetworkManager".into());
                        let _ = state.force_send(s);
                        continue;
                    }
                }
                publish(core, *sampling, &mut last);
                wake = Instant::now() + interval(*sampling);
            }
            None => {
                publish(core, *sampling, &mut last);
                wake += interval(*sampling);
                if wake < Instant::now() {
                    wake = Instant::now() + interval(*sampling);
                }
                // Cheap check for NetworkManager arriving later (one D-Bus call).
                if let Some(conn) = nm_watch
                    && last_probe.elapsed() >= Duration::from_secs(30)
                {
                    last_probe = Instant::now();
                    if let Some(c) = conn
                        && nm::present(c).await
                    {
                        return Exit::Rechoose;
                    }
                }
            }
        }
    }
}

fn interval(sampling: bool) -> Duration {
    if sampling { SAMPLE_INTERVAL } else { SLOW_INTERVAL }
}

/// Bookkeeping that only the service knows.
#[derive(Default)]
struct Live {
    ov: Overlay,
    connecting_since: Option<Instant>,
    scan_until: Option<Instant>,
    last_scan: i64,
    /// The trial profile created by `ApplyStatic` (deleted on revert).
    trial_path: Option<String>,
    revert_uuid: Option<String>,
}

const SCAN_MAX: Duration = Duration::from_secs(8);
const CONNECT_MAX: Duration = Duration::from_secs(45);

async fn run_nm(
    conn: &Connection,
    commands: &Receiver<Command>,
    state: &Sender<NetworkState>,
    core: &mut Core,
    sampling: &mut bool,
    fake: bool,
    safe_apply: Duration,
) -> zbus::Result<Exit> {
    let mut nm_snap: NmSnapshot = nm::read_snapshot(conn).await?;
    let mut live = Live::default();
    let mut last = NetworkState::default();

    // NetworkManager's property changes (device states, access points, profiles).
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build();
    let mut signals = zbus::MessageStream::for_match_rule(rule, conn, Some(32)).await?;

    let mut dirty = false;
    let publish = |core: &mut Core, nm_snap: &NmSnapshot, live: &Live, sampling: bool, last: &mut NetworkState| {
        let (date, unix) = wall();
        let mut s = core.tick(&Tick { now: Instant::now(), date: &date, unix, sampling });
        nm::overlay(&mut s, nm_snap, &live.ov);
        if s != *last {
            *last = s.clone();
            let _ = state.force_send(s);
        }
    };
    publish(core, &nm_snap, &live, *sampling, &mut last);
    let mut wake = Instant::now() + interval(*sampling);

    loop {
        enum Ev {
            Cmd(Option<Command>),
            Sig(bool),
            Timer,
        }
        let ev = or(
            or(
                async { Ev::Cmd(commands.recv().await.ok()) },
                async {
                    // A closed stream means the bus connection is gone.
                    Ev::Sig(signals.next().await.is_some())
                },
            ),
            async {
                smol::Timer::at(wake).await;
                Ev::Timer
            },
        )
        .await;

        let now = Instant::now();
        let mut refresh_nm = false;
        match ev {
            Ev::Cmd(None) => return Ok(Exit::Closed),
            Ev::Sig(false) => return Ok(Exit::Rechoose),
            Ev::Sig(true) => {
                // Coalesce bursts (access point strengths change constantly).
                dirty = true;
                wake = wake.min(now + Duration::from_millis(200));
                continue;
            }
            Ev::Cmd(Some(cmd)) => match cmd {
                Command::SetSampling(on) => *sampling = on,
                Command::Refresh => refresh_nm = true,
                cmd if fake => {
                    tracing::warn!(target: "ashd_svc_network::fake", "ASHD_NESTED: would run {cmd:?}");
                    continue;
                }
                cmd => {
                    execute(cmd, conn, &nm_snap, &mut live, now, safe_apply).await;
                    refresh_nm = true;
                    dirty = false;
                }
            },
            Ev::Timer => {
                // The slow poll re-reads NetworkManager even without signals.
                refresh_nm = dirty || !*sampling;
                dirty = false;
            }
        }

        // The safe-apply countdown ran out: put the previous profile back.
        if let Some(p) = live.ov.pending_apply.clone()
            && now >= p.deadline
        {
            revert(conn, &nm_snap, &mut live).await;
            refresh_nm = true;
        }
        if live.scan_until.is_some_and(|t| now >= t) {
            live.scan_until = None;
            live.ov.scanning = false;
        }
        if refresh_nm {
            match nm::read_snapshot(conn).await {
                Ok(s) => nm_snap = s,
                Err(e) => return Err(e),
            }
            settle(&mut live, &nm_snap, now);
        }
        publish(core, &nm_snap, &live, *sampling, &mut last);

        wake = now + interval(*sampling);
        if let Some(p) = &live.ov.pending_apply {
            wake = wake.min(p.deadline);
        }
        if let Some(t) = live.scan_until {
            wake = wake.min(t).min(now + Duration::from_secs(1));
        }
    }
}

/// Clear transient markers that the new snapshot resolves.
fn settle(live: &mut Live, nm: &NmSnapshot, now: Instant) {
    if let Some(w) = &nm.wifi {
        if w.last_scan != live.last_scan {
            if live.last_scan != 0 {
                live.scan_until = None;
                live.ov.scanning = false;
            }
            live.last_scan = w.last_scan;
        }
        if live.ov.connecting.is_some() {
            let since = live.connecting_since.unwrap_or(now);
            match w.state {
                nm::DEV_ACTIVATED => {
                    live.ov.connecting = None;
                    live.connecting_since = None;
                }
                nm::DEV_FAILED => {
                    live.ov.failed = Some("could not connect — wrong password?".into());
                    live.ov.connecting = None;
                    live.connecting_since = None;
                }
                _ if now.duration_since(since) > CONNECT_MAX => {
                    live.ov.failed = Some("connection timed out".into());
                    live.ov.connecting = None;
                    live.connecting_since = None;
                }
                _ => {}
            }
        }
    }
}

async fn revert(conn: &Connection, nm_snap: &NmSnapshot, live: &mut Live) {
    let cmds = Commands { conn, nm: nm_snap };
    if let Some(uuid) = live.revert_uuid.take()
        && let Err(e) = cmds.activate_profile(&uuid).await
    {
        tracing::warn!("network: revert failed: {e}");
        live.ov.error = Some(e);
    }
    if let Some(trial) = live.trial_path.take() {
        cmds.delete_path(&trial).await;
    }
    live.ov.pending_apply = None;
}

async fn execute(cmd: Command, conn: &Connection, nm_snap: &NmSnapshot, live: &mut Live, now: Instant, safe_apply: Duration) {
    let cmds = Commands { conn, nm: nm_snap };
    live.ov.error = None;
    let result: Result<(), String> = match cmd {
        Command::SetSampling(_) | Command::Refresh => Ok(()),
        Command::ConnectDevice { ifname } => cmds.connect_device(&ifname).await,
        Command::DisconnectDevice { ifname } => cmds.disconnect_device(&ifname).await,
        Command::ActivateProfile { uuid } => cmds.activate_profile(&uuid).await,
        Command::Reapply { ifname } => {
            // Re-activate whatever is active on the device: a fresh DHCP exchange.
            match nm_snap.devices.iter().find(|d| d.ifname == ifname).and_then(|d| d.active_uuid.clone()) {
                Some(uuid) => cmds.activate_profile(&uuid).await,
                None => cmds.connect_device(&ifname).await,
            }
        }
        Command::ForgetProfile { uuid } => cmds.forget_profile(&uuid).await,
        Command::ApplyStatic(cfg) => {
            let previous = nm_snap.devices.iter().find(|d| d.ifname == cfg.ifname).and_then(|d| d.active_uuid.clone());
            match cmds.apply_static(&cfg).await {
                Ok(path) => {
                    live.trial_path = Some(path);
                    live.revert_uuid = previous.clone();
                    let prev_name = previous
                        .as_deref()
                        .and_then(|u| nm_snap.profiles.iter().find(|(_, p)| p.uuid == u))
                        .map(|(_, p)| p.id.clone());
                    live.ov.pending_apply = Some(nm::pending(&cfg.name, prev_name, now, safe_apply));
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        Command::KeepApply => {
            live.ov.pending_apply = None;
            live.trial_path = None;
            live.revert_uuid = None;
            Ok(())
        }
        Command::RevertApply => {
            revert(conn, nm_snap, live).await;
            Ok(())
        }
        Command::WifiSetEnabled(on) => cmds.wifi_set_enabled(on).await,
        Command::WifiScan => {
            let r = cmds.wifi_scan().await;
            if r.is_ok() {
                live.ov.scanning = true;
                live.scan_until = Some(now + SCAN_MAX);
            }
            r
        }
        Command::WifiConnect { ssid, password } => {
            live.ov.failed = None;
            live.ov.connecting = Some(ssid.clone());
            live.connecting_since = Some(now);
            let r = cmds.wifi_connect(&ssid, password.as_ref()).await;
            // `password` (Zeroizing) is dropped here.
            if r.is_err() {
                live.ov.connecting = None;
                live.connecting_since = None;
            }
            r
        }
        Command::WifiDisconnect => cmds.wifi_disconnect().await,
    };
    if let Err(e) = result {
        tracing::warn!("network: command failed: {e}");
        live.ov.error = Some(e);
    }
}
