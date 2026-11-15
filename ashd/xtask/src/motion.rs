//! `cargo xtask motion`: does every surface really move? The shell runs in a nested session
//! with `ASHD_MOTION_SCALE` (every tween and spring takes N× as long) and
//! `ASHD_FRAME_LOG=1` (frames drawn per second, per surface kind). For each scenario:
//!
//! 1. a settled screenshot (the *before* state);
//! 2. the open command, then a burst of screenshots as fast as `grim` goes: at least two of
//!    them must differ from both the before state and the settled *after* state (the frames
//!    in between exist), and the settled picture must be the same as a second settled one
//!    (the animation really ends);
//! 3. the close command and another burst; the settled picture must equal the *before* state
//!    (nothing stuck, nothing half-transparent);
//! 4. open → close → open as fast as the CLI can run (a reversal mid-flight): the settled
//!    picture must equal the plain *after* state, then close again and equal the *before*;
//! 5. idle: three seconds with nothing happening must log zero frames for every surface.
//!
//! Screenshots land in `target-notes/motion-<scenario>-<phase>-<n>.png`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

mod more;

use anyhow::{Context, Result, bail};
use clap::Args;
use image::RgbaImage;

use crate::{
    harness::{self, Harness},
    nested::{StartOpts, parse_size},
    shot::{self, Via},
    util,
};

#[derive(Args)]
pub struct MotionArgs {
    #[arg(long, default_value = "1920x1080")]
    pub size: String,
    /// Do not `cargo build -p ashd-shell -p ashd-cli` first.
    #[arg(long)]
    pub no_build: bool,
    /// Slow-motion factor handed to the shell (`ASHD_MOTION_SCALE`).
    #[arg(long, default_value_t = 10.0)]
    pub scale: f32,
    /// Frames per burst.
    #[arg(long, default_value_t = 14)]
    pub frames: usize,
    /// Only these scenarios (default: all).
    #[arg(long)]
    pub only: Vec<String>,
    /// Extra `KEY=VALUE` for the shell's environment (e.g. `--env ASHD_NET_MOCK=connected`).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Run the shell with reduced motion (`ASHD_REDUCE_MOTION=1`): the scratchpad must then appear and leave at once, popups keep the 80 ms fade.
    #[arg(long)]
    pub reduce_motion: bool,
    /// Like `--reduce-motion`, but through `"reduceMotion": true` in the session's `shell.json`
    /// (the environment variable is not set).
    #[arg(long)]
    pub reduce_motion_config: bool,
    #[arg(long, value_enum, default_value = "grim")]
    pub via: Via,
    #[arg(long)]
    pub niri_config: Option<PathBuf>,
}

pub fn dispatch(args: MotionArgs) -> Result<ExitCode> {
    Ok(if run(&args)? { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

struct Scenario {
    name: &'static str,
    open: &'static [&'static str],
    close: &'static [&'static str],
    /// A popup anchored to a bar icon: its position follows the live status block.
    anchored: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "launcher", open: &["launcher", "--open"], close: &["launcher", "--close"], anchored: false },
    Scenario { name: "quick-settings", open: &["quick-settings", "--open"], close: &["quick-settings", "--close"], anchored: true },
    Scenario { name: "calendar", open: &["calendar", "--open"], close: &["calendar", "--close"], anchored: true },
    Scenario { name: "session", open: &["session", "--open"], close: &["session", "--close"], anchored: true },
    Scenario { name: "notifications", open: &["notifications", "--open"], close: &["notifications", "--close"], anchored: true },
    Scenario { name: "clipboard", open: &["clipboard", "--open"], close: &["clipboard", "--close"], anchored: false },
    Scenario { name: "cheatsheet", open: &["cheatsheet", "--open"], close: &["cheatsheet", "--close"], anchored: false },
    Scenario { name: "theme-pick", open: &["theme", "pick", "--open"], close: &["theme", "pick", "--close"], anchored: false },
    Scenario { name: "scratchpad", open: &["scratch", "open"], close: &["scratch", "close"], anchored: false },
    // Scroll <-> Canvas: the bar's mode switch and the canvas chip swap (`toggle` is its own inverse)
    Scenario { name: "canvas", open: &["canvas", "toggle"], close: &["canvas", "toggle"], anchored: false },
];

/// The status block of the bar (clock, cpu, mem, network) shows live data and is not part
/// of what the transitions are judged on.
fn live(x: u32, y: u32, h: u32) -> bool {
    y + 52 >= h && x < 570
}

/// Pixels (any channel off by more than 2) that differ, outside the live status block,
/// and the box around them.
fn diff_px(a: &RgbaImage, b: &RgbaImage) -> (usize, Option<(u32, u32, u32, u32)>) {
    diff_px_tol(a, b, 2)
}

