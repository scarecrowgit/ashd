//! The headless nested test session: sway (headless wlroots backend) → niri (winit) →
//! your clients. Same root layout and
//! environment contract (`source "$root/env"` keeps working), plus robust teardown and
//! environment scrubbing.
//!
//! Layout of a session root (`/tmp/ashd-nested.XXXXXX`, mode 0700, also the private
//! `XDG_RUNTIME_DIR`): `sway.conf niri.kdl sway.log niri.log sway.pid niri.pid env
//! home/ ashd-state/ wayland-1 (sway) wayland-2 (niri) niri.wayland-N.PID.sock`, with
//! `--with-bus` also `dbus.conf bus dbus.pid dbus.env`.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    sync::{
        Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use nix::{
    sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal},
    unistd::Pid,
};
use ashd_tokens::ThemeName;

use crate::util::{self, log_tail, pid_alive, pgid_of, resolve_tool, wait_until};

pub const ROOT_PREFIX: &str = "ashd-nested.";
const NIRI_KDL: &str = include_str!("../assets/nested-niri.kdl");
/// The user's `--niri-config` text, kept in the root so a theme switch can re-theme it.
const NIRI_BASE: &str = "niri.base.kdl";

/// Variables of the live desktop (or of services reachable through it) that must never
/// reach anything started in a session.
pub const SCRUBBED_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "DISPLAY",
    "XAUTHORITY",
    "NIRI_SOCKET",
    "SWAYSOCK",
    "I3SOCK",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "DBUS_SESSION_BUS_ADDRESS",
    "DBUS_STARTER_ADDRESS",
    "DBUS_STARTER_BUS_TYPE",
    "ASHD_SOCKET",
    "PULSE_SERVER",
    "PULSE_RUNTIME_PATH",
    "PULSE_STATE_PATH",
    "PULSE_COOKIE",
    "PIPEWIRE_REMOTE",
    "PIPEWIRE_RUNTIME_DIR",
    "XDG_ACTIVATION_TOKEN",
    "DESKTOP_STARTUP_ID",
];

