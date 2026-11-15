//! `cargo xtask fullscreen`: the shell next to the compositor's fullscreen handling (the ashd
//! niri fork: `fullscreen-workspaces`, `edge-reveal`, `create-workspaces-by-index`; set
//! `NIRI=` the fork's binary). A nested session with `assets/nested-niri-fullscreen.kdl`, the
//! shell, and a `kitty` window:
//!
//! 1. the window goes fullscreen: niri lists a workspace with `fullscreen_origin_id` /
//!    `fullscreen_window_id` and `idx` 0; the rail and the bar are covered (the compositor
//!    does it, the shell does not blank anything); the strip header (overlay layer) stays off;
//!    the OSD and a toast (overlay layer) still show over the window;
//! 2. the pointer rests at the left edge: the rail slides in over the window with its real
//!    content (the desk and the compact app entry, whose active marker is lit); at the bottom
//!    edge: the bar, with the status block exactly as before;
//! 3. the pointer leaves: both go again; leaving fullscreen puts everything back as it was.
//!
//! Screenshots land in `target-notes/fullscreen-<step>.png`.

use std::{fs, path::PathBuf, process::ExitCode, time::Duration};

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
pub struct FullscreenArgs {
    #[arg(long, default_value = "1920x1080")]
    pub size: String,
    /// Do not `cargo build -p ashd-shell -p ashd-cli` first.
    #[arg(long)]
    pub no_build: bool,
    #[arg(long, value_enum, default_value = "grim")]
    pub via: Via,
    /// A niri config to use instead of `assets/nested-niri-fullscreen.kdl`.
    #[arg(long)]
    pub niri_config: Option<PathBuf>,
}

impl FullscreenArgs {
    pub fn defaults() -> Self {
        Self { size: "1920x1080".into(), no_build: false, via: Via::Grim, niri_config: None }
    }
}

