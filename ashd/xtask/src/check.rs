//! `cargo xtask shot-check`: start `ashd-shell` in a nested session, screenshot the output
//! and assert frame geometry and token colours with pixel probes.
//!
//! Geometry comes from the theme's `Shape` (TOKENS.md §6) and the desktop boards
//! (desk-05..09): attached themes have no margin (rail and scratchpad stop above the
//! full-width bar); floating themes inset every surface by `gap`, the rail loses
//! `bar_h + 3*gap` of height (it ends `gap` above the bottom row), the scratchpad is full
//! height (`h - 2*gap`, desk-06 `340 × 1060`), and the bottom row is a 560 status pill
//! followed by the dock pill(s), not one bordered bar.

use std::{process::ExitCode, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Args;
use image::{Rgba, RgbaImage};
use ashd_ipc::ShellStatus;
use ashd_tokens::{LayoutVariant, ThemeName, Tokens, metrics};

use crate::{
    harness::{self, Harness},
    nested::{StartOpts, parse_size},
    shot::Via,
    util::{self, wait_until},
};

#[derive(Args)]
pub struct CheckArgs {
    /// Theme to check: ash ink midnight moss dusk, or `all` (cycles them live through
    /// `ashd theme set --no-reload`).
    #[arg(long, default_value = "ash")]
    pub theme: String,
    #[arg(long, default_value = "1920x1080")]
    pub size: String,
    /// Do not `cargo build -p ashd-shell -p ashd-cli` first.
    #[arg(long)]
    pub no_build: bool,
    /// Screenshot capture path.
    #[arg(long, value_enum, default_value = "grim")]
    pub via: Via,
    /// Keep the session root (logs) instead of removing it.
    #[arg(long)]
    pub keep: bool,
    /// niri config replacing the built-in one (e.g. one with `workspace "scratch" { hidden }`).
    #[arg(long)]
    pub niri_config: Option<std::path::PathBuf>,
}

impl CheckArgs {
    pub fn defaults() -> Self {
        Self { theme: "ash".into(), size: "1920x1080".into(), no_build: false, via: Via::Grim, keep: false, niri_config: None }
    }
}

pub fn dispatch(args: CheckArgs) -> Result<ExitCode> {
    Ok(if run(&args)? { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

// ── geometry ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Expected output-local rectangles of the three frame surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    pub gap: i32,
    pub floating: bool,
    pub rail: Rect,
    /// The bar surface (full width minus margins).
    pub bar: Rect,
    /// The status block (attached: inside the bar, right border) / status pill (floating).
    pub status: Rect,
    /// Row of the bar used for horizontal border scans: below the rounded corners and
    /// above the 22px separators.
    pub probe_y: i32,
    pub scratch: Rect,
}

pub fn layout_for(t: &Tokens, w: i32, h: i32) -> Layout {
    let floating = t.shape.layout == LayoutVariant::Floating;
    let gap = if floating { t.shape.gap.round() as i32 } else { 0 };
    let rail_w = t.shape.rail_width.round() as i32;
    let bar_h = t.shape.dock_height.round() as i32;
    let scratch_w = metrics::SCRATCH_W.round() as i32;
    let status_w = metrics::STATUS_W.round() as i32;
    // Bar: L|R|B, margin gap, zone bar_h + gap; rail: T|B, margin gap on top only (the
    // bar's zone ends gap above the bar); scratchpad: attached like the rail, floating
    // full height (zone -1, margin gap).
    let side_h = h - bar_h - 3 * gap;
    let scratch_h = if floating { h - 2 * gap } else { side_h };
    let bar = Rect { x: gap, y: h - gap - bar_h, w: w - 2 * gap, h: bar_h };
    let radius = t.radius.panel.round() as i32;
    Layout {
        w,
        h,
        gap,
        floating,
        rail: Rect { x: gap, y: gap, w: rail_w, h: side_h },
        bar,
        status: Rect { x: bar.x, y: bar.y, w: status_w, h: bar_h },
        probe_y: bar.y + 3.max(radius + 2),
        scratch: Rect { x: w - gap - scratch_w, y: gap, w: scratch_w, h: scratch_h },
    }
}

// ── results ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Outcome {
    pub name: String,
    pub ok: bool,
    pub expected: String,
    pub actual: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub x: i32,
    pub y: i32,
    pub ok: bool,
}

#[derive(Default, Debug)]
pub struct Report {
    pub checks: Vec<Outcome>,
    pub marks: Vec<Mark>,
}

impl Report {
    fn push(&mut self, name: impl Into<String>, ok: bool, expected: impl Into<String>, actual: impl Into<String>) {
        self.checks.push(Outcome { name: name.into(), ok, expected: expected.into(), actual: actual.into() });
    }

    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok).count()
    }

    pub fn extend(&mut self, other: Report) {
        self.checks.extend(other.checks);
        self.marks.extend(other.marks);
    }

    fn eq<T: PartialEq + std::fmt::Display>(&mut self, name: impl Into<String>, expected: T, actual: T) {
        let ok = expected == actual;
        self.push(name, ok, expected.to_string(), actual.to_string());
    }

    /// The pixel at (x, y) must be one of `allowed` (label, colour).
    fn pixel(&mut self, name: impl Into<String>, img: &RgbaImage, x: i32, y: i32, allowed: &[(&str, [u8; 3])]) {
        let name = name.into();
        let expected = allowed.iter().map(|(l, c)| format!("{l} {}", hex(*c))).collect::<Vec<_>>().join(" | ");
        match px(img, x, y) {
            Some(c) => {
                let ok = allowed.iter().any(|(_, a)| *a == c);
                self.marks.push(Mark { x, y, ok });
                self.push(format!("{name} @({x},{y})"), ok, expected, hex(c));
            }
            None => {
                self.marks.push(Mark { x, y, ok: false });
                self.push(format!("{name} @({x},{y})"), false, expected, "outside the image");
            }
        }
    }
}