#[derive(Subcommand)]
pub enum Cmd {
    /// Start a session and print its root (`root=$(cargo xtask nested start)`).
    Start {
        /// Size of the headless output.
        #[arg(long, default_value = "1920x1080")]
        size: String,
        /// Also start a private `dbus-daemon --session` (exports DBUS_SESSION_BUS_ADDRESS).
        #[arg(long)]
        with_bus: bool,
        /// Theme whose colours the nested niri uses for background / focus ring.
        #[arg(long, default_value = "ash")]
        theme: ThemeName,
        /// Use this niri config instead of the built-in one (theme colours are still
        /// substituted). Needed for the fork's `workspace "scratch" { hidden }`, which stock
        /// niri rejects.
        #[arg(long)]
        niri_config: Option<PathBuf>,
    },
    /// Stop a session (kills everything started in it) and remove its root.
    Stop {
        root: PathBuf,
        /// Keep the root directory (logs, screenshots).
        #[arg(long)]
        keep: bool,
    },
    /// Print `export …` lines for a session (`eval "$(cargo xtask nested env $root)"`).
    Env { root: PathBuf },
    /// Start a session, run a command inside it, tear the session down.
    Run {
        #[arg(long, default_value = "1920x1080")]
        size: String,
        #[arg(long)]
        with_bus: bool,
        #[arg(long, default_value = "ash")]
        theme: ThemeName,
        /// Use this niri config instead of the built-in one (see `nested start`).
        #[arg(long)]
        niri_config: Option<PathBuf>,
        /// Keep the session root (and print it) instead of removing it.
        #[arg(long)]
        keep: bool,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// List session roots and whether they are alive.
    List,
    /// Remove roots of dead sessions.
    Gc,
}

pub fn dispatch(cmd: Cmd) -> Result<ExitCode> {
    match cmd {
        Cmd::Start { size, with_bus, theme, niri_config } => {
            let opts = StartOpts { size: parse_size(&size)?, with_bus, theme, die_with_parent: false, niri_config };
            let session = Session::start(&opts)?;
            let root = session.detach();
            println!("{}", root.display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Stop { root, keep } => {
            let root = checked_root(&root)?;
            kill_session(&root, &mut []);
            if !keep {
                remove_root(&root)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Env { root } => {
            let root = checked_root(&root)?;
            let text = fs::read_to_string(root.join("env")).with_context(|| format!("{}/env", root.display()))?;
            print!("{text}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Run { size, with_bus, theme, niri_config, keep, cmd } => {
            run_command(&size, with_bus, theme, niri_config, keep, &cmd)
        }
        Cmd::List => {
            list();
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Gc => {
            let removed = gc();
            println!("removed {} stale session root(s)", removed.len());
            Ok(ExitCode::SUCCESS)
        }
    }
}

pub fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once('x').with_context(|| format!("size `{s}`: expected WxH"))?;
    let (w, h): (u32, u32) = (w.parse()?, h.parse()?);
    if w < 64 || h < 64 {
        bail!("size `{s}` is too small");
    }
    Ok((w, h))
}

#[derive(Clone, Debug)]
pub struct StartOpts {
    pub size: (u32, u32),
    pub with_bus: bool,
    pub theme: ThemeName,
    /// Children get PR_SET_PDEATHSIG(SIGKILL): they die with xtask even if xtask is
    /// SIGKILLed. Off for `nested start` (the session must outlive the command).
    pub die_with_parent: bool,
    /// A niri config to use instead of the built-in one (`--niri-config`).
    pub niri_config: Option<PathBuf>,
}

impl Default for StartOpts {
    fn default() -> Self {
        Self { size: (1920, 1080), with_bus: false, theme: ThemeName::Ash, die_with_parent: true, niri_config: None }
    }
}

/// A running (or attached) nested session.
pub struct Session {
    pub root: PathBuf,
    pub wayland_display: String,
    pub niri_socket: PathBuf,
    /// sway's socket (the headless parent), for `grim` of the whole output.
    pub parent_display: String,
    pub bus: Option<String>,
    children: Vec<Child>,
    /// Tear down and remove the root on drop.
    guard: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.guard {
            self.teardown();
        }
    }
}

// ── environment ───────────────────────────────────────────────────────────────

/// Remove the live-desktop variables from `cmd`.
pub fn scrub(cmd: &mut Command) -> &mut Command {
    for v in SCRUBBED_ENV {
        cmd.env_remove(v);
    }
    cmd
}

fn base_env(root: &Path) -> Vec<(&'static str, String)> {
    let r = root.display();
    vec![
        ("XDG_RUNTIME_DIR", r.to_string()),
        ("HOME", format!("{r}/home")),
        ("XDG_CONFIG_HOME", format!("{r}/home/.config")),
        ("XDG_STATE_HOME", format!("{r}/home/.local/state")),
        ("XDG_DATA_HOME", format!("{r}/home/.local/share")),
    ]
}

impl Session {
    /// The environment of everything that runs inside the session (also the content of
    /// `$root/env`, minus the `unset` line).
    pub fn env_vars(&self) -> Vec<(String, String)> {
        let r = self.root.display();
        let mut v: Vec<(String, String)> = vec![
            ("XDG_RUNTIME_DIR".into(), r.to_string()),
            ("WAYLAND_DISPLAY".into(), self.wayland_display.clone()),
            ("NIRI_SOCKET".into(), self.niri_socket.display().to_string()),
            ("ASHD_STATE_DIR".into(), format!("{r}/ashd-state")),
            ("HOME".into(), format!("{r}/home")),
            ("XDG_CONFIG_HOME".into(), format!("{r}/home/.config")),
            ("XDG_STATE_HOME".into(), format!("{r}/home/.local/state")),
            ("XDG_DATA_HOME".into(), format!("{r}/home/.local/share")),
            ("ASHD_NESTED".into(), "1".into()),
            ("ASHD_FAKE_POWER".into(), "1".into()),
            // Theme switching must not signal the user's real kitty / niri.
            ("ASHD_NO_RELOAD".into(), "1".into()),
            // Nothing in the session may reach the real audio server.
            ("PULSE_SERVER".into(), format!("unix:{r}/no-pulse")),
        ];
        if let Some(bus) = &self.bus {
            v.push(("DBUS_SESSION_BUS_ADDRESS".into(), bus.clone()));
        }
        v.push(("ASHD_PARENT_WAYLAND_DISPLAY".into(), self.parent_display.clone()));
        v
    }

    /// Scrub the live variables, then apply the session environment.
    pub fn apply_env(&self, cmd: &mut Command) {
        scrub(cmd);
        for (k, v) in self.env_vars() {
            cmd.env(k, v);
        }
    }

    /// A `Command` for `program` that runs inside the session.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        self.apply_env(&mut c);
        c
    }

    /// Contents of `$root/env`.
    pub fn env_text(&self) -> String {
        let exported: Vec<String> = self.env_vars().into_iter().map(|(k, _)| k).collect();
        let unset: Vec<&str> = SCRUBBED_ENV.iter().copied().filter(|v| !exported.iter().any(|e| e == v)).collect();
        let mut s = format!("unset {}\n", unset.join(" "));
        for (k, v) in self.env_vars() {
            if k == "ASHD_PARENT_WAYLAND_DISPLAY" {
                s.push_str("# sway (parent) socket, for grim of the whole headless output:\n");
            }
            s.push_str(&format!("export {k}='{v}'\n"));
        }
        s
    }

    /// Re-attach to a root created earlier (`nested start`).
    pub fn attach(root: &Path) -> Result<Session> {
        let root = checked_root(root)?;
        let text = fs::read_to_string(root.join("env")).with_context(|| format!("{}/env", root.display()))?;
        let vars = parse_env_file(&text);
        let get = |k: &str| vars.get(k).cloned().with_context(|| format!("{}/env has no {k}", root.display()));
        Ok(Session {
            wayland_display: get("WAYLAND_DISPLAY")?,
            niri_socket: PathBuf::from(get("NIRI_SOCKET")?),
            parent_display: vars.get("ASHD_PARENT_WAYLAND_DISPLAY").cloned().unwrap_or_else(|| "wayland-1".into()),
            bus: vars.get("DBUS_SESSION_BUS_ADDRESS").cloned(),
            root,
            children: Vec::new(),
            guard: false,
        })
    }

    // ── lifecycle ─────────────────────────────────────────────────────────────

    pub fn start(opts: &StartOpts) -> Result<Session> {
        install_signal_handlers();
        let sway = resolve_tool("SWAY", "sway", "sway")?;
        let niri = resolve_tool("NIRI", "niri", "niri")?;
        let dbus = if opts.with_bus { Some(resolve_tool("DBUS_DAEMON", "dbus-daemon", "dbus")?) } else { None };

        let root = make_root()?;
        let mut s = Session {
            root,
            wayland_display: String::new(),
            niri_socket: PathBuf::new(),
            parent_display: String::new(),
            bus: None,
            children: Vec::new(),
            guard: true,
        };
        register_root(&s.root);
        if let Err(e) = s.boot(opts, &sway, &niri, dbus.as_deref()) {
            let logs = format!(
                "--- sway.log ---\n{}\n--- niri.log ---\n{}",
                log_tail(&s.root.join("sway.log"), 15),
                log_tail(&s.root.join("niri.log"), 15)
            );
            return Err(e.context(logs));
        }
        Ok(s)
    }

    fn boot(&mut self, opts: &StartOpts, sway: &Path, niri: &Path, dbus: Option<&Path>) -> Result<()> {
        let root = self.root.clone();
        for d in ["home/.config", "home/.local/state", "home/.local/share", "ashd-state"] {
            fs::create_dir_all(root.join(d))?;
        }
        let (w, h) = opts.size;
        fs::write(
            root.join("sway.conf"),
            format!(
                "output HEADLESS-1 mode {w}x{h} position 0 0\ndefault_border none\ndefault_floating_border none\n\
                 gaps inner 0\ngaps outer 0\nfocus_follows_mouse no\n"
            ),
        )?;
        let base = match &opts.niri_config {
            Some(path) => {
                let text = fs::read_to_string(path).with_context(|| format!("reading niri config {}", path.display()))?;
                fs::write(root.join(NIRI_BASE), &text)?;
                text
            }
            None => NIRI_KDL.to_string(),
        };
        fs::write(root.join("niri.kdl"), themed_niri_config(&base, opts.theme))?;

        if let Some(dbus) = dbus {
            self.start_bus(dbus, opts)?;
        }

        // sway (headless wlroots backend) — always the first wayland-N socket of the root.
        let mut cmd = Command::new(sway);
        cmd.arg("-c").arg(root.join("sway.conf"));
        scrub(&mut cmd);
        for (k, v) in base_env(&root) {
            cmd.env(k, v);
        }
        cmd.env("WLR_BACKENDS", "headless").env("WLR_LIBINPUT_NO_DEVICES", "1").env("WLR_HEADLESS_OUTPUTS", "1");
        let mut sway_child = spawn_logged(cmd, &root.join("sway.log"), opts.die_with_parent)?;
        fs::write(root.join("sway.pid"), format!("{}\n", sway_child.id()))?;
        let parent = wait_until(Duration::from_secs(15), Duration::from_millis(50), || {
            if let Ok(Some(st)) = sway_child.try_wait() {
                return Some(Err(anyhow::anyhow!("sway exited early ({st})")));
            }
            wayland_sockets(&root).into_iter().next().map(Ok)
        });
        self.children.push(sway_child);
        self.parent_display = match parent {
            Some(r) => r?,
            None => bail!("sway did not start (no wayland socket in {})", root.display()),
        };

        // niri (winit backend) inside sway.
        let mut cmd = Command::new(niri);
        cmd.arg("-c").arg(root.join("niri.kdl"));
        scrub(&mut cmd);
        for (k, v) in base_env(&root) {
            cmd.env(k, v);
        }
        if let Some(bus) = &self.bus {
            cmd.env("DBUS_SESSION_BUS_ADDRESS", bus);
        }
        cmd.env("WAYLAND_DISPLAY", &self.parent_display);
        cmd.env("RUST_LOG", std::env::var("NIRI_LOG").unwrap_or_else(|_| "niri=info".into()));
        let mut niri_child = spawn_logged(cmd, &root.join("niri.log"), opts.die_with_parent)?;
        fs::write(root.join("niri.pid"), format!("{}\n", niri_child.id()))?;
        let sock = wait_until(Duration::from_secs(60), Duration::from_millis(50), || {
            if let Ok(Some(st)) = niri_child.try_wait() {
                return Some(Err(anyhow::anyhow!("niri exited early ({st})")));
            }
            niri_sockets(&root).into_iter().next().map(Ok)
        });
        self.children.push(niri_child);
        let sock = match sock {
            Some(r) => r?,
            None => bail!("niri did not start (no IPC socket in {})", root.display()),
        };
        self.wayland_display =
            niri_display_of(&sock).with_context(|| format!("cannot parse the display from {}", sock.display()))?;
        self.niri_socket = sock;

        fs::write(root.join("env"), self.env_text())?;
        Ok(())
    }

    fn start_bus(&mut self, dbus: &Path, opts: &StartOpts) -> Result<()> {
        let root = self.root.clone();
        let addr = format!("unix:path={}/bus", root.display());
        // A minimal config without service directories: nothing on the host can be
        // bus-activated inside the session.
        fs::write(
            root.join("dbus.conf"),
            format!(
                "<busconfig>\n<type>session</type>\n<listen>{addr}</listen>\n<auth>EXTERNAL</auth>\n\
                 <policy context=\"default\">\n<allow send_destination=\"*\" eavesdrop=\"true\"/>\n\
                 <allow eavesdrop=\"true\"/>\n<allow own=\"*\"/>\n</policy>\n</busconfig>\n"
            ),
        )?;
        let mut cmd = Command::new(dbus);
        cmd.arg("--config-file").arg(root.join("dbus.conf")).arg("--nofork");
        scrub(&mut cmd);
        for (k, v) in base_env(&root) {
            cmd.env(k, v);
        }
        let mut child = spawn_logged(cmd, &root.join("dbus.log"), opts.die_with_parent)?;
        fs::write(root.join("dbus.pid"), format!("{}\n", child.id()))?;
        let ready = wait_until(Duration::from_secs(10), Duration::from_millis(30), || {
            if let Ok(Some(st)) = child.try_wait() {
                return Some(Err(anyhow::anyhow!("dbus-daemon exited early ({st})")));
            }
            fs::metadata(root.join("bus")).ok().filter(|m| m.file_type().is_socket()).map(|_| Ok(()))
        });
        self.children.push(child);
        match ready {
            Some(r) => r?,
            None => bail!("dbus-daemon did not create {}/bus; see dbus.log", root.display()),
        }
        fs::write(root.join("dbus.env"), format!("export DBUS_SESSION_BUS_ADDRESS='{addr}'\n"))?;
        self.bus = Some(addr);
        Ok(())
    }

    /// Kill everything started in the session and remove the root.
    pub fn teardown(&mut self) {
        self.guard = false;
        kill_session(&self.root, &mut self.children);
        let _ = remove_root(&self.root);
        unregister_root(&self.root);
    }

    /// Give up ownership: the session keeps running, the root stays.
    pub fn detach(mut self) -> PathBuf {
        self.guard = false;
        unregister_root(&self.root);
        self.children.clear();
        self.root.clone()
    }

    /// Stop everything but keep the root (logs, screenshots).
    pub fn stop_keep(&mut self) {
        self.guard = false;
        kill_session(&self.root, &mut self.children);
        unregister_root(&self.root);
    }

    /// Rewrite niri's config for `theme` (niri live-reloads it).
    pub fn set_niri_theme(&self, theme: ThemeName) -> Result<()> {
        let base = fs::read_to_string(self.root.join(NIRI_BASE)).unwrap_or_else(|_| NIRI_KDL.to_string());
        fs::write(self.root.join("niri.kdl"), themed_niri_config(&base, theme)).context("rewriting niri.kdl")
    }
}

/// The nested niri config with the colours of `theme` (Ash = the checked-in file as is).
#[cfg(test)]
pub fn niri_config(theme: ThemeName) -> String {
    themed_niri_config(NIRI_KDL, theme)
}

/// [`niri_config`] for any base config text (the Ash colours in it are swapped for `theme`'s).
pub fn themed_niri_config(base: &str, theme: ThemeName) -> String {
    let ash = ashd_tokens::tokens(ThemeName::Ash);
    let t = ashd_tokens::tokens(theme);
    base
        .replace("#111110", &t.colors.bg.hex())
        .replace(&ash.colors.accent.hex(), &t.colors.accent.hex())
        .replace(&ash.colors.frame.hex(), &t.colors.frame.hex())
}

fn make_root() -> Result<PathBuf> {
    // Short path: Unix socket paths are limited to 108 bytes.
    let dir = tempfile::Builder::new().prefix(ROOT_PREFIX).tempdir_in("/tmp").context("creating /tmp/ashd-nested.*")?;
    let root = dir.keep();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    Ok(root)
}

fn spawn_logged(mut cmd: Command, log: &Path, die_with_parent: bool) -> Result<Child> {
    let f = File::create(log)?;
    cmd.stdin(Stdio::null()).stdout(f.try_clone()?).stderr(f).process_group(0);
    if die_with_parent {
        // SAFETY: only async-signal-safe prctl between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            });
        }
    }
    cmd.spawn().with_context(|| format!("spawning {:?}", cmd.get_program()))
}

fn wayland_sockets(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_socket()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("wayland-"))
        .collect();
    v.sort();
    v
}

fn niri_sockets(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with("niri.wayland-") && n.ends_with(".sock")
            })
        })
        .collect();
    v.sort();
    v
}