/// [`diff_px`] with a channel tolerance (0 = any difference at all).
fn diff_px_tol(a: &RgbaImage, b: &RgbaImage, tol: u8) -> (usize, Option<(u32, u32, u32, u32)>) {
    if a.dimensions() != b.dimensions() {
        return (usize::MAX, None);
    }
    let (w, h) = a.dimensions();
    let (mut n, mut bb): (usize, Option<(u32, u32, u32, u32)>) = (0, None);
    for (i, (p, q)) in a.as_raw().chunks_exact(4).zip(b.as_raw().chunks_exact(4)).enumerate() {
        if p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > tol) {
            let (px, py) = (i as u32 % w, i as u32 / w);
            if live(px, py, h) {
                continue;
            }
            n += 1;
            bb = Some(match bb {
                None => (px, py, px, py),
                Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
            });
        }
    }
    (n, bb)
}

/// Fraction of pixels that differ ([`diff_px`]).
fn diff(a: &RgbaImage, b: &RgbaImage) -> f64 {
    match diff_px(a, b) {
        (usize::MAX, _) => 1.0,
        (n, _) => n as f64 / (a.width() as f64 * a.height() as f64),
    }
}

/// The smallest difference of `a` against `b` moved sideways by up to ±12 px, over the main
/// area (right of the rail, above the bar), as a fraction of the whole picture. Popups anchored
/// to bar icons move with the live width of the status block (the cpu percentage gains or
/// loses a digit); a stuck or half-transparent panel differs everywhere and still fails.
fn diff_anchored(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.dimensions() != b.dimensions() {
        return 1.0;
    }
    let (w, h) = a.dimensions();
    let (x0, y1) = (70u32, h.saturating_sub(52));
    let best = (-12i32..=12)
        .map(|dx| {
            let mut n = 0usize;
            for y in 0..y1 {
                for x in x0..w {
                    let bx = (x as i32 - dx).clamp(0, w as i32 - 1) as u32;
                    let (p, q) = (a.get_pixel(x, y), b.get_pixel(bx, y));
                    if p.0.iter().zip(q.0.iter()).any(|(m, k)| m.abs_diff(*k) > 2) {
                        n += 1;
                    }
                }
            }
            n
        })
        .min()
        .unwrap_or(0);
    best as f64 / (w as f64 * h as f64)
}

/// "0.012 % (box x0,y0–x1,y1)" for a report line.
fn describe(a: &RgbaImage, b: &RgbaImage) -> String {
    let (n, bb) = diff_px(a, b);
    let pct = diff(a, b) * 100.0;
    match bb {
        Some((x0, y0, x1, y1)) if n > 0 => format!("{pct:.3} % ({n} px in {x0},{y0}-{x1},{y1})"),
        _ => format!("{pct:.3} %"),
    }
}

/// The clock and the age labels tick; a settled picture is "the same" below this.
const SAME: f64 = 0.0006;
/// A frame counts as "in between" when it differs from both end states by more than this.
const BETWEEN: f64 = 0.0004;

struct Run<'a> {
    h: &'a mut Harness,
    notes: PathBuf,
    frames: usize,
}

impl Run<'_> {
    fn path(&self, scenario: &str, phase: &str, n: usize) -> PathBuf {
        self.notes.join(format!("motion-{scenario}-{phase}-{n:02}.png"))
    }

    /// Wait until the shell itself has stopped drawing (two consecutive one-second samples of
    /// `ASHD_FRAME_LOG` where nothing but the bar drew a frame), then screenshot. A spring's
    /// tail moves less than a pixel per capture for seconds at slow motion, so "two identical
    /// pictures" alone would call it settled too early.
    fn settled(&self, out: &Path, timeout: Duration) -> Result<RgbaImage> {
        let start = fs::read_to_string(&self.h.shell_log).unwrap_or_default().lines().count();
        let end = std::time::Instant::now() + timeout.max(Duration::from_secs(20));
        loop {
            util::sleep_ms(400);
            let log = fs::read_to_string(&self.h.shell_log).unwrap_or_default();
            let quiet: Vec<bool> = log
                .lines()
                .skip(start)
                .filter(|l| l.contains("frames/s"))
                .map(|l| {
                    l.split_whitespace()
                        .skip_while(|w| *w != "frames/s")
                        .skip(1)
                        .filter(|kv| !kv.starts_with("bar="))
                        .all(|kv| kv.ends_with("=0"))
                })
                .collect();
            if quiet.len() >= 3 && quiet[quiet.len() - 2..].iter().all(|q| *q) || std::time::Instant::now() >= end {
                break;
            }
        }
        shot::capture_settled(&self.h.session, out, self.h.via, Duration::from_secs(4))
    }

    /// Run `args`, then screenshot `frames` times back to back. Returns the frames.
    fn burst(&self, args: &[&str], scenario: &str, phase: &str) -> Result<Vec<RgbaImage>> {
        let o = self.h.ashd(args)?;
        if o.code != Some(0) {
            bail!("ashd {} failed: {:?} {}", args.join(" "), o.code, o.stderr.trim());
        }
        let mut out = Vec::new();
        for n in 0..self.frames {
            let p = self.path(scenario, phase, n);
            shot::capture(&self.h.session, &p, self.h.via)?;
            out.push(shot::load(&p)?);
        }
        Ok(out)
    }
}

