//! The second batch of `cargo xtask motion` scenarios: the shell crates (bar, launcher, OSD,
//! canvas HUD / strip header / minimap) and the frame surfaces. Each one judges a *region*
//! of the screenshot (the whole picture changes for unrelated reasons: a clock, a window), with
//! the same rules as the first batch: a burst of screenshots taken while the change runs must
//! hold at least two frames that differ from both settled end states, and the settled end
//! state must be what it should be.
//!
//! Needs `--env ASHD_BAR_FIXTURE=calm --env ASHD_NET_MOCK=vpn` (the defaults of
//! `motion`): the bar then shows frozen values, so a region of it only changes when the scenario
//! changes it.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use image::RgbaImage;

use super::{Run, Table, between_crop, close_notification, count_diff, notify, pointer_to};
use crate::{shot, util};

/// x, y, width, height.
pub(super) type Rect = (u32, u32, u32, u32);

/// (frames in between, pixels that must differ from each end state to count as one)
const BIG: (usize, usize) = (2, 150);
const ONE: (usize, usize) = (1, 150);
/// A dot of 6 px: a handful of pixels is a frame.
const SMALL: (usize, usize) = (1, 12);
/// Icon-sized changes (a tray icon, the chevron, the now-playing glyph): two frames of a dozen pixels.
const TWO_SMALL: (usize, usize) = (2, 12);

/// The status block of the bar without the clock (whose minute ticks).
const STATUS: Rect = (80, 1036, 540, 44);
/// The whole bar.
const BAR: Rect = (80, 1034, 1840, 46);
/// The lower part of the status block, below the shadow of a toast standing on the bar.
const STATUS_LOW: Rect = (80, 1048, 540, 26);
/// The launcher card.
const LAUNCHER: Rect = (640, 170, 640, 480);
/// The launcher's list (below the input row).
const LIST: Rect = (650, 236, 620, 370);
/// The launcher's input row (mode chip, query).
const HEAD: Rect = (650, 181, 620, 52);
/// The OSD card.
const OSD: Rect = (820, 950, 280, 70);
/// The strip header's right end (`‹ ›` and the key hint); empty in Canvas mode.
const STRIP_END: Rect = (1680, 8, 240, 30);
/// The minimap card / chip at the bottom right of the main area.
const MINIMAP: Rect = (1640, 780, 270, 245);

fn crop(f: &RgbaImage, r: Rect) -> RgbaImage {
    image::imageops::crop_imm(f, r.0, r.1, r.2, r.3).to_image()
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().trim_matches('-').to_string()
}

fn same(a: &RgbaImage, b: &RgbaImage, r: Rect) -> usize {
    count_diff(&crop(a, r), &crop(b, r), 2)
}