fn px(img: &RgbaImage, x: i32, y: i32) -> Option<[u8; 3]> {
    if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
        return None;
    }
    let Rgba([r, g, b, _]) = *img.get_pixel(x as u32, y as u32);
    Some([r, g, b])
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn rgb(c: ashd_tokens::Rgba) -> [u8; 3] {
    [c.r, c.g, c.b]
}

// ── status (IPC) checks ───────────────────────────────────────────────────────

pub fn check_status(st: &ShellStatus, lay: &Layout, theme: ThemeName, scratch: bool) -> Report {
    let mut r = Report::default();
    r.eq("status: theme", theme.to_string(), st.theme.to_string());
    r.eq("status: niri_connected", true, st.niri_connected);
    r.eq("status: no modal open", "none".to_string(), st.modal.clone().unwrap_or_else(|| "none".into()));
    let mut kinds: Vec<(&str, Rect)> = vec![("rail", lay.rail), ("bar", lay.bar)];
    if scratch {
        kinds.push(("scratchpad", lay.scratch));
    }
    for (kind, rect) in kinds {
        let found: Vec<_> = st.surfaces.iter().filter(|s| s.kind == kind).collect();
        match found.as_slice() {
            [s] => {
                let ok = (s.width - rect.w as f32).abs() < 0.5 && (s.height - rect.h as f32).abs() < 0.5;
                r.push(
                    format!("status: {kind} surface size"),
                    ok,
                    format!("{}x{}", rect.w, rect.h),
                    format!("{}x{}", s.width, s.height),
                );
            }
            other => r.push(format!("status: {kind} surface count"), false, "1", other.len().to_string()),
        }
    }
    if !scratch {
        let n = st.surfaces.iter().filter(|s| s.kind == "scratchpad").count();
        r.eq("status: no scratchpad surface while hidden", 0, n);
    }
    r
}

/// Every expected frame surface present with the expected size (used while waiting).
fn status_matches(st: &ShellStatus, lay: &Layout, theme: ThemeName, scratch: bool) -> bool {
    check_status(st, lay, theme, scratch).failures() == 0
}

// ── image checks ──────────────────────────────────────────────────────────────