pub fn dispatch(args: FullscreenArgs) -> Result<ExitCode> {
    Ok(if run(&args)? { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

type Rect = (u32, u32, u32, u32);

/// The rail column.
const RAIL: Rect = (0, 0, 64, 1034);
/// The rail's fullscreen entry (right under the first desk button) and the desk's own button.
const FS_ENTRY: Rect = (0, 126, 64, 34);
const DESK_BUTTON: Rect = (0, 64, 64, 56);
/// The active marker column of the entry / of the desk.
const FS_MARKER: Rect = (0, 126, 4, 34);
const DESK_MARKER: Rect = (0, 64, 4, 56);
/// The status block of the bar without the clock (the pointer rests at the middle of the bar).
const STATUS: Rect = (80, 1036, 540, 44);
const OSD: Rect = (820, 950, 280, 70);
const TOAST: Rect = (0, 860, 420, 170);
/// The strip header's right end.
const STRIP_END: Rect = (1680, 8, 240, 30);

fn crop(f: &RgbaImage, r: Rect) -> RgbaImage {
    image::imageops::crop_imm(f, r.0, r.1, r.2, r.3).to_image()
}

/// Pixels of `r` that differ between `a` and `b` (any channel by more than 2).
fn changed(a: &RgbaImage, b: &RgbaImage, r: Rect) -> usize {
    crop(a, r).as_raw().chunks_exact(4).zip(crop(b, r).as_raw().chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count()
}

/// Pixels of `r` close to the accent colour (#E0A458).
fn accent(f: &RgbaImage, r: Rect) -> usize {
    crop(f, r).as_raw().chunks_exact(4).filter(|p| p[0].abs_diff(0xE0) < 12 && p[1].abs_diff(0xA4) < 12 && p[2].abs_diff(0x58) < 12).count()
}

/// Is `r` one flat colour (nothing of the shell drawn over the window)?
fn flat(f: &RgbaImage, r: Rect) -> bool {
    let c = crop(f, r);
    let first = *c.get_pixel(0, 0);
    c.pixels().all(|p| p.0.iter().zip(first.0.iter()).all(|(x, y)| x.abs_diff(*y) <= 2))
}

struct Run<'a> {
    h: &'a mut Harness,
    notes: PathBuf,
    n: usize,
    /// Where the pointer is (relative moves from here: going through a corner would rest it at an edge).
    pos: (i32, i32),
    rows: Vec<(String, bool, String)>,
}

impl Run<'_> {
    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!("  {} {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        self.rows.push((name.to_string(), ok, detail));
    }

    /// A settled screenshot saved as `fullscreen-<n>-<name>.png`.
    fn shot(&mut self, name: &str) -> Result<RgbaImage> {
        self.n += 1;
        let p = self.notes.join(format!("fullscreen-{:02}-{name}.png", self.n));
        shot::capture_settled(&self.h.session, &p, self.h.via, Duration::from_secs(6))
    }

    fn tool(&self, env: &str, bin: &str, args: &[&str]) -> Result<String> {
        let path = util::resolve_tool(env, bin, bin)?;
        let mut cmd = self.h.session.command(path);
        cmd.args(args);
        let c = util::run_captured(cmd, Duration::from_secs(10))?;
        if c.code != Some(0) {
            bail!("{bin} {}: exit {:?}: {}", args.join(" "), c.code, c.stderr.trim());
        }
        Ok(c.stdout)
    }

    /// The pointer to `(x, y)`: the first time from the corner, then relative to where it is
    /// (`x` 0 / `y` 1079 and the far sides overshoot, so they clamp to the edge).
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        if self.pos == (0, 0) {
            self.tool("WLRCTL", "wlrctl", &["pointer", "move", "-5000", "-5000"])?;
        }
        let (dx, dy) = (if x <= 0 { -5000 } else { x - self.pos.0 }, if y >= 1079 { 5000 } else { y - self.pos.1 });
        self.tool("WLRCTL", "wlrctl", &["pointer", "move", &dx.to_string(), &dy.to_string()])?;
        self.pos = (x.max(0), y.min(1079));
        Ok(())
    }

    fn niri_json(&self, what: &str) -> Result<serde_json::Value> {
        let out = self.tool("NIRI", "niri", &["msg", "--json", what])?;
        serde_json::from_str(out.trim()).with_context(|| format!("niri msg --json {what}: {out}"))
    }

    /// The fullscreen workspaces niri reports: (id, idx, origin, window).
    fn fullscreen_workspaces(&self) -> Result<Vec<(u64, u64, u64, Option<u64>)>> {
        let v = self.niri_json("workspaces")?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|w| {
                        let origin = w.get("fullscreen_origin_id")?.as_u64()?;
                        Some((w["id"].as_u64()?, w["idx"].as_u64()?, origin, w.get("fullscreen_window_id").and_then(|x| x.as_u64())))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn fullscreen_toggle(&self) -> Result<()> {
        self.tool("NIRI", "niri", &["msg", "action", "fullscreen-window"]).map(|_| ())
    }
}

pub fn run(args: &FullscreenArgs) -> Result<bool> {
    let (w, h) = parse_size(&args.size)?;
    if !args.no_build {
        harness::build_bins()?;
    }
    let config = args.niri_config.clone().unwrap_or_else(|| util::workspace_root().join("xtask/assets/nested-niri-fullscreen.kdl"));
    let opts = StartOpts { size: (w, h), with_bus: true, die_with_parent: true, niri_config: Some(config), ..Default::default() };
    let mut hs = Harness::new(&opts, args.via)?;
    let notes = util::notes_dir();
    fs::create_dir_all(&notes)?;
    hs.start_shell_env(&[], &[("ASHD_BAR_FIXTURE", "calm"), ("ASHD_NET_MOCK", "vpn")])?;
    hs.wait_ping(Duration::from_secs(90)).context("waiting for ashd ping")?;
    println!("fullscreen: ashd-shell up in {}", hs.session.root.display());
    util::sleep_ms(1500);
    let mut run = Run { h: &mut hs, notes, n: 0, pos: (0, 0), rows: Vec::new() };
    let kitty = util::find_in_path("kitty").context("kitty is not on PATH")?;

    println!("== a window, then fullscreen ==");
    run.pointer(960, 540)?;
    let normal = run.shot("normal")?;
    let mut cmd = run.h.session.command(kitty);
    cmd.args(["--class", "fullscreenprobe", "-o", "confirm_os_window_close=0"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().context("starting kitty")?;
    let up = util::wait_until(Duration::from_secs(20), Duration::from_millis(300), || {
        run.niri_json("windows").ok().filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    });
    run.check("a window opened", up.is_some(), "");
    util::sleep_ms(1500);
    let windowed = run.shot("windowed")?;
    let desk_before = run.fullscreen_workspaces()?;
    run.check("no fullscreen workspace before", desk_before.is_empty(), format!("{desk_before:?}"));
    run.fullscreen_toggle()?;
    util::sleep_ms(1200);
    let fs_ws = run.fullscreen_workspaces()?;
    run.check(
        "niri makes a fullscreen workspace (idx 0, origin and window ids set)",
        matches!(fs_ws.as_slice(), [(_, 0, _, Some(_))]),
        format!("{fs_ws:?}"),
    );
    let covered = run.shot("fullscreen")?;
    let alive = run.h.shell_alive();
    run.check("the shell is alive", alive, "");
    run.check("the rail is covered by the window", changed(&windowed, &covered, RAIL) > 500 && flat(&covered, (0, 300, 64, 400)), format!("{} px of the rail column changed", changed(&windowed, &covered, RAIL)));
    run.check("the bar is covered by the window", changed(&windowed, &covered, STATUS) > 300, format!("{} px of the status block changed", changed(&windowed, &covered, STATUS)));
    run.check("the strip header (overlay layer) is not drawn over the window", flat(&covered, STRIP_END), "");

    println!("== overlays over the fullscreen window ==");
    let _ = run.h.ashd(&["osd", "volume", "+5"])?;
    util::sleep_ms(500);
    let with_osd = run.shot("osd")?;
    run.check("the OSD shows over the fullscreen window", changed(&covered, &with_osd, OSD) > 200, format!("{} px", changed(&covered, &with_osd, OSD)));
    util::sleep_ms(2500);
    println!("== edge reveal ==");
    run.pointer(0, 500)?;
    util::sleep_ms(1500);
    let rail_in = run.shot("rail-revealed")?;
    let desk_btn = changed(&normal, &rail_in, DESK_BUTTON);
    run.check("the rail slides in over the window", changed(&covered, &rail_in, RAIL) > 1500, format!("{} px of the rail column differ from the covered picture", changed(&covered, &rail_in, RAIL)));
    run.check(
        "the revealed rail has real content: the desk and the lit marker of the app entry",
        accent(&rail_in, FS_MARKER) >= 10 && accent(&rail_in, DESK_MARKER) == 0 && changed(&windowed, &rail_in, FS_ENTRY) > 60,
        format!(
            "{} accent px at the entry's marker, {} at the desk's; entry differs from the windowed rail by {} px; desk button by {desk_btn}",
            accent(&rail_in, FS_MARKER),
            accent(&rail_in, DESK_MARKER),
            changed(&windowed, &rail_in, FS_ENTRY)
        ),
    );
    run.pointer(960, 1079)?;
    util::sleep_ms(1500);
    let bar_in = run.shot("bar-revealed")?;
    run.check("the bar slides in over the window", changed(&covered, &bar_in, STATUS) > 300, format!("{} px of the status block differ from the covered picture", changed(&covered, &bar_in, STATUS)));
    run.check(
        "the revealed bar has real content (its status block as before)",
        changed(&windowed, &bar_in, STATUS) < 60,
        format!("{} px differ from the windowed bar", changed(&windowed, &bar_in, STATUS)),
    );
    // (the compositor keeps a revealed panel while the pointer is on any revealed panel: the
    // rail stays here, which is its rule, not the shell's)
    run.pointer(960, 540)?;
    util::sleep_ms(1500);
    let hidden = run.shot("hidden-again")?;
    run.check(
        "both are gone when the pointer leaves",
        changed(&covered, &hidden, RAIL) < 60 && changed(&covered, &hidden, STATUS) < 60,
        format!("rail {} px, status {} px", changed(&covered, &hidden, RAIL), changed(&covered, &hidden, STATUS)),
    );

    println!("== a toast over the fullscreen window ==");
    let notify = util::find_in_path("notify-send").context("notify-send is not on PATH")?;
    let mut cmd = run.h.session.command(notify);
    cmd.args(["-p", "-t", "0", "-a", "fullscreen", "A toast", "over the fullscreen window"]);
    let id = util::run_captured(cmd, Duration::from_secs(10))?.stdout;
    util::sleep_ms(800);
    let with_toast = run.shot("toast")?;
    run.check("a toast shows over the fullscreen window", changed(&covered, &with_toast, TOAST) > 300, format!("{} px", changed(&covered, &with_toast, TOAST)));
    // (a toast's shadow reaches into the bar: close it before the next comparison)
    let id = id.trim().to_string();
    run.tool(
        "GDBUS",
        "gdbus",
        &["call", "--session", "--dest", "org.freedesktop.Notifications", "--object-path", "/org/freedesktop/Notifications", "--method", "org.freedesktop.Notifications.CloseNotification", &id],
    )?;
    util::sleep_ms(800);

    println!("== leaving fullscreen ==");
    run.fullscreen_toggle()?;
    util::sleep_ms(1200);
    let after = run.fullscreen_workspaces()?;
    run.check("niri removed the fullscreen workspace", after.is_empty(), format!("{after:?}"));
    let back = run.shot("windowed-again")?;
    run.check(
        "rail and bar are back as before",
        changed(&windowed, &back, RAIL) < 60 && changed(&windowed, &back, STATUS) < 60,
        format!("rail {} px, status {} px", changed(&windowed, &back, RAIL), changed(&windowed, &back, STATUS)),
    );
    let _ = child.kill();
    let _ = child.wait();

    let panics = run.h.panics();
    run.check("no panic in the shell log", panics.is_empty(), panics.first().cloned().unwrap_or_default());
    run.h.save_log("fullscreen-shell.log");
    let failed = run.rows.iter().filter(|r| !r.1).count();
    println!("\nfullscreen: {} checks, {failed} failed", run.rows.len());
    Ok(failed == 0)
}

