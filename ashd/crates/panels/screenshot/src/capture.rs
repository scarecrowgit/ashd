//! Pure logic of the capture overlay (no GPUI, no I/O): selection geometry (create / move /
//! resize from the eight handles / nudge, all clamped to the output), the mapping between
//! the logical selection and the pixels of the still, window rectangles derived from niri's
//! window layouts, file naming, recorder command lines and the small text helpers of the
//! UI. Everything is in *output-local logical pixels* unless a type says otherwise.

use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use ashd_ipc::CaptureMode;
use ashd_niri::niri_ipc::Window;

// ── points, rectangles ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Size of the output (the bounds every selection is clamped to).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dim {
    pub w: f32,
    pub h: f32,
}

impl Dim {
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Smallest selection (per side, logical px) that counts as a selection; a plain click
/// without a drag creates nothing.
pub const MIN_SELECTION: f32 = 2.0;

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// The whole output.
    pub const fn full(bounds: Dim) -> Self {
        Self::new(0.0, 0.0, bounds.w, bounds.h)
    }

    /// The rectangle spanned by two corners, in any order.
    pub fn from_corners(a: Pt, b: Pt) -> Self {
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// Edges rounded to whole logical px (the label and the outline stay crisp).
    pub fn rounded(self) -> Self {
        let (x0, y0) = (self.x.round(), self.y.round());
        Self::new(x0, y0, self.right().round() - x0, self.bottom().round() - y0)
    }

    /// The part inside `[0, bounds]`.
    pub fn clamp_to(self, bounds: Dim) -> Self {
        let x0 = self.x.clamp(0.0, bounds.w);
        let y0 = self.y.clamp(0.0, bounds.h);
        let x1 = self.right().clamp(0.0, bounds.w);
        let y1 = self.bottom().clamp(0.0, bounds.h);
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Same size, moved the least that puts it inside `bounds` (a rectangle larger than
    /// the bounds is pinned to the top-left).
    pub fn shifted_inside(self, bounds: Dim) -> Self {
        let x = self.x.min(bounds.w - self.w).max(0.0);
        let y = self.y.min(bounds.h - self.h).max(0.0);
        Self::new(x, y, self.w, self.h)
    }

    pub fn is_usable(&self) -> bool {
        self.w >= MIN_SELECTION && self.h >= MIN_SELECTION
    }
}

// ── handles and drags ─────────────────────────────────────────────────────────

/// The eight resize handles: corners (drawn as 8×8 squares) and edges (invisible bands).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Handle {
    #[cfg(test)]
    pub const ALL: [Handle; 8] =
        [Handle::N, Handle::NE, Handle::E, Handle::SE, Handle::S, Handle::SW, Handle::W, Handle::NW];
    pub const CORNERS: [Handle; 4] = [Handle::NW, Handle::NE, Handle::SE, Handle::SW];

    pub fn north(self) -> bool {
        matches!(self, Handle::N | Handle::NE | Handle::NW)
    }

    pub fn south(self) -> bool {
        matches!(self, Handle::S | Handle::SE | Handle::SW)
    }

    pub fn west(self) -> bool {
        matches!(self, Handle::W | Handle::NW | Handle::SW)
    }

    pub fn east(self) -> bool {
        matches!(self, Handle::E | Handle::NE | Handle::SE)
    }

    /// Where the handle sits on `r`.
    pub fn point(self, r: &Rect) -> Pt {
        let x = if self.west() {
            r.x
        } else if self.east() {
            r.right()
        } else {
            r.x + r.w / 2.0
        };
        let y = if self.north() {
            r.y
        } else if self.south() {
            r.bottom()
        } else {
            r.y + r.h / 2.0
        };
        Pt::new(x, y)
    }
}

/// What the pointer grabs on a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    Handle(Handle),
    Body,
}

/// Half-size of a corner handle's hit box.
pub const CORNER_HIT: f32 = 9.0;
/// Half-width of an edge's hit band.
pub const EDGE_HIT: f32 = 5.0;

/// What is under `p` for the selection `sel`: a corner, an edge, the inside, nothing.
pub fn hit_test(sel: &Rect, p: Pt) -> Option<Grab> {
    for h in Handle::CORNERS {
        let c = h.point(sel);
        if (p.x - c.x).abs() <= CORNER_HIT && (p.y - c.y).abs() <= CORNER_HIT {
            return Some(Grab::Handle(h));
        }
    }
    let in_x = p.x >= sel.x - EDGE_HIT && p.x <= sel.right() + EDGE_HIT;
    let in_y = p.y >= sel.y - EDGE_HIT && p.y <= sel.bottom() + EDGE_HIT;
    if in_y && (p.x - sel.x).abs() <= EDGE_HIT {
        return Some(Grab::Handle(Handle::W));
    }
    if in_y && (p.x - sel.right()).abs() <= EDGE_HIT {
        return Some(Grab::Handle(Handle::E));
    }
    if in_x && (p.y - sel.y).abs() <= EDGE_HIT {
        return Some(Grab::Handle(Handle::N));
    }
    if in_x && (p.y - sel.bottom()).abs() <= EDGE_HIT {
        return Some(Grab::Handle(Handle::S));
    }
    sel.contains(p).then_some(Grab::Body)
}

/// A pointer drag in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    /// A new selection from `anchor` to the pointer.
    Create { anchor: Pt },
    /// The selection follows the pointer (`grab` = where it was picked up).
    Move { grab: Pt, origin: Rect },
    /// One handle follows the pointer; dragging past the opposite edge flips the rectangle.
    Resize { handle: Handle, grab: Pt, origin: Rect },
}