/// Count consecutive pixels from (x, y) stepping (dx, dy) that are NOT `stop`.
fn run_len(img: &RgbaImage, mut x: i32, mut y: i32, dx: i32, dy: i32, stop: [u8; 3]) -> i32 {
    let mut n = 0;
    while let Some(c) = px(img, x, y) {
        if c == stop {
            break;
        }
        n += 1;
        x += dx;
        y += dy;
    }
    n
}

pub fn check_image(img: &RgbaImage, lay: &Layout, t: &Tokens, scratch: bool) -> Report {
    let mut r = Report::default();
    let c = &t.colors;
    let (bg, surface, line, frame, accent) = (rgb(c.bg), rgb(c.surface), rgb(c.line), rgb(c.frame), rgb(c.accent));
    let borders = [("line", line), ("frame", frame)];

    r.eq("image size", format!("{}x{}", lay.w, lay.h), format!("{}x{}", img.width(), img.height()));

    let (rail, bar, sc) = (lay.rail, lay.bar, lay.scratch);
    let ys = rail.y + rail.h * 85 / 100; // empty lower part of the rail / scratchpad
    let bxp = bar.x + bar.w * 62 / 100; // right of the dock, left of the bar's right group

    // main area and gaps
    r.pixel("main area = bg", img, lay.w / 2, lay.h / 2, &[("bg", bg)]);
    if lay.floating {
        r.pixel("screen-edge gap = bg", img, lay.gap / 2, rail.y + rail.h / 2, &[("bg", bg)]);
        r.pixel("rail/strip gap = bg", img, rail.x + rail.w, ys, &[("bg", bg)]);
    } else {
        r.pixel("main area right of rail = bg", img, rail.x + rail.w, ys, &[("bg", bg)]);
    }

    // rail
    r.pixel("rail fill = surface", img, rail.x + 2, ys, &[("surface", surface)]);
    r.pixel("rail fill beside border = surface", img, rail.x + rail.w - 2, ys, &[("surface", surface)]);
    r.pixel("rail right border", img, rail.x + rail.w - 1, ys, &borders);
    r.eq(
        format!("rail width measured on row {ys}"),
        rail.w,
        run_len(img, rail.x, ys, 1, 0, bg),
    );

    // bar: attached — the full-width bar, probed right of the dock; floating — the status
    // pill, probed in its empty right part (left of the bell, clear of rounded corners).
    let st = lay.status;
    let bx = if lay.floating { st.x + st.w - 20 } else { bxp };
    r.pixel("bar fill = surface", img, bx, bar.y + 3, &[("surface", surface)]);
    r.pixel("bar top border", img, bx, bar.y, &borders);
    r.pixel("above the bar = bg", img, bx, bar.y - 1, &[("bg", bg)]);
    r.eq(format!("bar height measured on column {bx}"), bar.h, run_len(img, bx, bar.y + bar.h - 1, 0, -1, bg));

    // status block (BarStart): 560 wide, 1px border on its right edge (attached: the
    // separator to the dock; floating: the pill's own frame).
    let status_edge = st.x + st.w - 1;
    let ey = if lay.floating { bar.y + bar.h / 2 } else { bar.y + 3 };
    r.pixel("status fill beside border = surface", img, status_edge - 1, ey, &[("surface", surface)]);
    r.pixel("status right border", img, status_edge, ey, &borders);
    let first_border = (st.x + 1..bar.x + bar.w).find(|&x| px(img, x, lay.probe_y).is_some_and(|p| p == line || p == frame));
    r.eq("status width measured (first border on the bar row)", st.w, first_border.map_or(-1, |x| x - st.x + 1));
    if lay.floating {
        // gap between the status pill and the dock row; the row ends at the main area's
        // right edge, `gap` left of a shown scratchpad.
        r.pixel("status/dock gap = bg", img, st.x + st.w + lay.gap / 2, bar.y + bar.h / 2, &[("bg", bg)]);
        if scratch {
            r.pixel("dock row/scratchpad gap = bg", img, sc.x - lay.gap / 2, bar.y + bar.h / 2, &[("bg", bg)]);
            r.pixel("scratchpad beside the bottom row = surface", img, sc.x + sc.w / 2, bar.y + bar.h / 2, &[("surface", surface)]);
        }
    }

    // scratchpad
    if scratch {
        r.pixel("scratchpad fill = surface", img, sc.x + sc.w / 2, ys, &[("surface", surface)]);
        r.pixel("scratchpad left border", img, sc.x, ys, &borders);
        r.pixel("left of scratchpad = bg", img, sc.x - 1, ys, &[("bg", bg)]);
        r.eq(
            format!("scratchpad width measured on row {ys}"),
            sc.w,
            run_len(img, sc.x + sc.w - 1, ys, -1, 0, bg),
        );
    } else {
        r.pixel("hidden scratchpad area = bg", img, sc.x + sc.w / 2, ys, &[("bg", bg)]);
        r.pixel("right screen edge = bg", img, lay.w - lay.gap - 1, ys, &[("bg", bg)]);
    }

    // accent on the active workspace (somewhere in the rail)
    let mut n = 0;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
    for y in rail.y..rail.y + rail.h {
        for x in rail.x..rail.x + rail.w {
            if px(img, x, y) == Some(accent) {
                n += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    let ok = n >= 8;
    if ok {
        r.marks.push(Mark { x: (x0 + x1) / 2, y: (y0 + y1) / 2, ok });
    }
    r.push(
        "accent (active workspace) in the rail",
        ok,
        format!("accent {} on >= 8 px", hex(accent)),
        if n > 0 { format!("{n} px in ({x0},{y0})-({x1},{y1})") } else { "no accent pixel".into() },
    );
    r
}

/// Draw the probe points on a copy: a 9x9 hollow square (green pass, red fail) with a dot.
pub fn annotate(img: &RgbaImage, marks: &[Mark]) -> RgbaImage {
    let mut out = img.clone();
    let (w, h) = (out.width() as i32, out.height() as i32);
    let mut put = |x: i32, y: i32, c: [u8; 4]| {
        if x >= 0 && y >= 0 && x < w && y < h {
            out.put_pixel(x as u32, y as u32, Rgba(c));
        }
    };
    for m in marks {
        let c = if m.ok { [0, 255, 80, 255] } else { [255, 0, 60, 255] };
        for d in -4..=4 {
            put(m.x + d, m.y - 4, c);
            put(m.x + d, m.y + 4, c);
            put(m.x - 4, m.y + d, c);
            put(m.x + 4, m.y + d, c);
        }
        put(m.x, m.y, [255, 0, 255, 255]);
    }
    out
}

// ── the run ───────────────────────────────────────────────────────────────────

pub fn parse_themes(s: &str) -> Result<Vec<ThemeName>> {
    if s == "all" {
        return Ok(ThemeName::ALL.to_vec());
    }
    s.split(',').map(|t| t.trim().parse::<ThemeName>().map_err(|_| anyhow::anyhow!("unknown theme `{t}`"))).collect()
}

fn print_report(title: &str, r: &Report) {
    println!("  {title}");
    for c in &r.checks {
        if c.ok {
            println!("    PASS  {:<58} {}", c.name, c.actual);
        } else {
            println!("    FAIL  {:<58} expected {}, got {}", c.name, c.expected, c.actual);
        }
    }
}

/// Wait until `ashd status` matches the layout of `theme` (surfaces are recreated on a
/// shape change). Returns the last status.
fn wait_layout(h: &mut Harness, lay: &Layout, theme: ThemeName, scratch: bool) -> Result<ShellStatus> {
    let mut last: Option<ShellStatus> = None;
    let _ = wait_until(Duration::from_secs(8), Duration::from_millis(150), || {
        let st = h.status().ok()?;
        let done = status_matches(&st, lay, theme, scratch);
        last = Some(st);
        done.then_some(())
    });
    match last {
        Some(s) => Ok(s),
        None => h.status(),
    }
}

/// Runs the check; returns true when every probe passed.
pub fn run(args: &CheckArgs) -> Result<bool> {
    let themes = parse_themes(&args.theme)?;
    let (w, h) = parse_size(&args.size)?;
    if !args.no_build {
        harness::build_bins()?;
    }
    let first = themes[0];
    let opts = StartOpts { size: (w, h), with_bus: false, theme: first, die_with_parent: true, niri_config: args.niri_config.clone() };
    let mut hs = Harness::new(&opts, args.via)?;
    let notes = util::notes_dir();
    std::fs::create_dir_all(&notes)?;

    // Persist the first theme in the session's private state dir, then start the shell.
    let o = hs.ashd(&["theme", "set", &first.to_string(), "--no-reload"])?;
    if o.code != Some(0) {
        bail!("ashd theme set {first} --no-reload failed: {}", o.stderr.trim());
    }
    hs.start_shell(&[])?;
    hs.wait_ping(Duration::from_secs(90)).context("waiting for ashd ping")?;
    println!("shot-check: ashd-shell is up in {}", hs.session.root.display());

    let mut total = Report::default();
    for (i, theme) in themes.iter().copied().enumerate() {
        let t = ashd_tokens::tokens(theme);
        let lay = layout_for(t, w as i32, h as i32);
        println!(
            "== theme {theme} ({}, rail {}, bar {}, gap {}) ==",
            if lay.floating { "floating" } else { "attached" },
            lay.rail.w,
            lay.bar.h,
            lay.gap
        );
        if i > 0 {
            let o = hs.ashd(&["theme", "set", &theme.to_string(), "--no-reload"])?;
            if o.code != Some(0) {
                bail!("ashd theme set {theme} --no-reload failed: {}", o.stderr.trim());
            }
            hs.session.set_niri_theme(theme)?;
        }
        let mut rep = Report::default();

        // scratchpad shown
        let o = hs.ashd(&["scratch", "open"])?;
        if o.code != Some(0) {
            rep.push("ashd scratch open", false, "exit 0", format!("exit {:?}: {}", o.code, o.stderr.trim()));
        }
        let st = wait_layout(&mut hs, &lay, theme, true)?;
        rep.extend(check_status(&st, &lay, theme, true));
        let shot_path = notes.join(format!("check-{theme}.png"));
        let mut img = hs.screenshot_settled(&shot_path)?;
        let mut img_rep = check_image(&img, &lay, t, true);
        if i > 0 && img_rep.failures() > 0 {
            // niri live-reloads its config and the crossfade takes 300 ms: allow one more settle.
            util::sleep_ms(700);
            img = hs.screenshot_settled(&shot_path)?;
            img_rep = check_image(&img, &lay, t, true);
        }
        annotate(&img, &img_rep.marks).save(notes.join(format!("check-{theme}-annotated.png")))?;
        rep.extend(img_rep);

        // scratchpad hidden
        let o = hs.ashd(&["scratch", "close"])?;
        if o.code != Some(0) {
            rep.push("ashd scratch close", false, "exit 0", format!("exit {:?}: {}", o.code, o.stderr.trim()));
        }
        let st = wait_layout(&mut hs, &lay, theme, false)?;
        rep.extend(check_status(&st, &lay, theme, false));
        let img = hs.screenshot_settled(&notes.join(format!("check-{theme}-noscratch.png")))?;
        rep.extend(check_image(&img, &lay, t, false));
        let _ = hs.ashd(&["scratch", "open"]);

        print_report("probes", &rep);
        total.extend(rep);
    }

    let mut health = Report::default();
    health.eq("ashd-shell still running", true, hs.shell_alive());
    let panics = hs.panics();
    health.push("no panics in the shell log", panics.is_empty(), "0", panics.len().to_string());
    print_report("shell health", &health);
    for p in panics.iter().take(5) {
        println!("    panic: {p}");
    }
    total.extend(health);
    if total.failures() > 0 {
        hs.save_log("check-shell.log");
    }
    if args.keep {
        hs.stop_shell();
        hs.session.stop_keep();
        println!("session root kept: {}", hs.session.root.display());
    }

    let failed = total.failures();
    println!(
        "\nshot-check: {} checks, {} failed — screenshots in {}",
        total.checks.len(),
        failed,
        notes.display()
    );
    Ok(failed == 0)
}

#[cfg(test)]
mod tests {
    use ashd_ipc::SurfaceStatus;

    use super::*;

    fn fill(img: &mut RgbaImage, r: Rect, c: [u8; 3]) {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], 255]));
            }
        }
    }

    /// A synthetic frame drawn exactly the way the design describes it.
    fn synth(theme: ThemeName, scratch: bool) -> (RgbaImage, Layout) {
        let t = ashd_tokens::tokens(theme);
        let lay = layout_for(t, 1920, 1080);
        let (bg, surface, line, accent) = (rgb(t.colors.bg), rgb(t.colors.surface), rgb(t.colors.line), rgb(t.colors.accent));
        let mut img = RgbaImage::from_pixel(1920, 1080, Rgba([bg[0], bg[1], bg[2], 255]));
        let border = |img: &mut RgbaImage, r: Rect| {
            fill(img, Rect { x: r.x, y: r.y, w: r.w, h: 1 }, line);
            fill(img, Rect { x: r.x, y: r.y + r.h - 1, w: r.w, h: 1 }, line);
            fill(img, Rect { x: r.x, y: r.y, w: 1, h: r.h }, line);
            fill(img, Rect { x: r.x + r.w - 1, y: r.y, w: 1, h: r.h }, line);
        };
        // floating: the status pill stands alone (the dock pills are not probed)
        let bottom = if lay.floating { lay.status } else { lay.bar };
        for r in [lay.rail, bottom].into_iter().chain(scratch.then_some(lay.scratch)) {
            fill(&mut img, r, surface);
            border(&mut img, r);
        }
        // status separator (attached) / the pill's right border (floating)
        fill(&mut img, Rect { x: lay.status.x + 559, y: lay.bar.y, w: 1, h: lay.bar.h }, line);
        // active workspace: a 3x40 accent bar
        fill(&mut img, Rect { x: lay.rail.x + 2, y: lay.rail.y + 70, w: 3, h: 40 }, accent);
        (img, lay)
    }

    #[test]
    fn expected_geometry_matches_the_design_tables() {
        let g = |t| layout_for(ashd_tokens::tokens(t), 1920, 1080);
        // TOKENS.md §6 "Measured": rail boxes.
        let ash = g(ThemeName::Ash);
        assert_eq!(ash.rail, Rect { x: 0, y: 0, w: 64, h: 1034 });
        assert_eq!(ash.bar, Rect { x: 0, y: 1034, w: 1920, h: 46 });
        assert_eq!(ash.scratch, Rect { x: 1580, y: 0, w: 340, h: 1034 });
        assert_eq!(g(ThemeName::Ink).rail, Rect { x: 10, y: 10, w: 92, h: 998 });
        assert_eq!(g(ThemeName::Midnight).rail, Rect { x: 10, y: 10, w: 60, h: 996 });
        assert_eq!(g(ThemeName::Moss).rail, Rect { x: 8, y: 8, w: 64, h: 1004 });
        assert_eq!(g(ThemeName::Dusk).rail, Rect { x: 12, y: 12, w: 92, h: 990 });
        // desk-06..09: full-height floating scratchpads and the status pills.
        assert_eq!(g(ThemeName::Ink).scratch, Rect { x: 1570, y: 10, w: 340, h: 1060 });
        assert_eq!(g(ThemeName::Midnight).scratch, Rect { x: 1570, y: 10, w: 340, h: 1060 });
        assert_eq!(g(ThemeName::Moss).scratch, Rect { x: 1572, y: 8, w: 340, h: 1064 });
        assert_eq!(g(ThemeName::Dusk).scratch, Rect { x: 1568, y: 12, w: 340, h: 1056 });
        assert_eq!(g(ThemeName::Ink).status, Rect { x: 10, y: 1018, w: 560, h: 52 });
        assert_eq!(g(ThemeName::Midnight).status, Rect { x: 10, y: 1016, w: 560, h: 54 });
        assert_eq!(g(ThemeName::Moss).status, Rect { x: 8, y: 1020, w: 560, h: 52 });
        assert_eq!(g(ThemeName::Dusk).status, Rect { x: 12, y: 1014, w: 560, h: 54 });
    }

    #[test]
    fn probes_pass_on_a_correct_frame_for_every_theme() {
        for theme in ThemeName::ALL {
            let (img, lay) = synth(theme, true);
            let rep = check_image(&img, &lay, ashd_tokens::tokens(theme), true);
            let failed: Vec<_> = rep.checks.iter().filter(|c| !c.ok).collect();
            assert!(failed.is_empty(), "{theme}: {failed:?}");
            let (img, lay) = synth(theme, false);
            let rep = check_image(&img, &lay, ashd_tokens::tokens(theme), false);
            let failed: Vec<_> = rep.checks.iter().filter(|c| !c.ok).collect();
            assert!(failed.is_empty(), "{theme} (no scratchpad): {failed:?}");
        }
    }

    #[test]
    fn probes_catch_wrong_colours_and_sizes() {
        let (mut img, lay) = synth(ThemeName::Ash, true);
        // rail painted 4 px too narrow
        fill(&mut img, Rect { x: 60, y: 0, w: 4, h: 1034 }, [0x11, 0x11, 0x10]);
        let rep = check_image(&img, &lay, ashd_tokens::tokens(ThemeName::Ash), true);
        let failed: Vec<&str> = rep.checks.iter().filter(|c| !c.ok).map(|c| c.name.as_str()).collect();
        assert!(failed.iter().any(|n| n.starts_with("rail width")), "{failed:?}");
        assert!(failed.iter().any(|n| n.starts_with("rail right border")), "{failed:?}");

        // wrong accent
        let (mut img, lay) = synth(ThemeName::Ash, true);
        fill(&mut img, Rect { x: 2, y: 70, w: 3, h: 40 }, [0xFF, 0x5B, 0x3A]);
        let rep = check_image(&img, &lay, ashd_tokens::tokens(ThemeName::Ash), true);
        assert!(rep.checks.iter().any(|c| !c.ok && c.name.starts_with("accent")));

        // a one-bit colour error is a failure (exact tokens)
        let (mut img, lay) = synth(ThemeName::Ash, true);
        img.put_pixel(960, 540, Rgba([0x11, 0x11, 0x11, 255]));
        let rep = check_image(&img, &lay, ashd_tokens::tokens(ThemeName::Ash), true);
        assert!(rep.checks.iter().any(|c| !c.ok && c.name.starts_with("main area")));
    }

    #[test]
    fn annotation_marks_points() {
        let (img, lay) = synth(ThemeName::Ash, true);
        let rep = check_image(&img, &lay, ashd_tokens::tokens(ThemeName::Ash), true);
        let out = annotate(&img, &rep.marks);
        assert_eq!(out.dimensions(), img.dimensions());
        assert_ne!(out.as_raw(), img.as_raw());
        let m = rep.marks[0];
        assert_eq!(*out.get_pixel(m.x as u32, m.y as u32), Rgba([255, 0, 255, 255]));
    }

    #[test]
    fn status_checks() {
        let lay = layout_for(ashd_tokens::tokens(ThemeName::Ash), 1920, 1080);
        let surf = |kind: &str, w: f32, h: f32| SurfaceStatus { kind: kind.into(), output: "winit".into(), width: w, height: h };
        let mut st = ShellStatus {
            version: "0".into(),
            theme: ThemeName::Ash,
            niri_connected: true,
            modal: None,
            surfaces: vec![surf("bar", 1920., 46.), surf("rail", 64., 1034.), surf("scratchpad", 340., 1034.)],
        };
        assert_eq!(check_status(&st, &lay, ThemeName::Ash, true).failures(), 0);
        assert!(check_status(&st, &lay, ThemeName::Ash, false).failures() > 0);
        st.surfaces[1].width = 92.;
        assert!(check_status(&st, &lay, ThemeName::Ash, true).failures() > 0);
        st.surfaces.pop();
        st.surfaces[1].width = 64.;
        assert_eq!(check_status(&st, &lay, ThemeName::Ash, false).failures(), 0);
    }

    #[test]
    fn theme_list_parsing() {
        assert_eq!(parse_themes("all").unwrap().len(), 5);
        assert_eq!(parse_themes("moss").unwrap(), vec![ThemeName::Moss]);
        assert!(parse_themes("neon").is_err());
    }
}