#[derive(Default)]
struct Table {
    rows: Vec<(String, bool, String)>,
}

impl Table {
    fn add(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        let (name, detail) = (name.into(), detail.into());
        println!("  {} {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        self.rows.push((name, ok, detail));
    }

    fn failures(&self) -> usize {
        self.rows.iter().filter(|r| !r.1).count()
    }
}

/// Pixels that differ by more than `tol` in some channel (no mask: for crops).
fn count_diff(a: &RgbaImage, b: &RgbaImage, tol: u8) -> usize {
    a.as_raw()
        .chunks_exact(4)
        .zip(b.as_raw().chunks_exact(4))
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > tol))
        .count()
}

/// [`between_fine`] for crops.
fn between_crop(frames: &[RgbaImage], a: &RgbaImage, b: &RgbaImage) -> usize {
    frames.iter().filter(|f| count_diff(f, a, 0) >= 150 && count_diff(f, b, 0) >= 150).count()
}

/// Like [`between`] for low-contrast changes (a hover fill is a handful of levels): a frame
/// counts when 150 or more pixels differ from each end state at all.
fn between_fine(frames: &[RgbaImage], a: &RgbaImage, b: &RgbaImage) -> usize {
    frames.iter().filter(|f| diff_px_tol(f, a, 0).0 >= 150 && diff_px_tol(f, b, 0).0 >= 150).count()
}

/// How many frames of the burst lie strictly between the two end states.
fn between(frames: &[RgbaImage], a: &RgbaImage, b: &RgbaImage) -> usize {
    frames.iter().filter(|f| diff(f, a) > BETWEEN && diff(f, b) > BETWEEN).count()
}

/// Absolute pointer position through `wlrctl` (relative moves: park in the corner first).
fn pointer_to(run: &Run<'_>, x: i32, y: i32) -> Result<()> {
    let wlrctl = util::resolve_tool("WLRCTL", "wlrctl", "wlrctl")?;
    for args in [["pointer".into(), "move".into(), "-5000".into(), "-5000".into()], ["pointer".into(), "move".into(), x.to_string(), y.to_string()]] {
        let mut cmd = run.h.session.command(&wlrctl);
        cmd.args(args);
        let c = util::run_captured(cmd, Duration::from_secs(10))?;
        if c.code != Some(0) {
            bail!("wlrctl pointer move: exit {:?}: {}", c.code, c.stderr.trim());
        }
    }
    Ok(())
}

/// A switch flips: the do-not-disturb toggle of the notification centre (knob, track and
/// border glide for 120 ms).
fn toggle(run: &mut Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== toggle ==");
    let wlrctl = util::resolve_tool("WLRCTL", "wlrctl", "wlrctl")?;
    let click = |run: &Run<'_>| -> Result<()> {
        pointer_to(run, 497, 826)?;
        let mut cmd = run.h.session.command(&wlrctl);
        cmd.args(["pointer", "click", "left"]);
        let c = util::run_captured(cmd, Duration::from_secs(10))?;
        if c.code != Some(0) {
            bail!("wlrctl pointer click: {:?} {}", c.code, c.stderr.trim());
        }
        Ok(())
    };
    pointer_to(run, 1000, 300)?;
    let _ = run.h.ashd(&["notifications", "--open"])?;
    let crop = |f: &RgbaImage| image::imageops::crop_imm(f, 465, 808, 64, 36).to_image();
    let before = crop(&run.settled(&run.path("toggle", "off", 0), settle)?);
    click(run)?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("toggle", "to-on", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(crop(&shot::load(&p)?));
    }
    pointer_to(run, 1000, 300)?; // the cursor image is part of the screenshot
    let on = crop(&run.settled(&run.path("toggle", "on", 0), settle)?);
    let mid = between_crop(&frames, &before, &on);
    table.add(
        "toggle: do-not-disturb glides on",
        count_diff(&before, &on, 2) > 150 && mid >= 2,
        format!("{} px differ between off and on; {mid} in-between frames", count_diff(&before, &on, 2)),
    );
    click(run)?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("toggle", "to-off", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(crop(&shot::load(&p)?));
    }
    pointer_to(run, 1000, 300)?;
    let off = crop(&run.settled(&run.path("toggle", "off-again", 0), settle)?);
    let mid = between_crop(&frames, &on, &off);
    table.add(
        "toggle: and glides back off",
        count_diff(&before, &off, 2) < 20 && mid >= 2,
        format!("{} px differ from the start; {mid} in-between frames", count_diff(&before, &off, 2)),
    );
    let _ = run.h.ashd(&["dismiss"])?;
    Ok(())
}