impl Drag {
    /// The drag a press at `p` starts, given the current selection.
    pub fn start(sel: Option<Rect>, p: Pt) -> Drag {
        match sel.and_then(|s| hit_test(&s, p).map(|g| (s, g))) {
            Some((origin, Grab::Handle(handle))) => Drag::Resize { handle, grab: p, origin },
            Some((origin, Grab::Body)) => Drag::Move { grab: p, origin },
            None => Drag::Create { anchor: p },
        }
    }

    /// The selection with the pointer at `cursor`, clamped to `bounds`, whole px.
    pub fn apply(&self, cursor: Pt, bounds: Dim) -> Rect {
        let c = clamp_pt(cursor, bounds);
        match *self {
            Drag::Create { anchor } => Rect::from_corners(clamp_pt(anchor, bounds), c).rounded(),
            Drag::Move { grab, origin } => {
                let moved = Rect::new(origin.x + (cursor.x - grab.x), origin.y + (cursor.y - grab.y), origin.w, origin.h);
                moved.shifted_inside(bounds).rounded()
            }
            Drag::Resize { handle, grab, origin } => {
                let (dx, dy) = (cursor.x - grab.x, cursor.y - grab.y);
                let (mut l, mut t, mut r, mut b) = (origin.x, origin.y, origin.right(), origin.bottom());
                if handle.west() {
                    l += dx;
                }
                if handle.east() {
                    r += dx;
                }
                if handle.north() {
                    t += dy;
                }
                if handle.south() {
                    b += dy;
                }
                let a = clamp_pt(Pt::new(l, t), bounds);
                let z = clamp_pt(Pt::new(r, b), bounds);
                Rect::from_corners(a, z).rounded()
            }
        }
    }
}

fn clamp_pt(p: Pt, bounds: Dim) -> Pt {
    Pt::new(p.x.clamp(0.0, bounds.w), p.y.clamp(0.0, bounds.h))
}

// ── keyboard ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Pixels moved by one arrow key press: 1, or 10 with Shift.
pub fn nudge_step(shift: bool) -> f32 {
    if shift { 10.0 } else { 1.0 }
}

/// Arrow-key nudge: moves the selection by `step`, or (`resize`) moves its right / bottom
/// edge (Left / Up shrink, Right / Down grow). Stays inside `bounds`, at least 1 px.
pub fn nudge(sel: Rect, dir: Dir, step: f32, resize: bool, bounds: Dim) -> Rect {
    let (dx, dy) = match dir {
        Dir::Left => (-step, 0.0),
        Dir::Right => (step, 0.0),
        Dir::Up => (0.0, -step),
        Dir::Down => (0.0, step),
    };
    if !resize {
        return Rect::new(sel.x + dx, sel.y + dy, sel.w, sel.h).shifted_inside(bounds).rounded();
    }
    let w = (sel.w + dx).clamp(1.0, (bounds.w - sel.x).max(1.0));
    let h = (sel.h + dy).clamp(1.0, (bounds.h - sel.y).max(1.0));
    Rect::new(sel.x, sel.y, w, h).rounded()
}

/// A selection to start from when the first arrow key is pressed with none: the middle
/// half of the output.
pub fn default_selection(bounds: Dim) -> Rect {
    Rect::new(bounds.w / 4.0, bounds.h / 4.0, bounds.w / 2.0, bounds.h / 2.0).rounded()
}

// ── logical → still pixels ────────────────────────────────────────────────────

/// A pixel rectangle inside the still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Maps the logical output (what the overlay and niri's window rectangles are in) to the
/// pixels of the still niri rendered. The factor is *measured* (image size over logical
/// size) rather than read from the output's `scale`, so rounding of the logical size at a
/// fractional scale (1.25, 1.5 …) cannot push the crop off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScaleMap {
    pub logical: Dim,
    pub image: (u32, u32),
}

impl ScaleMap {
    pub fn factors(&self) -> (f64, f64) {
        (f64::from(self.image.0) / f64::from(self.logical.w.max(1.0)), f64::from(self.image.1) / f64::from(self.logical.h.max(1.0)))
    }