/// `niri.wayland-2.1317221.sock` → `wayland-2`.
fn niri_display_of(sock: &Path) -> Option<String> {
    let n = sock.file_name()?.to_string_lossy().into_owned();
    let rest = n.strip_prefix("niri.")?;
    let (disp, _) = rest.split_once('.')?;
    Some(disp.to_string())
}

pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("export ") else { continue };
        let Some((k, v)) = rest.split_once('=') else { continue };
        m.insert(k.trim().to_string(), v.trim().trim_matches('\'').to_string());
    }
    m
}

// ── teardown ──────────────────────────────────────────────────────────────────

/// Only ever delete things that look like our roots.
pub fn checked_root(p: &Path) -> Result<PathBuf> {
    let p = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir()?.join(p) };
    let ok = p.parent() == Some(Path::new("/tmp"))
        && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(ROOT_PREFIX));
    if !ok {
        bail!("{} is not a session root (expected /tmp/{ROOT_PREFIX}*)", p.display());
    }
    if !p.is_dir() {
        bail!("{} does not exist", p.display());
    }
    Ok(p)
}

fn remove_root(root: &Path) -> Result<()> {
    let root = checked_root(root)?;
    fs::remove_dir_all(&root).with_context(|| format!("removing {}", root.display()))
}