/// A modal that is dismissed while another opens: the old one leaves while the new one arrives
/// (two layer surfaces at once), and the shell ends up showing only the new one.
fn switch(run: &mut Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== switch ==");
    let _ = run.h.ashd(&["calendar", "--open"])?;
    let only = run.settled(&run.path("switch", "calendar", 0), settle)?;
    let _ = run.h.ashd(&["calendar", "--close"])?;
    let empty = run.settled(&run.path("switch", "empty", 0), settle)?;
    let _ = run.h.ashd(&["launcher", "--open"])?;
    util::sleep_ms(300);
    let _ = run.h.ashd(&["calendar", "--open"])?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("switch", "mid", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    let after = run.settled(&run.path("switch", "after", 0), settle)?;
    let st = run.h.status()?;
    table.add(
        "switch: launcher -> calendar leaves only the calendar",
        st.modal.as_deref() == Some("calendar") && diff_anchored(&only, &after) <= SAME,
        format!("modal {:?}; differs from the calendar alone by {:.3} %", st.modal, diff_anchored(&only, &after) * 100.0),
    );
    table.add(
        "switch: both surfaces are on screen meanwhile",
        between(&frames, &empty, &after) >= 2,
        format!("{} in-between frames", between(&frames, &empty, &after)),
    );
    let _ = run.h.ashd(&["calendar", "--close"])?;
    let gone = run.settled(&run.path("switch", "gone", 0), settle)?;
    table.add("switch: closing restores the start", diff(&empty, &gone) <= SAME, format!("differs by {}", describe(&empty, &gone)));
    Ok(())
}

/// A new app's dock button grows in and a closed one shrinks away (`kitty --class …` as the
/// client; skipped when kitty cannot start in the session).
fn dock(run: &mut Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== dock ==");
    let Some(kitty) = util::find_in_path("kitty") else {
        println!("  (skipped: no kitty on PATH)");
        return Ok(());
    };
    let before = run.settled(&run.path("dock", "before", 0), settle)?;
    let mut cmd = run.h.session.command(kitty);
    cmd.args(["--class", "motiondock", "-o", "confirm_os_window_close=0"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().context("starting kitty")?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("dock", "in", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    let with = run.settled(&run.path("dock", "with", 0), settle)?;
    // The window itself changes the picture everywhere; judge the dock row only.
    let row = |f: &RgbaImage| image::imageops::crop_imm(f, 600, 1036, 500, 44).to_image();
    let (rb, rw): (RgbaImage, RgbaImage) = (row(&before), row(&with));
    let rf: Vec<RgbaImage> = frames.iter().map(row).collect();
    let grown = count_diff(&rb, &rw, 2);
    let mid = between_crop(&rf, &rb, &rw);
    table.add("dock: a new app's button grows in", grown > 200 && mid >= 2, format!("{grown} px differ in the dock row; {mid} in-between frames"));
    let _ = child.kill();
    let _ = child.wait();
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("dock", "out", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    let gone = run.settled(&run.path("dock", "gone", 0), settle)?;
    let rg = row(&gone);
    let rf: Vec<RgbaImage> = frames.iter().map(row).collect();
    let mid = between_crop(&rf, &rw, &rg);
    table.add(
        "dock: a closed app's button shrinks away and the dock returns to the start",
        mid >= 2 && count_diff(&rb, &rg, 2) < 50,
        format!("{mid} in-between frames; the row differs from the start by {} px", count_diff(&rb, &rg, 2)),
    );
    Ok(())
}

/// Two `kitty` windows, the second moved to workspace 2: the rail then shows two desks (an
/// empty workspace is removed once it loses the focus, and an empty trailing one is not a
/// button). `None` when kitty cannot start.
fn second_desk(run: &Run<'_>) -> Result<Option<Vec<std::process::Child>>> {
    let Some(kitty) = util::find_in_path("kitty") else { return Ok(None) };
    let niri = util::resolve_tool("NIRI", "niri", "niri")?;
    let mut children = Vec::new();
    for _ in 0..2 {
        let mut cmd = run.h.session.command(&kitty);
        cmd.args(["--class", "motiondesk", "-o", "confirm_os_window_close=0"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        children.push(cmd.spawn().context("starting kitty")?);
        util::sleep_ms(2500);
    }
    let mut mv = run.h.session.command(&niri);
    mv.args(["msg", "action", "move-window-to-workspace", "2"]);
    let c = util::run_captured(mv, Duration::from_secs(10))?;
    if c.code != Some(0) {
        bail!("niri msg move-window-to-workspace 2: {:?} {}", c.code, c.stderr.trim());
    }
    let mut back = run.h.session.command(&niri);
    back.args(["msg", "action", "focus-workspace", "1"]);
    let _ = util::run_captured(back, Duration::from_secs(10))?;
    util::sleep_ms(800);
    Ok(Some(children))
}

/// y of the top of the rail's active marker (accent pixels in the left column), if any.
fn marker_y(img: &RgbaImage) -> Option<u32> {
    (56..900u32).find(|y| {
        let p = img.get_pixel(1, *y).0;
        p[0].abs_diff(0xE0) < 8 && p[1].abs_diff(0xA4) < 8 && p[2].abs_diff(0x58) < 8
    })
}

/// The rail's active marker slides from workspace 1 to workspace 2 and back (spring 1.0 / 1000).
fn rail_marker(run: &mut Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== rail marker ==");
    let niri = util::resolve_tool("NIRI", "niri", "niri")?;
    let focus = |run: &Run<'_>, n: &str| -> Result<()> {
        let mut cmd = run.h.session.command(&niri);
        cmd.args(["msg", "action", "focus-workspace", n]);
        let c = util::run_captured(cmd, Duration::from_secs(10))?;
        if c.code != Some(0) {
            bail!("niri msg focus-workspace {n}: {:?} {}", c.code, c.stderr.trim());
        }
        Ok(())
    };
    focus(run, "1")?;
    let first = run.settled(&run.path("rail-marker", "first", 0), settle)?;
    focus(run, "2")?;
    let mut ys = Vec::new();
    for n in 0..run.frames {
        let p = run.path("rail-marker", "down", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        ys.push(marker_y(&shot::load(&p)?));
    }
    let second = run.settled(&run.path("rail-marker", "second", 0), settle)?;
    let (a, b) = (marker_y(&first), marker_y(&second));
    let mid = match (a, b) {
        (Some(a), Some(b)) => ys.iter().flatten().filter(|y| **y > a.min(b) + 3 && **y + 3 < a.max(b)).count(),
        _ => 0,
    };
    table.add(
        "rail marker slides to the next workspace",
        a.is_some() && b.is_some() && a != b && mid >= 2,
        format!("top y {a:?} -> {b:?}; {mid} frames with the marker in between ({ys:?})"),
    );
    focus(run, "1")?;
    let back = run.settled(&run.path("rail-marker", "back", 0), settle)?;
    table.add("rail marker returns exactly", marker_y(&back) == a, format!("top y {:?}", marker_y(&back)));
    Ok(())
}

/// Hover: the fill of a control eases in when the pointer arrives and out when it leaves.
fn hovers(run: &mut Run<'_>, table: &mut Table, settle: Duration, only: &[String]) -> Result<()> {
    // (name, setup command, control centre, a point outside everything)
    let targets: [(&str, &[&str], (i32, i32)); 5] = [
        ("dock launcher button", &[], (680, 1057)),
        ("bar Canvas segment", &[], (1805, 1057)),
        ("bar power button", &[], (1893, 1057)),
        ("rail workspace 02", &[], (31, 149)),
        ("quick-settings tile", &["quick-settings", "--open"], (415, 720)),
    ];
    for (name, setup, (x, y)) in targets {
        if !only.is_empty() && !only.iter().any(|o| o == "hover") {
            continue;
        }
        println!("== hover: {name} ==");
        pointer_to(run, 1000, 300)?;
        if !setup.is_empty() {
            let _ = run.h.ashd(setup)?;
        }
        let key = name.replace(' ', "-");
        let before = run.settled(&run.path(&format!("hover-{key}"), "before", 0), settle)?;
        pointer_to(run, x, y)?;
        let mut frames = Vec::new();
        for n in 0..run.frames {
            let p = run.path(&format!("hover-{key}"), "in", n);
            shot::capture(&run.h.session, &p, run.h.via)?;
            frames.push(shot::load(&p)?);
        }
        let on = run.settled(&run.path(&format!("hover-{key}"), "on", 0), settle)?;
        let ease = between_fine(&frames, &before, &on);
        table.add(
            format!("hover {name}: the fill eases in"),
            diff(&before, &on) > 0.00002 && ease >= 1,
            format!("{ease} in-between frames; hovered differs from rest by {}", describe(&before, &on)),
        );
        pointer_to(run, 1000, 300)?;
        let mut frames = Vec::new();
        for n in 0..run.frames {
            let p = run.path(&format!("hover-{key}"), "out", n);
            shot::capture(&run.h.session, &p, run.h.via)?;
            frames.push(shot::load(&p)?);
        }
        let off = run.settled(&run.path(&format!("hover-{key}"), "off", 0), settle)?;
        let ease = between_fine(&frames, &on, &off);
        table.add(
            format!("hover {name}: the fill eases out and rest returns"),
            ease >= 1 && diff(&before, &off) <= SAME,
            format!("{ease} in-between frames; rest differs by {}", describe(&before, &off)),
        );
        if !setup.is_empty() {
            let _ = run.h.ashd(&["dismiss"])?;
        }
    }
    Ok(())
}

/// `notify-send` / `gdbus` inside the session; stdout.
fn in_session(run: &Run<'_>, program: &str, args: &[&str]) -> Result<String> {
    let path = util::find_in_path(program).with_context(|| format!("{program} is not on PATH"))?;
    let mut cmd = run.h.session.command(path);
    cmd.args(args);
    let c = util::run_captured(cmd, Duration::from_secs(10))?;
    if c.code != Some(0) {
        bail!("{program} {}: exit {:?}: {}", args.join(" "), c.code, c.stderr.trim());
    }
    Ok(c.stdout)
}

fn notify(run: &Run<'_>, summary: &str) -> Result<String> {
    Ok(in_session(run, "notify-send", &["-p", "-t", "0", "-a", "motion", summary, "a notification that stays"])?.trim().to_string())
}

fn close_notification(run: &Run<'_>, id: &str) -> Result<()> {
    in_session(
        run,
        "gdbus",
        &["call", "--session", "--dest", "org.freedesktop.Notifications", "--object-path", "/org/freedesktop/Notifications", "--method", "org.freedesktop.Notifications.CloseNotification", id],
    )?;
    Ok(())
}

/// Toasts: the first enters, the second enters and moves the first, closing the first
/// makes the second glide down; the last one leaves.
fn toasts(run: &mut Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== toasts ==");
    let before = run.settled(&run.path("toasts", "before", 0), settle)?;
    let first = notify(run, "First toast")?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("toasts", "in1", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    let one = run.settled(&run.path("toasts", "one", 0), settle)?;
    table.add("toasts: one enters", between(&frames, &before, &one) >= 2, format!("{} in-between frames", between(&frames, &before, &one)));

    let second = notify(run, "Second toast")?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("toasts", "in2", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    let two = run.settled(&run.path("toasts", "two", 0), settle)?;
    table.add("toasts: a second enters", between(&frames, &one, &two) >= 2, format!("{} in-between frames", between(&frames, &one, &two)));

    close_notification(run, &first)?;
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path("toasts", "out1", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    let left = run.settled(&run.path("toasts", "left", 0), settle)?;
    table.add("toasts: closing one animates (fade, then the rest glides)", between(&frames, &two, &left) >= 2, format!("{} in-between frames", between(&frames, &two, &left)));

    close_notification(run, &second)?;
    let _ = run.burst(&["status"], "toasts", "out2")?;
    let gone = run.settled(&run.path("toasts", "gone", 0), settle)?;
    table.add("toasts: all gone restores the start", diff(&before, &gone) <= SAME, format!("differs by {}", describe(&before, &gone)));
    Ok(())
}

fn run(args: &MotionArgs) -> Result<bool> {
    let (w, h) = parse_size(&args.size)?;
    if !args.no_build {
        harness::build_bins()?;
        harness::build_examples()?;
    }
    let opts = StartOpts { size: (w, h), with_bus: true, die_with_parent: true, niri_config: args.niri_config.clone(), ..Default::default() };
    let mut hs = Harness::new(&opts, args.via)?;
    let notes = util::notes_dir();
    fs::create_dir_all(&notes)?;
    let scale = args.scale.to_string();
    let mut env = vec![("ASHD_MOTION_SCALE", scale.as_str()), ("ASHD_FRAME_LOG", "1")];
    if args.reduce_motion {
        env.push(("ASHD_REDUCE_MOTION", "1"));
    }
    for kv in &args.env {
        match kv.split_once('=') {
            Some((k, v)) => env.push((k, v)),
            None => bail!("--env expects KEY=VALUE, got `{kv}`"),
        }
    }
    // Frozen bar values (`worst`: every readout at its widest) and the network mock: the popups
    // anchored to bar icons, and the click points aimed at them, stand still; `--env` overrides.
    for (k, v) in [("ASHD_BAR_FIXTURE", "worst"), ("ASHD_NET_MOCK", "vpn")] {
        if !env.iter().any(|(ek, _)| *ek == k) {
            env.push((k, v));
        }
    }
    if args.reduce_motion_config {
        let dir = hs.session.root.join("home/.config/ashd");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("shell.json"), r#"{ "reduceMotion": true }"#)?;
    }
    hs.start_shell_env(&[], &env)?;
    hs.wait_ping(Duration::from_secs(90)).context("waiting for ashd ping")?;
    println!("motion: ashd-shell up in {} (scale {}, {} frames per burst)", hs.session.root.display(), args.scale, args.frames);
    util::sleep_ms(1500);

    let mut table = Table::default();
    let mut run = Run { h: &mut hs, notes: notes.clone(), frames: args.frames };
    let reduced = args.reduce_motion || args.reduce_motion_config;
    let settle = Duration::from_millis((2500.0 + 160.0 * f64::from(args.scale)) as u64);
    for sc in SCENARIOS.iter().filter(|s| args.only.is_empty() || args.only.iter().any(|o| o == s.name)) {
        println!("== {} ==", sc.name);
        let before = run.settled(&run.path(sc.name, "before", 0), settle)?;

        let open = run.burst(sc.open, sc.name, "open")?;
        let after = run.settled(&run.path(sc.name, "after", 0), settle)?;
        let after2 = run.settled(&run.path(sc.name, "after", 1), settle)?;
        let mid = between(&open, &before, &after);
        let changed = diff(&before, &after);
        table.add(format!("{} opens", sc.name), changed > BETWEEN, format!("before/after differ by {:.2} %", changed * 100.0));
        if reduced {
            // The scratchpad only moves (no fade): it must appear at once. Popups keep their 80 ms
            // fade (× the slow-motion scale here), which `mid` counts.
            // (its rows keep their 80 ms fade; the panel itself must not travel: the surface
            // fill at a fixed point is already the final colour in every frame but the first)
            let probe = |f: &RgbaImage| f.get_pixel(1600, 900).0;
            let off_place = open.iter().filter(|f| probe(f) != probe(&after)).count();
            let ok = sc.name != "scratchpad" || off_place <= 1;
            table.add(
                format!("{} open (reduced motion)", sc.name),
                ok,
                format!("{mid} in-between frames (only the 80 ms fade may remain); {off_place} frames with the panel off its place"),
            );
        } else {
            table.add(format!("{} open animates", sc.name), mid >= 2, format!("{mid} of {} frames between the end states", open.len()));
        }
        table.add(
            format!("{} open ends", sc.name),
            diff(&after, &after2) <= SAME,
            format!("two settled pictures differ by {}", describe(&after, &after2)),
        );

        let close = run.burst(sc.close, sc.name, "close")?;
        let back = run.settled(&run.path(sc.name, "back", 0), settle)?;
        let mid = between(&close, &after, &back);
        if reduced {
            let ok = sc.name != "scratchpad" || mid <= 1;
            table.add(format!("{} close (reduced motion)", sc.name), ok, format!("{mid} in-between frames (a scratchpad without motion leaves at once)"));
        } else {
            table.add(format!("{} close animates", sc.name), mid >= 2, format!("{mid} of {} frames between the end states", close.len()));
        }
        table.add(
            format!("{} close restores the before state", sc.name),
            diff(&before, &back) <= SAME,
            format!("differs by {}", describe(&before, &back)),
        );

        // open → close → open, as fast as the CLI goes: a reversal in flight.
        for a in [sc.open, sc.close, sc.open] {
            let o = run.h.ashd(a)?;
            if o.code != Some(0) {
                bail!("ashd {} failed: {:?}", a.join(" "), o.stderr.trim());
            }
        }
        let again = run.settled(&run.path(sc.name, "reopen", 0), settle)?;
        let again_diff = if sc.anchored { diff_anchored(&after, &again) } else { diff(&after, &again) };
        table.add(
            format!("{} open-close-open settles open", sc.name),
            again_diff <= SAME,
            format!(
                "differs from the plain open by {}{}",
                describe(&after, &again),
                if sc.anchored { format!("; {:.3} % after realigning the anchor", again_diff * 100.0) } else { String::new() }
            ),
        );
        for a in [sc.close, sc.open, sc.close] {
            let _ = run.h.ashd(a)?;
        }
        let gone = run.settled(&run.path(sc.name, "reclose", 0), settle)?;
        table.add(
            format!("{} close-open-close settles closed", sc.name),
            diff(&before, &gone) <= SAME,
            format!("differs from the start by {}", describe(&before, &gone)),
        );
        if !run.h.shell_alive() {
            bail!("ashd-shell died during {}", sc.name);
        }
    }

    if args.only.is_empty() || args.only.iter().any(|o| o == "toggle") {
        toggle(&mut run, &mut table, settle)?;
    }
    if args.only.is_empty() || args.only.iter().any(|o| o == "switch") {
        switch(&mut run, &mut table, settle)?;
    }
    if args.only.is_empty() || args.only.iter().any(|o| o == "dock") {
        dock(&mut run, &mut table, settle)?;
    }
    // The rail's second desk needs a window of its own (an empty trailing workspace is not a button).
    let two_desks = if args.only.is_empty() || args.only.iter().any(|o| o == "rail" || o == "hover") {
        second_desk(&run)?
    } else {
        None
    };
    if args.only.is_empty() || args.only.iter().any(|o| o == "rail") {
        rail_marker(&mut run, &mut table, settle)?;
    }
    if args.only.is_empty() || args.only.iter().any(|o| o == "hover") {
        hovers(&mut run, &mut table, settle, &args.only)?;
    }
    for mut k in two_desks.into_iter().flatten() {
        let _ = k.kill();
        let _ = k.wait();
    }
    if args.only.is_empty() || args.only.iter().any(|o| o == "toasts") {
        toasts(&mut run, &mut table, settle)?;
    }
    let want = |name: &str| args.only.is_empty() || args.only.iter().any(|o| o == name);
    // The second batch judges regions of the bar, so the shell restarts with frozen, narrow bar
    // values (`calm`: the clock aside, a region of the bar only changes when a scenario changes
    // it) and the network mock; `--env` overrides both. (The popups of the first batch are
    // anchored to bar icons whose position follows the live readouts, so that batch runs without.)
    if !reduced && ["bar", "launcher-ui", "osd", "mode", "shape"].iter().any(|n| want(n)) {
        let mut second: Vec<(&str, &str)> = vec![("ASHD_MOTION_SCALE", scale.as_str()), ("ASHD_FRAME_LOG", "1")];
        for kv in &args.env {
            if let Some((k, v)) = kv.split_once('=') {
                second.push((k, v));
            }
        }
        for (k, v) in [("ASHD_BAR_FIXTURE", "calm"), ("ASHD_NET_MOCK", "vpn")] {
            if !second.iter().any(|(ek, _)| *ek == k) {
                second.push((k, v));
            }
        }
        run.h.stop_shell();
        run.h.start_shell_env(&[], &second)?;
        run.h.wait_ping(Duration::from_secs(60)).context("restarting the shell for the second batch")?;
        util::sleep_ms(1500);
    }
    if want("bar") && !reduced {
        more::bar_items(&run, &mut table, settle)?;
    }
    if want("launcher-ui") && !reduced {
        more::launcher(&run, &mut table, settle)?;
    }
    if want("osd") && !reduced {
        more::osd(&run, &mut table, settle)?;
    }
    if want("mode") && !reduced {
        let kitty = more::mode(&run, &mut table, settle)?;
        if let Some(mut k) = kitty {
            let _ = k.kill();
            let _ = k.wait();
        }
    }
    if want("shape") && !reduced {
        more::shape(&run, &mut table, settle)?;
    }

    // Idle: nothing running, nothing drawn. The bar is the one surface fed by live data
    // (the 1 Hz system sample: a draw per sample, plus the 160 ms ease of the cpu / mem meters
    // when a value moved); every other surface must read 0, and the bar must not hold a
    // frame loop (a loop is ~60 per second, every second).
    println!("== idle ==");
    // At real speed: the meters ease for 160 ms per sample, not 160 ms × the slow-motion scale.
    run.h.stop_shell();
    let mut real_env: Vec<(&str, &str)> = vec![("ASHD_FRAME_LOG", "1")];
    if args.reduce_motion {
        real_env.push(("ASHD_REDUCE_MOTION", "1"));
    }
    for kv in &args.env {
        if let Some((k, v)) = kv.split_once('=') {
            real_env.push((k, v));
        }
    }
    run.h.start_shell_env(&[], &real_env)?;
    run.h.wait_ping(Duration::from_secs(60)).context("restarting the shell for the idle check")?;
    util::sleep_ms(3500);
    let mark = fs::read_to_string(&run.h.shell_log).unwrap_or_default().lines().count();
    util::sleep_ms(6500);
    let log = fs::read_to_string(&run.h.shell_log).unwrap_or_default();
    let samples: Vec<std::collections::HashMap<String, u32>> = log
        .lines()
        .skip(mark)
        .filter(|l| l.contains("frames/s"))
        .map(|l| {
            l.split_whitespace()
                .skip_while(|w| *w != "frames/s")
                .skip(1)
                .filter_map(|kv| kv.split_once('='))
                .filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?)))
                .collect()
        })
        .collect();
    let max = |k: &str| samples.iter().map(|s| s.get(k).copied().unwrap_or(0)).max().unwrap_or(0);
    let quiet_bar = samples.iter().filter(|s| s.get("bar").copied().unwrap_or(0) <= 3).count();
    let others = ["rail", "scratchpad", "scratchpad-zone", "overlay-passive", "overlay-modal"];
    table.add(
        "idle: rail, scratchpad and overlays draw no frames",
        !samples.is_empty() && others.iter().all(|k| max(k) == 0),
        format!("{} one-second samples; max per second: {}", samples.len(), others.iter().map(|k| format!("{k}={}", max(k))).collect::<Vec<_>>().join(" ")),
    );
    table.add(
        "idle: the bar holds no frame loop",
        !samples.is_empty() && max("bar") <= 20 && quiet_bar * 2 >= samples.len(),
        format!("bar frames/s: max {}, {} of {} samples at most 3 (data-driven: 1 Hz sample + meter ease)", max("bar"), quiet_bar, samples.len()),
    );
    let panics = run.h.panics();
    table.add("no panic in the shell log", panics.is_empty(), panics.first().cloned().unwrap_or_default());
    run.h.save_log("motion-shell.log");

    println!("\nmotion: {} checks, {} failed", table.rows.len(), table.failures());
    Ok(table.failures() == 0)
}