    /// The pixels of `r`: edges rounded to the nearest pixel (so neighbouring rectangles
    /// stay adjacent), clamped to the image, at least 1×1.
    pub fn to_pixels(self, r: Rect) -> PixelRect {
        let (sx, sy) = self.factors();
        let (iw, ih) = (self.image.0.max(1), self.image.1.max(1));
        let edge = |v: f32, s: f64, max: u32| ((f64::from(v) * s).round().max(0.0) as u32).min(max);
        let x0 = edge(r.x, sx, iw - 1);
        let y0 = edge(r.y, sy, ih - 1);
        let x1 = edge(r.right(), sx, iw).max(x0 + 1);
        let y1 = edge(r.bottom(), sy, ih).max(y0 + 1);
        PixelRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }
}

// ── windows ───────────────────────────────────────────────────────────────────

/// A window as drawn on the output.
#[derive(Clone, Debug, PartialEq)]
pub struct WinRect {
    pub id: u64,
    pub rect: Rect,
    pub title: String,
    pub app_id: String,
    pub floating: bool,
    pub focused: bool,
}

/// The visible tiles of workspace `workspace`, topmost first (floating windows above tiled
/// ones, newest focus first), clipped to the output. niri's `tile_pos_in_workspace_view`
/// is in output-local logical px (the workspace view starts at the output's origin and the
/// working area is already included) and is `None` for tiles scrolled out of view.
pub fn windows_in_view<'a>(windows: impl IntoIterator<Item = &'a Window>, workspace: u64, bounds: Dim) -> Vec<WinRect> {
    let windows: Vec<&Window> = windows.into_iter().filter(|w| w.workspace_id == Some(workspace)).collect();
    let zoom = canvas_zoom(&windows);
    let mut v: Vec<(Option<(u64, u32)>, WinRect)> = windows
        .into_iter()
        .filter_map(|w| {
            let (x, y) = w.layout.tile_pos_in_workspace_view?;
            let (tw, th) = w.layout.tile_size;
            let rect = Rect::new(x as f32, y as f32, (tw * zoom) as f32, (th * zoom) as f32).clamp_to(bounds);
            rect.is_usable().then(|| {
                (
                    w.focus_timestamp.map(|t| (t.secs, t.nanos)),
                    WinRect {
                        id: w.id,
                        rect,
                        title: w.title.clone().unwrap_or_default(),
                        app_id: w.app_id.clone().unwrap_or_default(),
                        floating: w.is_floating,
                        focused: w.is_focused,
                    },
                )
            })
        })
        .collect();
    v.sort_by(|a, b| {
        b.1.floating
            .cmp(&a.1.floating)
            .then_with(|| b.0.cmp(&a.0))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    v.into_iter().map(|(_, w)| w).collect()
}

/// The zoom of a canvas-mode workspace (1.0 for the scrolling layout and when it cannot be
/// told). On a canvas niri reports the tile position *in the view* but the tile size
/// *unscaled*; two windows that have both a plane position (`pos_in_canvas`) and a view
/// position give the zoom as the ratio of their distances.
pub fn canvas_zoom(windows: &[&Window]) -> f64 {
    let pts: Vec<((f64, f64), (f64, f64))> = windows
        .iter()
        .filter(|w| !w.layout.is_pinned)
        .filter_map(|w| Some((w.layout.pos_in_canvas?, w.layout.tile_pos_in_workspace_view?)))
        .collect();
    for (i, (pa, va)) in pts.iter().enumerate() {
        for (pb, vb) in &pts[i + 1..] {
            for (dp, dv) in [(pa.0 - pb.0, va.0 - vb.0), (pa.1 - pb.1, va.1 - vb.1)] {
                if dp.abs() > 50.0 {
                    let z = dv / dp;
                    if z.is_finite() && z > 0.05 && z < 20.0 {
                        return z;
                    }
                }
            }
        }
    }
    1.0
}

/// The topmost window under `p`.
pub fn window_at(windows: &[WinRect], p: Pt) -> Option<&WinRect> {
    windows.iter().find(|w| w.rect.contains(p))
}

// ── outputs, recording targets ────────────────────────────────────────────────

/// An output as the compositor lays it out (logical px, global coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct OutputGeom {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub scale: f64,
}

impl OutputGeom {
    pub fn bounds(&self) -> Dim {
        Dim::new(self.w as f32, self.h as f32)
    }
}

/// A rectangle in the compositor's global logical space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// What a recorder is pointed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecTarget {
    /// A whole output.
    Output(String),
    /// A rectangle (global logical coordinates) that lies on one output.
    Region { output: String, rect: GlobalRect },
}

