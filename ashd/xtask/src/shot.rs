//! Screenshots of the nested session's output.
//!
//! Two capture paths, both confined to the session:
//! - `grim`: wlr-screencopy on sway, the headless parent (the tool the bash recipe used);
//! - `niri`: niri's own `screenshot-screen` IPC action writing to a path.

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use image::RgbaImage;

use crate::{
    nested::{Session, scrub},
    util::{self, resolve_tool, run_captured, wait_until},
};

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum Via {
    Grim,
    Niri,
}

#[derive(Args)]
pub struct ShotArgs {
    /// PNG to write (default: `target-notes/shot.png`).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Session root (default: `$XDG_RUNTIME_DIR` when it is a session, else the only live session).
    #[arg(long)]
    root: Option<PathBuf>,
    /// Capture path.
    #[arg(long, value_enum, default_value = "grim")]
    via: Via,
}

pub fn dispatch(args: ShotArgs) -> Result<ExitCode> {
    let root = match args.root {
        Some(r) => r,
        None => detect_root()?,
    };
    let session = Session::attach(&root)?;
    let out = args.out.unwrap_or_else(|| util::notes_dir().join("shot.png"));
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir)?;
    }
    let img = capture_settled(&session, &out, args.via, Duration::from_secs(3))?;
    println!("{} ({}x{})", out.display(), img.width(), img.height());
    Ok(ExitCode::SUCCESS)
}

/// The session to use when `--root` is not given. Never the live desktop.
fn detect_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(dir);
        if crate::nested::checked_root(&dir).is_ok() {
            return Ok(dir);
        }
    }
    let alive: Vec<_> = crate::nested::discover_roots().into_iter().filter(|r| r.alive && r.mine).collect();
    match alive.as_slice() {
        [one] => Ok(one.root.clone()),
        [] => bail!("no live nested session; start one with `cargo xtask nested start`"),
        many => bail!(
            "{} live sessions ({}); pass --root",
            many.len(),
            many.iter().map(|r| r.root.display().to_string()).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Take one screenshot of the session's output into `out`.
pub fn capture(session: &Session, out: &Path, via: Via) -> Result<()> {
    let out = if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir()?.join(out) };
    match via {
        Via::Grim => {
            let grim = resolve_tool("GRIM", "grim", "grim")?;
            let mut cmd = std::process::Command::new(grim);
            scrub(&mut cmd);
            // grim talks to sway (the parent), inside the session's private runtime dir.
            cmd.env("XDG_RUNTIME_DIR", &session.root).env("WAYLAND_DISPLAY", &session.parent_display).arg(&out);
            let r = run_captured(cmd, Duration::from_secs(20))?;
            if r.code != Some(0) {
                bail!("grim failed ({:?}): {}", r.code, r.stderr.trim());
            }
        }
        Via::Niri => {
            let niri = resolve_tool("NIRI", "niri", "niri")?;
            let _ = fs::remove_file(&out);
            let mut cmd = session.command(niri);
            cmd.args(["msg", "action", "screenshot-screen", "--write-to-disk", "true", "--show-pointer", "false", "--path"])
                .arg(&out);
            let r = run_captured(cmd, Duration::from_secs(20))?;
            if r.code != Some(0) {
                bail!("niri screenshot-screen failed ({:?}): {}", r.code, r.stderr.trim());
            }
            // The action is queued: wait until a complete PNG is on disk.
            let ok = wait_until(Duration::from_secs(10), Duration::from_millis(50), || image::open(&out).ok());
            if ok.is_none() {
                bail!("niri did not write {}", out.display());
            }
        }
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<RgbaImage> {
    Ok(image::open(path).with_context(|| format!("decoding {}", path.display()))?.to_rgba8())
}

/// Screenshot until two consecutive captures are identical (the shell finished drawing),
/// or `timeout` passes (then the last one is returned).
pub fn capture_settled(session: &Session, out: &Path, via: Via, timeout: Duration) -> Result<RgbaImage> {
    let end = std::time::Instant::now() + timeout;
    let mut prev: Option<RgbaImage> = None;
    loop {
        capture(session, out, via)?;
        let img = load(out)?;
        if prev.as_ref().is_some_and(|p| p.as_raw() == img.as_raw()) || std::time::Instant::now() >= end {
            return Ok(img);
        }
        prev = Some(img);
        util::sleep_ms(120);
    }
}