/// pids named by the root's pid files, with the command line each must match.
fn pidfile_pids(root: &Path) -> Vec<i32> {
    [("sway.pid", "sway"), ("niri.pid", "niri"), ("dbus.pid", "dbus")]
        .iter()
        .filter_map(|(f, needle)| {
            let pid: i32 = fs::read_to_string(root.join(f)).ok()?.trim().parse().ok()?;
            (pid_alive(pid) && util::cmdline(pid).contains(needle)).then_some(pid)
        })
        .collect()
}

/// All live processes belonging to the session: pid-file leaders and everything that
/// carries the root as `XDG_RUNTIME_DIR`.
fn session_processes(root: &Path) -> (Vec<i32>, Vec<i32>) {
    let leaders = pidfile_pids(root);
    let mut others = util::scan_session_pids().remove(root).unwrap_or_default();
    others.retain(|p| !leaders.contains(p));
    (leaders, others)
}

fn send(pid: i32, leader: bool, sig: Signal) {
    let p = Pid::from_raw(pid);
    // Only signal a whole group when the pid leads its own group; never our own.
    if leader && pgid_of(pid) == Some(pid) {
        let _ = signal::killpg(p, sig);
    } else {
        let _ = signal::kill(p, sig);
    }
}

/// SIGTERM, wait, SIGKILL everything of the session; reap owned children.
pub fn kill_session(root: &Path, children: &mut [Child]) {
    for round in 0..2 {
        let sig = if round == 0 { Signal::SIGTERM } else { Signal::SIGKILL };
        let (leaders, others) = session_processes(root);
        if leaders.is_empty() && others.is_empty() {
            break;
        }
        for p in &leaders {
            send(*p, true, sig);
        }
        for p in &others {
            send(*p, false, sig);
        }
        let gone = wait_until(Duration::from_secs(if round == 0 { 3 } else { 2 }), Duration::from_millis(50), || {
            for c in children.iter_mut() {
                let _ = c.try_wait();
            }
            let (l, o) = session_processes(root);
            (l.is_empty() && o.is_empty()).then_some(())
        });
        if gone.is_some() {
            break;
        }
    }
    for c in children.iter_mut() {
        let _ = c.try_wait();
    }
}