/// The recording target for `sel` (output-local) on `out`: the whole output when `sel`
/// covers it, else a region in global coordinates with even width / height (video codecs
/// need even dimensions).
pub fn rec_target(out: &OutputGeom, sel: Rect) -> RecTarget {
    let sel = sel.clamp_to(out.bounds()).rounded();
    if sel.x <= 0.0 && sel.y <= 0.0 && sel.w >= out.w as f32 && sel.h >= out.h as f32 {
        return RecTarget::Output(out.name.clone());
    }
    let even = |v: f32| ((v as u32) & !1).max(2);
    let rect = GlobalRect { x: out.x + sel.x as i32, y: out.y + sel.y as i32, w: even(sel.w), h: even(sel.h) };
    RecTarget::Region { output: out.name.clone(), rect }
}

/// The screen recorders we know how to drive, in order of preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderKind {
    WfRecorder,
    GpuScreenRecorder,
}

impl RecorderKind {
    pub const PREFERRED: [RecorderKind; 2] = [RecorderKind::WfRecorder, RecorderKind::GpuScreenRecorder];

    /// The executable name looked up on `PATH`.
    pub fn binary(self) -> &'static str {
        match self {
            RecorderKind::WfRecorder => "wf-recorder",
            RecorderKind::GpuScreenRecorder => "gpu-screen-recorder",
        }
    }

    /// `wf-recorder` asks the compositor for a frame only when the screen changed, and a
    /// recorder waiting for one cannot be stopped (SIGINT is only looked at between frames):
    /// on a still screen the mp4 would never be finalized. `-D -r 60` records at a constant
    /// 60 fps instead.
    const WF_COMMON: [&'static str; 3] = ["-D", "-r", "60"];

    /// Command-line arguments that record `target` into `file`.
    ///
    /// - `wf-recorder`: `-o <output>` or `-g "x,y wxh"` (global logical coordinates, the
    ///   `slurp` format; the output is derived from the geometry), `-f <file>`.
    /// - `gpu-screen-recorder`: `-w <output>` or `-w region -region WxH+X+Y`, 60 fps,
    ///   `-o <file>`.
    pub fn args(self, target: &RecTarget, file: &Path) -> Vec<String> {
        let file = file.to_string_lossy().into_owned();
        match (self, target) {
            (RecorderKind::WfRecorder, RecTarget::Output(name)) => {
                let mut v = Self::WF_COMMON.map(String::from).to_vec();
                v.extend(["-o".into(), name.clone(), "-f".into(), file]);
                v
            }
            (RecorderKind::WfRecorder, RecTarget::Region { rect, .. }) => {
                let mut v = Self::WF_COMMON.map(String::from).to_vec();
                v.extend(["-g".into(), format!("{},{} {}x{}", rect.x, rect.y, rect.w, rect.h), "-f".into(), file]);
                v
            }
            (RecorderKind::GpuScreenRecorder, RecTarget::Output(name)) => {
                vec!["-w".into(), name.clone(), "-f".into(), "60".into(), "-o".into(), file]
            }
            (RecorderKind::GpuScreenRecorder, RecTarget::Region { rect, .. }) => vec![
                "-w".into(),
                "region".into(),
                "-region".into(),
                format!("{}x{}+{}+{}", rect.w, rect.h, rect.x, rect.y),
                "-f".into(),
                "60".into(),
                "-o".into(),
                file,
            ],
        }
    }
}

/// Environment variables a recorder must *not* inherit: the dev shell's library path and
/// anything that could point it at another display than the shell's own.
pub const RECORDER_ENV_REMOVE: [&str; 4] = ["LD_LIBRARY_PATH", "LD_PRELOAD", "DISPLAY", "WAYLAND_SOCKET"];

// ── files ─────────────────────────────────────────────────────────────────────

const STAMP: &str = "%Y-%m-%d %H-%M-%S";

/// `Screenshot from 2026-11-14 15-04-05.png`.
pub fn screenshot_file_name(t: NaiveDateTime) -> String {
    format!("Screenshot from {}.png", t.format(STAMP))
}

/// `Recording 2026-11-14 15-04-05.mp4`.
pub fn recording_file_name(t: NaiveDateTime) -> String {
    format!("Recording {}.mp4", t.format(STAMP))
}

/// `dir/name`, or `dir/name (2).ext`, `(3)` … when `exists` says the name is taken (two
/// shots in the same second).
pub fn unique_path(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !exists(p))
        .expect("an unbounded range always yields a free name")
}

// ── UI state helpers ──────────────────────────────────────────────────────────

/// Seconds to wait between confirming and capturing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delay {
    #[default]
    Off,
    Three,
    Five,
}

impl Delay {
    pub fn secs(self) -> u32 {
        match self {
            Delay::Off => 0,
            Delay::Three => 3,
            Delay::Five => 5,
        }
    }

    /// 0 → 3 → 5 → 0.
    pub fn next(self) -> Self {
        match self {
            Delay::Off => Delay::Three,
            Delay::Three => Delay::Five,
            Delay::Five => Delay::Off,
        }
    }

    /// `delay 3s`.
    pub fn label(self) -> String {
        format!("delay {}s", self.secs())
    }
}