fn grab(run: &Run<'_>, sc: &str, phase: &str) -> Result<Vec<RgbaImage>> {
    let mut frames = Vec::new();
    for n in 0..run.frames {
        let p = run.path(sc, phase, n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        frames.push(shot::load(&p)?);
    }
    Ok(frames)
}

/// One transition judged in `rect`: run `act`, screenshot a burst, wait for the shell to settle.
/// Passes when the region changed and at least `min_mid` frames of the burst lie between the
/// two end states. Returns the settled picture and the burst.
#[allow(clippy::too_many_arguments)]
fn step(
    run: &Run<'_>,
    table: &mut Table,
    settle: Duration,
    sc: &str,
    label: &str,
    rect: Rect,
    before: &RgbaImage,
    need: (usize, usize),
    act: &mut dyn FnMut(&Run<'_>) -> Result<()>,
) -> Result<(RgbaImage, Vec<RgbaImage>)> {
    act(run)?;
    let key = slug(label);
    let frames = grab(run, sc, &key)?;
    let after = run.settled(&run.path(sc, &key, 99), settle)?;
    let (rb, ra) = (crop(before, rect), crop(&after, rect));
    let rf: Vec<RgbaImage> = frames.iter().map(|f| crop(f, rect)).collect();
    let changed = count_diff(&rb, &ra, 2);
    let mid = rf.iter().filter(|f| count_diff(f, &rb, 0) >= need.1 && count_diff(f, &ra, 0) >= need.1).count();
    table.add(format!("{sc}: {label}"), changed >= need.1.min(40) && mid >= need.0, format!("{mid} in-between frames; {changed} px differ in the region"));
    Ok((after, frames))
}

/// `a` and `b` agree in `rect` (a transition really ended where it started).
fn restored(table: &mut Table, name: &str, a: &RgbaImage, b: &RgbaImage, rect: Rect) {
    let n = same(a, b, rect);
    table.add(name, n < 40, format!("{n} px differ in the region"));
}

fn wlrctl(run: &Run<'_>, args: &[&str]) -> Result<()> {
    let tool = util::resolve_tool("WLRCTL", "wlrctl", "wlrctl")?;
    let mut cmd = run.h.session.command(&tool);
    cmd.args(args);
    let c = util::run_captured(cmd, Duration::from_secs(10))?;
    if c.code != Some(0) {
        bail!("wlrctl {}: exit {:?}: {}", args.join(" "), c.code, c.stderr.trim());
    }
    Ok(())
}

fn click(run: &Run<'_>, x: i32, y: i32) -> Result<()> {
    pointer_to(run, x, y)?;
    util::sleep_ms(250);
    wlrctl(run, &["pointer", "click", "left"])
}

fn ashd(run: &Run<'_>, args: &[&str]) -> Result<()> {
    let o = run.h.ashd(args)?;
    if o.code != Some(0) {
        bail!("ashd {} failed: {:?} {}", args.join(" "), o.code, o.stderr.trim());
    }
    Ok(())
}

/// `<target>/<profile>/examples/<name>`, when it was built.
fn example(run: &Run<'_>, name: &str) -> Option<PathBuf> {
    let p = run.h.bins.ashd.parent()?.join("examples").join(name);
    p.exists().then_some(p)
}

/// A scripted mock (stdin commands) in the session.
fn spawn_mock(run: &Run<'_>, bin: &Path) -> Result<Child> {
    let mut cmd = run.h.session.command(bin);
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().with_context(|| format!("starting {}", bin.display()))
}

fn say(child: &mut Child, line: &str) -> Result<()> {
    let stdin = child.stdin.as_mut().context("the mock has no stdin")?;
    writeln!(stdin, "{line}")?;
    stdin.flush()?;
    Ok(())
}

/// Tray items, now-playing and the unread dot come and go in the bar's status block.
pub(super) fn bar_items(run: &Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== bar items ==");
    let before = run.settled(&run.path("bar-items", "before", 0), settle)?;

    if let Some(bin) = example(run, "mock_tray") {
        let mut child = spawn_mock(run, &bin)?;
        let (with, _) = step(run, table, settle, "bar", "tray items arrive", STATUS, &before, TWO_SMALL, &mut |_| Ok(()))?;
        say(&mut child, "quit")?;
        let (gone, _) = step(run, table, settle, "bar", "tray items leave", STATUS, &with, TWO_SMALL, &mut |_| Ok(()))?;
        let _ = child.wait();
        restored(table, "bar: the status block returns to the start after the tray items left", &before, &gone, STATUS);
    } else {
        println!("  (skipped: the mock_tray example is not built)");
    }

    if let Some(bin) = example(run, "mock_player") {
        let mut child = spawn_mock(run, &bin)?;
        let (with, _) = step(run, table, settle, "bar", "now-playing fades in", STATUS, &before, TWO_SMALL, &mut |_| Ok(()))?;
        say(&mut child, "quit")?;
        let (gone, _) = step(run, table, settle, "bar", "now-playing fades out", STATUS, &with, TWO_SMALL, &mut |_| Ok(()))?;
        let _ = child.wait();
        restored(table, "bar: the status block returns to the start after the player left", &before, &gone, STATUS);
    } else {
        println!("  (skipped: the mock_player example is not built)");
    }

    // The unread dot (fixture `worst-read`: only real notifications set it).
    let mut id = String::new();
    let (with, _) = step(run, table, settle, "bar", "the unread dot pops in", STATUS_LOW, &before, SMALL, &mut |r| {
        id = notify(r, "Bell dot")?;
        Ok(())
    })?;
    let (gone, _) = step(run, table, settle, "bar", "the unread dot shrinks away", STATUS_LOW, &with, SMALL, &mut |r| close_notification(r, &id))?;
    restored(table, "bar: the dot is gone again", &before, &gone, STATUS);
    Ok(())
}

/// The launcher: the selection plate glides, the mode chip swaps, an inline command's output
/// enters, and the card leaves with a command still running.
pub(super) fn launcher(run: &Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== launcher ==");
    pointer_to(run, 1700, 500)?;
    let closed = run.settled(&run.path("launcher-ui", "closed", 0), settle)?;
    ashd(run, &["launcher", "--open", "--mode", "emoji"])?;
    let open = run.settled(&run.path("launcher-ui", "emoji", 0), settle)?;

    let (down, _) = step(run, table, settle, "launcher", "the selection plate glides to the hovered row", LIST, &open, BIG, &mut |r| pointer_to(r, 900, 392))?;
    let (_, _) = step(run, table, settle, "launcher", "and glides back", LIST, &down, BIG, &mut |r| pointer_to(r, 900, 260))?;

    // the chip: a click on it leaves the mode
    let (_, _) = step(run, table, settle, "launcher", "the mode chip swaps when the mode is left", HEAD, &open, ONE, &mut |r| click(r, 677, 206))?;
    pointer_to(run, 1700, 500)?;
    ashd(run, &["launcher", "--close"])?;
    let closed2 = run.settled(&run.path("launcher-ui", "closed", 1), settle)?;
    restored(table, "launcher: closing restores the start", &closed, &closed2, LAUNCHER);

    // inline output
    ashd(run, &["launcher", "--open", "--mode", "run"])?;
    let run_open = run.settled(&run.path("launcher-ui", "run", 0), settle)?;
    let (ran, _) = step(run, table, settle, "launcher", "an inline command's output enters", LAUNCHER, &run_open, BIG, &mut |r| {
        util::sleep_ms(300);
        wlrctl(r, &["keyboard", "type", "echo hi\n"])
    })?;
    let _ = ran;
    ashd(run, &["launcher", "--close"])?;
    run.settled(&run.path("launcher-ui", "closed", 2), settle)?;

    // the host leaves while a command runs
    ashd(run, &["launcher", "--open", "--mode", "run"])?;
    util::sleep_ms(600);
    wlrctl(run, &["keyboard", "type", "sleep 4242\n"])?;
    util::sleep_ms(1500);
    let running_path = run.path("launcher-ui", "running", 0);
    shot::capture(&run.h.session, &running_path, run.h.via)?;
    let running = shot::load(&running_path)?;
    let (gone, _) = step(run, table, settle, "launcher", "closing with a command running still animates", LAUNCHER, &running, BIG, &mut |r| ashd(r, &["launcher", "--close"]))?;
    restored(table, "launcher: the card is gone after closing with a command running", &closed, &gone, LAUNCHER);
    let alive = run.h.status().is_ok();
    let left = {
        let mut cmd = std::process::Command::new("pgrep");
        cmd.args(["-f", "^sleep 4242$"]);
        util::run_captured(cmd, Duration::from_secs(5)).map(|c| c.stdout.split_whitespace().count()).unwrap_or(0)
    };
    table.add("launcher: the shell answers after the exit and the command is cleaned up", alive && left == 0, format!("{left} sleep process(es) left"));
    if left > 0 {
        let mut cmd = std::process::Command::new("pkill");
        cmd.args(["-f", "^sleep 4242$"]);
        let _ = util::run_captured(cmd, Duration::from_secs(5));
    }
    Ok(())
}

/// The OSD: fade in, fade out, a retrigger while it shows, and the icon / card cross-fade
/// when the kind changes (volume to brightness).
pub(super) fn osd(run: &Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== osd ==");
    let before = run.settled(&run.path("osd", "before", 0), settle)?;
    ashd(run, &["osd", "volume", "+5"])?;
    // one long burst: the fade in, the hold, the fade out
    let mut frames = Vec::new();
    let end = std::time::Instant::now() + Duration::from_secs(25);
    let rb = crop(&before, OSD);
    let mut seen = false;
    while std::time::Instant::now() < end {
        let p = run.path("osd", "life", frames.len());
        shot::capture(&run.h.session, &p, run.h.via)?;
        let f = crop(&shot::load(&p)?, OSD);
        let on = count_diff(&f, &rb, 2) > 200;
        frames.push(f);
        if seen && count_diff(frames.last().unwrap_or(&rb), &rb, 2) < 20 {
            break;
        }
        seen |= on;
    }
    let shown = frames.iter().max_by_key(|f| count_diff(f, &rb, 2)).cloned().unwrap_or_else(|| rb.clone());
    let top = frames.iter().position(|f| count_diff(f, &shown, 2) < 20).unwrap_or(0);
    let last_on = frames.iter().rposition(|f| count_diff(f, &shown, 2) < 20).unwrap_or(0);
    let fade_in = between_crop(&frames[..top.max(1)], &rb, &shown);
    let fade_out = between_crop(&frames[last_on..], &shown, &rb);
    table.add("osd: fades in", count_diff(&shown, &rb, 2) > 200 && fade_in >= 1, format!("{fade_in} in-between frames before it is fully there ({} frames)", frames.len()));
    table.add("osd: fades out", fade_out >= 1, format!("{fade_out} in-between frames after the hold"));
    let gone = run.settled(&run.path("osd", "gone", 0), settle)?;
    restored(table, "osd: gone after the hold", &before, &gone, OSD);

    // icon / card cross-fade: volume, then brightness while it is still up
    ashd(run, &["osd", "volume", "+5"])?;
    let mut a: Option<RgbaImage> = None;
    for n in 0..30 {
        let p = run.path("osd", "volume", n);
        shot::capture(&run.h.session, &p, run.h.via)?;
        let f = crop(&shot::load(&p)?, OSD);
        if let Some(prev) = &a
            && count_diff(prev, &f, 0) == 0
            && count_diff(&f, &rb, 2) > 200
        {
            break;
        }
        a = Some(f);
    }
    let a = a.context("no OSD frame")?;
    ashd(run, &["osd", "brightness", "+5"])?;
    let burst: Vec<RgbaImage> = grab(run, "osd", "kind")?.iter().map(|f| crop(f, OSD)).collect();
    let b = burst.iter().rev().find(|f| count_diff(f, &rb, 2) > 200).cloned().unwrap_or_else(|| a.clone());
    let mid = between_crop(&burst, &a, &b);
    table.add(
        "osd: the icon and card cross-fade when the kind changes",
        count_diff(&a, &b, 2) > 40 && mid >= 1,
        format!("{mid} in-between frames; {} px differ between volume and brightness", count_diff(&a, &b, 2)),
    );
    run.settled(&run.path("osd", "gone", 1), settle)?;
    // a retrigger in flight: two requests 300 ms apart, nothing stuck afterwards
    ashd(run, &["osd", "volume", "+5"])?;
    util::sleep_ms(300);
    ashd(run, &["osd", "volume", "-5"])?;
    let again = run.settled(&run.path("osd", "gone", 2), settle)?;
    restored(table, "osd: two requests 300 ms apart settle back to nothing", &before, &again, OSD);
    Ok(())
}

/// Scroll to Canvas and back with a window open: the strip header, the canvas chip and the
/// minimap swap in 280 ms; then the minimap's card collapses into its chip and expands again.
/// Also: a settled Canvas desktop draws no frames (the minimap is at 0 while hidden).
pub(super) fn mode(run: &Run<'_>, table: &mut Table, settle: Duration) -> Result<Option<std::process::Child>> {
    println!("== mode ==");
    let Some(kitty) = util::find_in_path("kitty") else {
        println!("  (skipped: no kitty on PATH)");
        return Ok(None);
    };
    pointer_to(run, 1000, 300)?;
    let mut cmd = run.h.session.command(kitty);
    cmd.args(["--class", "motionmode", "-o", "confirm_os_window_close=0"]).stdout(Stdio::null()).stderr(Stdio::null());
    let child = cmd.spawn().context("starting kitty")?;
    util::sleep_ms(2500);
    let scroll = run.settled(&run.path("mode", "scroll", 0), settle)?;

    let (canvas, frames) =
        step(run, table, settle, "mode", "the strip header leaves for Canvas", STRIP_END, &scroll, BIG, &mut |r| ashd(r, &["canvas", "toggle"]))?;
    // the minimap card is shown around the switch and hidden again once it is idle
    let rf: Vec<RgbaImage> = frames.iter().map(|f| crop(f, MINIMAP)).collect();
    let shown = between_crop(&rf, &crop(&scroll, MINIMAP), &crop(&canvas, MINIMAP));
    table.add("mode: the minimap card shows around the switch", frames.iter().any(|f| same(f, &scroll, MINIMAP) > 400), format!("{shown} in-between frames, {} frames with the card", frames.iter().filter(|f| same(f, &scroll, MINIMAP) > 400).count()));
    restored(table, "mode: the minimap is hidden (0 frames) once the view is idle", &scroll, &canvas, MINIMAP);

    // idle in Canvas: the chip and the (hidden) minimap draw nothing
    let mark = std::fs::read_to_string(&run.h.shell_log).unwrap_or_default().lines().count();
    util::sleep_ms(4200);
    let log = std::fs::read_to_string(&run.h.shell_log).unwrap_or_default();
    let mut max_passive = 0u32;
    let mut samples = 0;
    for l in log.lines().skip(mark).filter(|l| l.contains("frames/s")) {
        samples += 1;
        for kv in l.split_whitespace().skip_while(|w| *w != "frames/s").skip(1) {
            if let Some(("overlay-passive" | "overlay-modal" | "rail" | "scratchpad", v)) = kv.split_once('=') {
                max_passive = max_passive.max(v.parse().unwrap_or(0));
            }
        }
    }
    table.add(
        "mode: a settled Canvas desktop (chip up, minimap hidden) draws no frames",
        samples >= 3 && max_passive == 0,
        format!("{samples} one-second samples; max frames per second of the overlays, rail and scratchpad: {max_passive}"),
    );

    // the card collapses into its chip (a click on its header) and expands again
    ashd(run, &["canvas", "zoom-in"])?;
    pointer_to(run, 1680, 807)?;
    util::sleep_ms(1800);
    wlrctl(run, &["pointer", "move", "1", "0"])?;
    wlrctl(run, &["pointer", "move", "-1", "0"])?;
    let card_path = run.path("mode", "card", 0);
    shot::capture(&run.h.session, &card_path, run.h.via)?;
    let card = shot::load(&card_path)?;
    let (chip, _) = step(run, table, settle, "mode", "the minimap card collapses into its chip", MINIMAP, &card, ONE, &mut |r| {
        wlrctl(r, &["pointer", "click", "left"])?;
        pointer_to(r, 1856, 1006)
    })?;
    let (_, _) = step(run, table, settle, "mode", "and the chip expands into the card", MINIMAP, &chip, ONE, &mut |r| wlrctl(r, &["pointer", "click", "left"]))?;
    pointer_to(run, 1000, 300)?;

    let (back, _) = step(run, table, settle, "mode", "the strip header returns for Scroll", STRIP_END, &canvas, BIG, &mut |r| ashd(r, &["canvas", "toggle"]))?;
    restored(table, "mode: Scroll again looks like the start", &scroll, &back, STRIP_END);
    Ok(Some(child))
}

/// A theme with another frame shape recreates the frame surfaces; the new ones fade in.
pub(super) fn shape(run: &Run<'_>, table: &mut Table, settle: Duration) -> Result<()> {
    println!("== shape ==");
    let before = run.settled(&run.path("shape", "before", 0), settle)?;
    let (dusk, _) = step(run, table, settle, "shape", "recreated frame surfaces fade in (ash to dusk)", BAR, &before, BIG, &mut |r| ashd(r, &["theme", "set", "dusk", "--no-reload"]))?;
    let (back, _) = step(run, table, settle, "shape", "and back (dusk to ash)", BAR, &dusk, BIG, &mut |r| ashd(r, &["theme", "set", "ash", "--no-reload"]))?;
    restored(table, "shape: the bar is back as it was", &before, &back, BAR);
    Ok(())
}