// ── signal handling: tear sessions down on SIGINT/SIGTERM/SIGHUP ──────────────

static SIGNALED: AtomicBool = AtomicBool::new(false);
static ROOTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

extern "C" fn on_signal(_sig: libc::c_int) {
    SIGNALED.store(true, Ordering::SeqCst);
}

fn register_root(root: &Path) {
    if let Ok(mut r) = ROOTS.lock() {
        r.push(root.to_path_buf());
    }
}

fn unregister_root(root: &Path) {
    if let Ok(mut r) = ROOTS.lock() {
        r.retain(|p| p != root);
    }
}

fn install_signal_handlers() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let action = SigAction::new(SigHandler::Handler(on_signal), SaFlags::empty(), SigSet::empty());
        for sig in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
            // SAFETY: the handler only stores to an atomic.
            let _ = unsafe { signal::sigaction(sig, &action) };
        }
        thread::spawn(|| {
            loop {
                thread::sleep(Duration::from_millis(50));
                if SIGNALED.load(Ordering::SeqCst) {
                    let roots: Vec<PathBuf> = ROOTS.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default();
                    for r in roots {
                        eprintln!("xtask: interrupted, tearing down {}", r.display());
                        kill_session(&r, &mut []);
                        let _ = remove_root(&r);
                    }
                    std::process::exit(130);
                }
            }
        });
    });
}