/// Shot or Record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Shot,
    Record,
}

pub fn mode_index(m: CaptureMode) -> usize {
    match m {
        CaptureMode::Region => 0,
        CaptureMode::Window => 1,
        CaptureMode::Output => 2,
    }
}

pub fn mode_from_index(i: usize) -> CaptureMode {
    match i {
        0 => CaptureMode::Region,
        1 => CaptureMode::Window,
        _ => CaptureMode::Output,
    }
}

/// `760 × 480 · 520, 200`.
pub fn size_label(r: Rect) -> String {
    format!("{} × {} · {}, {}", r.w.round() as i64, r.h.round() as i64, r.x.round() as i64, r.y.round() as i64)
}

/// `00:14`; past an hour `1:02:03`.
pub fn clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// Where the size label goes: 8 px above the selection (the chip is 22 high), or inside
/// its top edge when there is no room above.
pub fn label_pos(sel: Rect) -> Pt {
    const CHIP_H: f32 = 22.0;
    const GAP: f32 = 8.0;
    let above = sel.y - CHIP_H - GAP;
    if above >= 0.0 { Pt::new(sel.x, above) } else { Pt::new(sel.x + GAP, sel.y + GAP) }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pretty_assertions::assert_eq;
    use ashd_niri::niri_ipc::{Timestamp, WindowLayout};

    use super::*;

    const B: Dim = Dim::new(1920.0, 1080.0);

    fn pt(x: f32, y: f32) -> Pt {
        Pt::new(x, y)
    }

    #[test]
    fn create_from_any_direction_and_clamped() {
        let d = Drag::start(None, pt(100.0, 100.0));
        assert_eq!(d, Drag::Create { anchor: pt(100.0, 100.0) });
        assert_eq!(d.apply(pt(300.0, 250.0), B), Rect::new(100.0, 100.0, 200.0, 150.0));
        // up and to the left of the anchor: same rectangle, normalised
        assert_eq!(d.apply(pt(40.0, 20.0), B), Rect::new(40.0, 20.0, 60.0, 80.0));
        // the pointer leaves the output: clamped to the edge
        assert_eq!(d.apply(pt(5000.0, -30.0), B), Rect::new(100.0, 0.0, 1820.0, 100.0));
        // fractional pointer positions give whole px
        assert_eq!(d.apply(pt(200.4, 200.6), B), Rect::new(100.0, 100.0, 100.0, 101.0));
    }

    #[test]
    fn create_anchor_at_origin_is_kept() {
        let d = Drag::start(None, pt(0.0, 0.0));
        assert_eq!(d, Drag::Create { anchor: pt(0.0, 0.0) });
        assert_eq!(d.apply(pt(10.0, 10.0), B), Rect::new(0.0, 0.0, 10.0, 10.0));
    }

    #[test]
    fn a_plain_click_is_not_a_selection() {
        let d = Drag::start(None, pt(50.0, 50.0));
        assert!(!d.apply(pt(50.0, 50.0), B).is_usable());
        assert!(!d.apply(pt(51.0, 51.0), B).is_usable());
        assert!(d.apply(pt(60.0, 60.0), B).is_usable());
    }

    #[test]
    fn move_keeps_size_and_stays_inside() {
        let sel = Rect::new(520.0, 200.0, 760.0, 480.0);
        let d = Drag::start(Some(sel), pt(700.0, 400.0));
        assert!(matches!(d, Drag::Move { .. }));
        assert_eq!(d.apply(pt(710.0, 380.0), B), Rect::new(530.0, 180.0, 760.0, 480.0));
        // dragged far beyond every edge: pinned, size unchanged
        assert_eq!(d.apply(pt(-9000.0, -9000.0), B), Rect::new(0.0, 0.0, 760.0, 480.0));
        assert_eq!(d.apply(pt(9000.0, 9000.0), B), Rect::new(1160.0, 600.0, 760.0, 480.0));
    }

    #[test]
    fn resize_from_each_handle() {
        let sel = Rect::new(100.0, 100.0, 200.0, 100.0);
        let grab = |h: Handle| Drag::Resize { handle: h, grab: h.point(&sel), origin: sel };
        let to = |h: Handle, dx: f32, dy: f32| {
            let p = h.point(&sel);
            grab(h).apply(pt(p.x + dx, p.y + dy), B)
        };
        assert_eq!(to(Handle::E, 30.0, 99.0), Rect::new(100.0, 100.0, 230.0, 100.0), "E ignores dy");
        assert_eq!(to(Handle::W, -20.0, 99.0), Rect::new(80.0, 100.0, 220.0, 100.0));
        assert_eq!(to(Handle::N, 99.0, -10.0), Rect::new(100.0, 90.0, 200.0, 110.0));
        assert_eq!(to(Handle::S, 99.0, 40.0), Rect::new(100.0, 100.0, 200.0, 140.0));
        assert_eq!(to(Handle::NW, -10.0, -10.0), Rect::new(90.0, 90.0, 210.0, 110.0));
        assert_eq!(to(Handle::NE, 10.0, -10.0), Rect::new(100.0, 90.0, 210.0, 110.0));
        assert_eq!(to(Handle::SE, 10.0, 10.0), Rect::new(100.0, 100.0, 210.0, 110.0));
        assert_eq!(to(Handle::SW, -10.0, 10.0), Rect::new(90.0, 100.0, 210.0, 110.0));
        // shrinking moves the edge inwards
        assert_eq!(to(Handle::E, -50.0, 0.0), Rect::new(100.0, 100.0, 150.0, 100.0));
    }

    #[test]
    fn resize_flips_past_the_opposite_edge_and_clamps() {
        let sel = Rect::new(100.0, 100.0, 200.0, 100.0);
        let e = Drag::Resize { handle: Handle::E, grab: pt(300.0, 150.0), origin: sel };
        assert_eq!(e.apply(pt(50.0, 150.0), B), Rect::new(50.0, 100.0, 50.0, 100.0), "dragged past the left edge");
        let se = Drag::Resize { handle: Handle::SE, grab: pt(300.0, 200.0), origin: sel };
        assert_eq!(se.apply(pt(9999.0, 9999.0), B), Rect::new(100.0, 100.0, 1820.0, 980.0));
        let nw = Drag::Resize { handle: Handle::NW, grab: pt(100.0, 100.0), origin: sel };
        assert_eq!(nw.apply(pt(-500.0, -500.0), B), Rect::new(0.0, 0.0, 300.0, 200.0));
    }

    #[test]
    fn grabbing_does_not_jump() {
        // grabbing the east edge 3 px off the exact line does not move it on the first event
        let sel = Rect::new(100.0, 100.0, 200.0, 100.0);
        let d = Drag::start(Some(sel), pt(303.0, 150.0));
        assert!(matches!(d, Drag::Resize { handle: Handle::E, .. }));
        assert_eq!(d.apply(pt(303.0, 150.0), B), sel);
    }

    #[test]
    fn hit_testing() {
        let sel = Rect::new(100.0, 100.0, 200.0, 100.0);
        assert_eq!(hit_test(&sel, pt(100.0, 100.0)), Some(Grab::Handle(Handle::NW)));
        assert_eq!(hit_test(&sel, pt(305.0, 205.0)), Some(Grab::Handle(Handle::SE)));
        assert_eq!(hit_test(&sel, pt(100.0, 150.0)), Some(Grab::Handle(Handle::W)));
        assert_eq!(hit_test(&sel, pt(300.0, 150.0)), Some(Grab::Handle(Handle::E)));
        assert_eq!(hit_test(&sel, pt(200.0, 98.0)), Some(Grab::Handle(Handle::N)));
        assert_eq!(hit_test(&sel, pt(200.0, 202.0)), Some(Grab::Handle(Handle::S)));
        assert_eq!(hit_test(&sel, pt(200.0, 150.0)), Some(Grab::Body));
        assert_eq!(hit_test(&sel, pt(50.0, 50.0)), None);
        assert_eq!(hit_test(&sel, pt(200.0, 215.0)), None);
        assert_eq!(Handle::ALL.len(), 8);
        for h in Handle::ALL {
            let p = h.point(&sel);
            assert_eq!(hit_test(&sel, p), Some(Grab::Handle(h)), "{h:?}");
        }
    }

    #[test]
    fn nudging() {
        let sel = Rect::new(100.0, 100.0, 200.0, 100.0);
        assert_eq!(nudge(sel, Dir::Right, 1.0, false, B), Rect::new(101.0, 100.0, 200.0, 100.0));
        assert_eq!(nudge(sel, Dir::Up, nudge_step(true), false, B), Rect::new(100.0, 90.0, 200.0, 100.0));
        assert_eq!(nudge(sel, Dir::Down, 10.0, true, B), Rect::new(100.0, 100.0, 200.0, 110.0));
        assert_eq!(nudge(sel, Dir::Left, 1.0, true, B), Rect::new(100.0, 100.0, 199.0, 100.0));
        // walls
        let edge = Rect::new(0.0, 0.0, 50.0, 50.0);
        assert_eq!(nudge(edge, Dir::Left, 10.0, false, B), edge);
        assert_eq!(nudge(edge, Dir::Up, 10.0, false, B), edge);
        let corner = Rect::new(1870.0, 1030.0, 50.0, 50.0);
        assert_eq!(nudge(corner, Dir::Right, 10.0, false, B), corner);
        assert_eq!(nudge(corner, Dir::Down, 10.0, true, B), corner, "cannot grow past the output");
        // never below 1 px
        assert_eq!(nudge(Rect::new(10.0, 10.0, 1.0, 1.0), Dir::Left, 10.0, true, B).w, 1.0);
        assert_eq!(default_selection(B), Rect::new(480.0, 270.0, 960.0, 540.0));
    }

    #[test]
    fn pixels_at_integer_and_fractional_scale() {
        let one = ScaleMap { logical: B, image: (1920, 1080) };
        assert_eq!(one.to_pixels(Rect::new(520.0, 200.0, 760.0, 480.0)), PixelRect { x: 520, y: 200, w: 760, h: 480 });
        let two = ScaleMap { logical: Dim::new(1920.0, 1080.0), image: (3840, 2160) };
        assert_eq!(two.to_pixels(Rect::new(10.0, 20.0, 30.0, 40.0)), PixelRect { x: 20, y: 40, w: 60, h: 80 });
        // 1.25: 1536x864 logical, 1920x1080 px
        let frac = ScaleMap { logical: Dim::new(1536.0, 864.0), image: (1920, 1080) };
        assert_eq!(frac.factors(), (1.25, 1.25));
        assert_eq!(frac.to_pixels(Rect::new(3.0, 3.0, 10.0, 10.0)), PixelRect { x: 4, y: 4, w: 12, h: 12 });
        assert_eq!(frac.to_pixels(Rect::full(frac.logical)), PixelRect { x: 0, y: 0, w: 1920, h: 1080 });
        // 1.5 with a rounded logical size (1280x720 logical for 1920x1080 is exact; 1366x768
        // for 2049x1152 is not): the measured factor still maps the full rectangle to the image
        let odd = ScaleMap { logical: Dim::new(1366.0, 768.0), image: (2049, 1152) };
        assert_eq!(odd.to_pixels(Rect::full(odd.logical)), PixelRect { x: 0, y: 0, w: 2049, h: 1152 });
        // edges rounded, so two neighbouring rectangles leave no gap
        let a = frac.to_pixels(Rect::new(0.0, 0.0, 7.0, 5.0));
        let b = frac.to_pixels(Rect::new(7.0, 0.0, 7.0, 5.0));
        assert_eq!(a.x + a.w, b.x);
        // clamped and never empty
        assert_eq!(one.to_pixels(Rect::new(1919.9, 1079.9, 50.0, 50.0)), PixelRect { x: 1919, y: 1079, w: 1, h: 1 });
        assert_eq!(one.to_pixels(Rect::new(5.0, 5.0, 0.0, 0.0)), PixelRect { x: 5, y: 5, w: 1, h: 1 });
    }

    fn window(id: u64, ws: u64, pos: Option<(f64, f64)>, size: (f64, f64), floating: bool, focus: Option<u64>) -> Window {
        Window {
            id,
            title: Some(format!("w{id}")),
            app_id: Some("app".into()),
            pid: None,
            workspace_id: Some(ws),
            is_focused: false,
            is_floating: floating,
            is_urgent: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: None,
                tile_size: size,
                window_size: (size.0 as i32, size.1 as i32),
                tile_pos_in_workspace_view: pos,
                window_offset_in_tile: (0.0, 0.0),
                pos_in_canvas: None,
                is_pinned: false,
            },
            focus_timestamp: focus.map(|secs| Timestamp { secs, nanos: 0 }),
            hidden_from_workspace_id: None,
        }
    }

    #[test]
    fn windows_are_ordered_and_filtered() {
        let ws = [
            window(1, 7, Some((76.0, 12.0)), (740.0, 1010.0), false, Some(5)),
            window(2, 7, Some((828.0, 12.0)), (740.0, 1010.0), false, Some(9)),
            window(3, 7, Some((400.0, 300.0)), (500.0, 300.0), true, Some(1)),
            window(4, 7, None, (740.0, 1010.0), false, None), // scrolled out of view
            window(5, 8, Some((76.0, 12.0)), (740.0, 1010.0), false, None), // other workspace
            window(6, 7, Some((1800.0, 900.0)), (500.0, 500.0), true, Some(3)), // hangs off the output
        ];
        let v = windows_in_view(&ws, 7, B);
        assert_eq!(v.iter().map(|w| w.id).collect::<Vec<_>>(), vec![6, 3, 2, 1]);
        assert_eq!(v[0].rect, Rect::new(1800.0, 900.0, 120.0, 180.0), "clipped to the output");
        // topmost wins where windows overlap
        assert_eq!(window_at(&v, pt(450.0, 400.0)).map(|w| w.id), Some(3));
        assert_eq!(window_at(&v, pt(100.0, 40.0)).map(|w| w.id), Some(1));
        assert_eq!(window_at(&v, pt(1.0, 1.0)), None);
        assert_eq!(v[3].title, "w1");
    }

    #[test]
    fn canvas_zoom_scales_the_tiles() {
        let mut a = window(1, 7, Some((100.0, 100.0)), (400.0, 300.0), true, None);
        let mut b = window(2, 7, Some((300.0, 100.0)), (400.0, 300.0), true, None);
        // plane positions 0 and 400 apart, seen 200 apart: zoom 0.5
        a.layout.pos_in_canvas = Some((0.0, 0.0));
        b.layout.pos_in_canvas = Some((400.0, 0.0));
        assert_eq!(canvas_zoom(&[&a, &b]), 0.5);
        let v = windows_in_view([&a, &b], 7, B);
        assert_eq!(v.iter().find(|w| w.id == 1).unwrap().rect, Rect::new(100.0, 100.0, 200.0, 150.0));
        // one window, or none on the plane: 1.0
        assert_eq!(canvas_zoom(&[&a]), 1.0);
        let plain = window(3, 7, Some((0.0, 0.0)), (10.0, 10.0), false, None);
        assert_eq!(canvas_zoom(&[&plain, &plain]), 1.0);
    }

    #[test]
    fn recording_targets() {
        let out = OutputGeom { name: "DP-1".into(), x: 1920, y: 0, w: 1920, h: 1080, scale: 1.0 };
        assert_eq!(rec_target(&out, Rect::full(out.bounds())), RecTarget::Output("DP-1".into()));
        assert_eq!(
            rec_target(&out, Rect::new(10.0, 20.0, 801.0, 601.0)),
            RecTarget::Region { output: "DP-1".into(), rect: GlobalRect { x: 1930, y: 20, w: 800, h: 600 } },
            "global coordinates, even size"
        );
        // beyond the output: clamped first
        assert_eq!(
            rec_target(&out, Rect::new(-10.0, -10.0, 9999.0, 9999.0)),
            RecTarget::Output("DP-1".into())
        );
    }

    #[test]
    fn recorder_command_lines() {
        let file = Path::new("/home/u/Videos/Recordings/Recording 2026-11-14 15-04-05.mp4");
        let out = RecTarget::Output("DP-1".into());
        let region = RecTarget::Region { output: "DP-1".into(), rect: GlobalRect { x: 520, y: 200, w: 760, h: 480 } };
        let f = file.to_str().unwrap().to_string();
        assert_eq!(RecorderKind::WfRecorder.args(&out, file), vec!["-D", "-r", "60", "-o", "DP-1", "-f", &f]);
        assert_eq!(
            RecorderKind::WfRecorder.args(&region, file),
            vec!["-D", "-r", "60", "-g", "520,200 760x480", "-f", &f]
        );
        assert_eq!(RecorderKind::GpuScreenRecorder.args(&out, file), vec!["-w", "DP-1", "-f", "60", "-o", &f]);
        assert_eq!(
            RecorderKind::GpuScreenRecorder.args(&region, file),
            vec!["-w", "region", "-region", "760x480+520+200", "-f", "60", "-o", &f]
        );
        assert_eq!(RecorderKind::PREFERRED[0].binary(), "wf-recorder");
        assert!(RECORDER_ENV_REMOVE.contains(&"LD_LIBRARY_PATH"));
        assert!(!RECORDER_ENV_REMOVE.contains(&"WAYLAND_DISPLAY"), "the shell's own display is passed on");
    }

    #[test]
    fn file_names() {
        let t = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap().and_hms_opt(15, 4, 5).unwrap();
        assert_eq!(screenshot_file_name(t), "Screenshot from 2026-11-14 15-04-05.png");
        assert_eq!(recording_file_name(t), "Recording 2026-11-14 15-04-05.mp4");
        let dir = Path::new("/p");
        let name = screenshot_file_name(t);
        assert_eq!(unique_path(dir, &name, |_| false), dir.join(&name));
        let taken = [dir.join(&name), dir.join("Screenshot from 2026-11-14 15-04-05 (2).png")];
        assert_eq!(
            unique_path(dir, &name, |p| taken.iter().any(|t| t == p)),
            dir.join("Screenshot from 2026-11-14 15-04-05 (3).png")
        );
    }

    #[test]
    fn labels_and_cycles() {
        assert_eq!(size_label(Rect::new(520.0, 200.0, 760.0, 480.0)), "760 × 480 · 520, 200");
        assert_eq!(clock(14), "00:14");
        assert_eq!(clock(754), "12:34");
        assert_eq!(clock(3723), "1:02:03");
        let d = Delay::default();
        assert_eq!((d.secs(), d.label().as_str()), (0, "delay 0s"));
        assert_eq!(d.next().secs(), 3);
        assert_eq!(d.next().next().secs(), 5);
        assert_eq!(d.next().next().next(), Delay::Off);
        for m in [CaptureMode::Region, CaptureMode::Window, CaptureMode::Output] {
            assert_eq!(mode_from_index(mode_index(m)), m);
        }
        assert_eq!(label_pos(Rect::new(520.0, 200.0, 760.0, 480.0)), pt(520.0, 170.0));
        assert_eq!(label_pos(Rect::new(0.0, 10.0, 50.0, 50.0)), pt(8.0, 18.0), "no room above: inside");
    }
}