// ── run ───────────────────────────────────────────────────────────────────────

fn run_command(
    size: &str,
    with_bus: bool,
    theme: ThemeName,
    niri_config: Option<PathBuf>,
    keep: bool,
    cmd: &[String],
) -> Result<ExitCode> {
    let opts = StartOpts { size: parse_size(size)?, with_bus, theme, die_with_parent: true, niri_config };
    let mut session = Session::start(&opts)?;
    let status = session.command(&cmd[0]).args(&cmd[1..]).status();
    if keep {
        session.stop_keep();
        eprintln!("xtask: session root kept: {}", session.root.display());
    } else {
        session.teardown();
    }
    let status = status.with_context(|| format!("running `{}`", cmd[0]))?;
    use std::os::unix::process::ExitStatusExt;
    let code = status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(1);
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

// ── list / gc ─────────────────────────────────────────────────────────────────

pub struct RootInfo {
    pub root: PathBuf,
    pub alive: bool,
    pub pids: Vec<i32>,
    pub age: Duration,
    pub has_pidfiles: bool,
    pub mine: bool,
}

pub fn discover_roots() -> Vec<RootInfo> {
    let mut scan = util::scan_session_pids();
    let uid = nix::unistd::geteuid().as_raw();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/tmp") else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(ROOT_PREFIX) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if !md.is_dir() {
            continue;
        }
        let root = e.path();
        let mut pids = pidfile_pids(&root);
        for p in scan.remove(&root).unwrap_or_default() {
            if !pids.contains(&p) {
                pids.push(p);
            }
        }
        let has_pidfiles = ["sway.pid", "niri.pid", "dbus.pid"].iter().any(|f| root.join(f).exists());
        let age = md.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
        out.push(RootInfo { alive: !pids.is_empty(), pids, age, has_pidfiles, mine: md.uid() == uid, root });
    }
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out
}

/// A root is garbage when nothing in it runs, it is ours, and it is not mid-start.
pub fn is_stale(r: &RootInfo) -> bool {
    r.mine && !r.alive && (r.has_pidfiles || r.age > Duration::from_secs(60))
}

fn list() {
    let roots = discover_roots();
    if roots.is_empty() {
        println!("no session roots");
        return;
    }
    for r in roots {
        let state = if r.alive {
            "alive"
        } else if is_stale(&r) {
            "dead"
        } else {
            "starting?"
        };
        let pids: Vec<String> = r.pids.iter().map(|p| p.to_string()).collect();
        println!("{}  {state:<9}  age {:>5}s  pids [{}]", r.root.display(), r.age.as_secs(), pids.join(" "));
    }
}

pub fn gc() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for r in discover_roots() {
        if is_stale(&r) && remove_root(&r.root).is_ok() {
            println!("removed {}", r.root.display());
            removed.push(r.root);
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(root: &str) -> Session {
        Session {
            root: PathBuf::from(root),
            wayland_display: "wayland-2".into(),
            niri_socket: PathBuf::from(format!("{root}/niri.wayland-2.1.sock")),
            parent_display: "wayland-1".into(),
            bus: None,
            children: vec![],
            guard: false,
        }
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
        assert!(parse_size("1920").is_err());
        assert!(parse_size("10x10").is_err());
    }

    #[test]
    fn env_contract() {
        let mut s = fake("/tmp/ashd-nested.abc");
        let text = s.env_text();
        for needle in [
            "export XDG_RUNTIME_DIR='/tmp/ashd-nested.abc'",
            "export WAYLAND_DISPLAY='wayland-2'",
            "export NIRI_SOCKET='/tmp/ashd-nested.abc/niri.wayland-2.1.sock'",
            "export ASHD_STATE_DIR='/tmp/ashd-nested.abc/ashd-state'",
            "export HOME='/tmp/ashd-nested.abc/home'",
            "export XDG_CONFIG_HOME='/tmp/ashd-nested.abc/home/.config'",
            "export ASHD_NESTED='1'",
            "export ASHD_FAKE_POWER='1'",
            "export ASHD_NO_RELOAD='1'",
            "export PULSE_SERVER='unix:/tmp/ashd-nested.abc/no-pulse'",
            "export ASHD_PARENT_WAYLAND_DISPLAY='wayland-1'",
        ] {
            assert!(text.contains(needle), "{needle} missing in\n{text}");
        }
        // No bus: DBUS_SESSION_BUS_ADDRESS is unset, never exported.
        assert!(text.lines().next().unwrap().contains("DBUS_SESSION_BUS_ADDRESS"));
        assert!(!text.contains("export DBUS_SESSION_BUS_ADDRESS"));
        // The env file round-trips.
        let parsed = parse_env_file(&text);
        assert_eq!(parsed["WAYLAND_DISPLAY"], "wayland-2");
        s.bus = Some("unix:path=/tmp/ashd-nested.abc/bus".into());
        let text = s.env_text();
        assert!(text.contains("export DBUS_SESSION_BUS_ADDRESS='unix:path=/tmp/ashd-nested.abc/bus'"));
        assert!(!text.lines().next().unwrap().contains("DBUS_SESSION_BUS_ADDRESS"));
    }

    #[test]
    fn live_desktop_is_scrubbed() {
        // Even with every live variable set in our own environment, the command built
        // for the session carries none of them.
        let s = fake("/tmp/ashd-nested.abc");
        let mut cmd = s.command("true");
        cmd.env("UNRELATED", "1");
        let envs: BTreeMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        for v in ["DISPLAY", "WAYLAND_SOCKET", "SWAYSOCK", "DBUS_SESSION_BUS_ADDRESS", "ASHD_SOCKET"] {
            assert_eq!(envs.get(v), Some(&None), "{v} must be explicitly removed");
        }
        assert_eq!(envs["WAYLAND_DISPLAY"].as_deref(), Some("wayland-2"));
        assert_eq!(envs["PULSE_SERVER"].as_deref(), Some("unix:/tmp/ashd-nested.abc/no-pulse"));
        assert_eq!(envs["ASHD_FAKE_POWER"].as_deref(), Some("1"));
        assert_eq!(envs["ASHD_NO_RELOAD"].as_deref(), Some("1"));
        assert_eq!(envs["NIRI_SOCKET"].as_deref(), Some("/tmp/ashd-nested.abc/niri.wayland-2.1.sock"));
        assert_eq!(envs["XDG_RUNTIME_DIR"].as_deref(), Some("/tmp/ashd-nested.abc"));
    }

    #[test]
    fn scrubbed_child_really_sees_nothing() {
        // Whatever the test process inherits (a live desktop in a dev shell), the child
        // of a session sees only the session's values.
        let s = fake("/tmp/ashd-nested.abc");
        let mut c = s.command("sh");
        c.args(["-c", "echo \"[$DISPLAY][$DBUS_SESSION_BUS_ADDRESS][$ASHD_SOCKET][$WAYLAND_DISPLAY]\""]);
        let r = util::run_captured(c, Duration::from_secs(5)).unwrap();
        assert_eq!(r.stdout.trim(), "[][][][wayland-2]");
    }

    #[test]
    fn niri_display_parsing() {
        assert_eq!(niri_display_of(Path::new("/tmp/x/niri.wayland-2.1317221.sock")).as_deref(), Some("wayland-2"));
        assert_eq!(niri_display_of(Path::new("/tmp/x/other")), None);
    }

    #[test]
    fn niri_config_per_theme() {
        assert_eq!(niri_config(ThemeName::Ash), NIRI_KDL);
        let ink = niri_config(ThemeName::Ink);
        assert!(ink.contains("#0E0F11"));
        assert!(ink.contains("#FF5B3A"));
        assert!(!ink.contains("#111110"));
    }

    #[test]
    fn custom_niri_config_is_themed_too() {
        let base = "layout { background-color \"#111110\" }\nworkspace \"scratch\" { hidden }\n";
        let ink = themed_niri_config(base, ThemeName::Ink);
        assert!(ink.contains("#0E0F11") && !ink.contains("#111110"));
        assert!(ink.contains("workspace \"scratch\" { hidden }"));
        assert_eq!(themed_niri_config(NIRI_KDL, ThemeName::Moss), niri_config(ThemeName::Moss));
    }

    #[test]
    fn root_safety() {
        assert!(checked_root(Path::new("/etc")).is_err());
        assert!(checked_root(Path::new("/tmp")).is_err());
        assert!(checked_root(Path::new("/tmp/ashd-nested.does-not-exist")).is_err());
        assert!(checked_root(Path::new("/tmp/other.abc")).is_err());
    }

    #[test]
    fn gc_removes_only_dead_roots() {
        let dead = make_root().unwrap();
        fs::write(dead.join("sway.pid"), "2147483000\n").unwrap();
        let live = make_root().unwrap();
        // our own pid, but the command line check (`sway`) fails: counts as dead
        // unless a process carries the root in its environment.
        let mut child = Command::new("sleep").arg("30").env("XDG_RUNTIME_DIR", &live).spawn().unwrap();
        let fresh = make_root().unwrap(); // no pid files, brand new: "starting"
        let removed = gc();
        assert!(removed.contains(&dead), "dead root must be removed");
        assert!(!dead.exists());
        assert!(live.exists(), "live root (process with XDG_RUNTIME_DIR) must survive");
        assert!(fresh.exists(), "a root that is still starting must survive");
        let _ = child.kill();
        let _ = child.wait();
        fs::remove_dir_all(&live).unwrap();
        fs::remove_dir_all(&fresh).unwrap();
    }

    #[test]
    fn kill_session_kills_env_marked_processes() {
        let root = make_root().unwrap();
        let mut child = Command::new("sleep").arg("30").env("XDG_RUNTIME_DIR", &root).spawn().unwrap();
        assert!(pid_alive(child.id() as i32));
        let mut owned = [];
        kill_session(&root, &mut owned);
        let st = wait_until(Duration::from_secs(3), Duration::from_millis(20), || child.try_wait().ok().flatten());
        assert!(st.is_some(), "process of the session must be gone");
        remove_root(&root).unwrap();
    }
}
