//! Canvas-mode helpers (ashd fork of niri): windows on the plane, plane bounds, and the
//! live view state published separately from [`NiriState`](crate::NiriState).
//!
//! Plane coordinates are unscaled logical px; a canvas view maps them to the workspace
//! view with `view = (plane - offset) * zoom`.

use std::collections::HashMap;

use niri_ipc::{CanvasRegion, CanvasViewState, StripViewState, Window};

/// Axis-aligned rectangle (plane or view units).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PlaneRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl PlaneRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Smallest rect containing both.
    pub fn union(&self, o: &PlaneRect) -> PlaneRect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        PlaneRect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }

    /// Grown by `pad` on every side.
    pub fn padded(&self, pad: f64) -> PlaneRect {
        PlaneRect { x: self.x - pad, y: self.y - pad, w: self.w + 2.0 * pad, h: self.h + 2.0 * pad }
    }

    /// The rects overlap (touching edges do not count).
    pub fn intersects(&self, o: &PlaneRect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub fn contains(&self, (px, py): (f64, f64)) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// A window on a canvas plane.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasWindow {
    pub id: u64,
    /// Tile rect on the plane (`pos_in_canvas`, `tile_size`).
    pub rect: PlaneRect,
    /// Holds the keyboard focus (or is the workspace's active window while another output
    /// has the focus).
    pub focused: bool,
    /// Pinned: keeps its screen position while the view pans.
    pub pinned: bool,
}

/// Plane windows among `windows` (any order), sorted by id. When none of them is focused
/// the one with id `active_window` is reported focused.
pub fn canvas_windows<'a>(windows: impl IntoIterator<Item = &'a Window>, active_window: Option<u64>) -> Vec<CanvasWindow> {
    let mut v: Vec<CanvasWindow> = windows
        .into_iter()
        .filter_map(|w| {
            let (x, y) = w.layout.pos_in_canvas?;
            let (tw, th) = w.layout.tile_size;
            Some(CanvasWindow {
                id: w.id,
                rect: PlaneRect::new(x, y, tw, th),
                focused: w.is_focused,
                pinned: w.layout.is_pinned,
            })
        })
        .collect();
    v.sort_by_key(|w| w.id);
    if !v.iter().any(|w| w.focused)
        && let Some(id) = active_window
        && let Some(w) = v.iter_mut().find(|w| w.id == id)
    {
        w.focused = true;
    }
    v
}

/// Union of the window rects and the regions; `None` when both are empty.
pub fn bounds(windows: &[CanvasWindow], regions: &[CanvasRegion]) -> Option<PlaneRect> {
    windows
        .iter()
        .map(|w| w.rect)
        .chain(regions.iter().map(|r| PlaneRect::new(r.x, r.y, r.w, r.h)))
        .reduce(|a, b| a.union(&b))
}

/// The plane rect a canvas view shows right now (`offset`, `view_size / zoom`).
pub fn visible_plane_rect(view: &CanvasViewState) -> PlaneRect {
    let z = if view.zoom > 0.0 { view.zoom } else { 1.0 };
    PlaneRect::new(view.offset.0, view.offset.1, view.view_size.0 / z, view.view_size.1 / z)
}

/// The plane rect the view settles at (`target_offset`, `target_zoom`).
pub fn target_plane_rect(view: &CanvasViewState) -> PlaneRect {
    let z = if view.target_zoom > 0.0 { view.target_zoom } else { 1.0 };
    PlaneRect::new(view.target_offset.0, view.target_offset.1, view.view_size.0 / z, view.view_size.1 / z)
}

/// The `canvas-pan-to` offset that centres plane point `p` in a view (keeping its zoom).
pub fn offset_centering(view: &CanvasViewState, p: (f64, f64)) -> (f64, f64) {
    let r = target_plane_rect(view);
    (p.0 - r.w / 2.0, p.1 - r.h / 2.0)
}

/// Live view state of every workspace: what moves every frame while a view animates.
///
/// Published by the ashd-niri thread on its own channel
/// ([`NiriHandle::subscribe_views`](crate::NiriHandle::subscribe_views)), at most once per
/// [`VIEWS_MIN_INTERVAL`](crate::VIEWS_MIN_INTERVAL), so that only the panels that draw the
/// view (strip header, canvas HUD, minimap) wake up during a pan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveViews {
    /// By workspace id: every canvas-mode workspace.
    pub canvas: HashMap<u64, CanvasViewState>,
    /// By workspace id: every strip-mode workspace.
    pub strip: HashMap<u64, StripViewState>,
}

impl LiveViews {
    pub fn canvas(&self, workspace_id: u64) -> Option<&CanvasViewState> {
        self.canvas.get(&workspace_id)
    }

    pub fn strip(&self, workspace_id: u64) -> Option<&StripViewState> {
        self.strip.get(&workspace_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::win;

    fn canvas_win(id: u64, x: f64, y: f64, w: f64, h: f64) -> Window {
        let mut win = win(id, 1, None, w, false);
        win.layout.tile_size = (w, h);
        win.layout.pos_in_canvas = Some((x, y));
        win
    }

    pub(crate) fn view(ws: u64, offset: (f64, f64), zoom: f64) -> CanvasViewState {
        CanvasViewState {
            workspace_id: ws,
            offset,
            zoom,
            target_offset: offset,
            target_zoom: zoom,
            view_size: (1920.0, 1080.0),
            animating: false,
            regions: vec![],
        }
    }

    #[test]
    fn windows_and_bounds() {
        let mut a = canvas_win(2, -100.0, 50.0, 800.0, 600.0);
        a.layout.is_pinned = true;
        let b = canvas_win(1, 1000.0, -200.0, 400.0, 300.0);
        let strip_only = win(3, 1, Some((1, 1)), 500.0, true); // fullscreen/strip: not on the plane
        let ws = [a, b, strip_only];
        let v = canvas_windows(ws.iter(), None);
        assert_eq!(v.iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(v[1].pinned && !v[0].pinned);
        assert!(v.iter().all(|w| !w.focused), "the focused window is not on the plane");
        let v2 = canvas_windows(ws.iter(), Some(1));
        assert!(v2[0].focused);
        let regions = [CanvasRegion { name: "notes".into(), x: -400.0, y: 0.0, w: 100.0, h: 2000.0 }];
        assert_eq!(bounds(&v, &[]), Some(PlaneRect::new(-100.0, -200.0, 1500.0, 850.0)));
        assert_eq!(bounds(&v, &regions), Some(PlaneRect::new(-400.0, -200.0, 1800.0, 2200.0)));
        assert_eq!(bounds(&[], &[]), None);
    }

    #[test]
    fn rect_math() {
        let r = PlaneRect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.intersects(&PlaneRect::new(5.0, 5.0, 10.0, 10.0)));
        assert!(!r.intersects(&PlaneRect::new(10.0, 0.0, 10.0, 10.0)), "touching is not overlapping");
        assert!(r.contains((0.0, 9.9)) && !r.contains((10.0, 0.0)));
        assert_eq!(r.padded(2.0), PlaneRect::new(-2.0, -2.0, 14.0, 14.0));
        assert_eq!(r.center(), (5.0, 5.0));
    }

    #[test]
    fn view_math() {
        let mut v = view(1, (100.0, 50.0), 0.5);
        assert_eq!(visible_plane_rect(&v), PlaneRect::new(100.0, 50.0, 3840.0, 2160.0));
        v.target_zoom = 1.0;
        v.target_offset = (0.0, 0.0);
        assert_eq!(target_plane_rect(&v), PlaneRect::new(0.0, 0.0, 1920.0, 1080.0));
        // Centring uses the settled zoom.
        assert_eq!(offset_centering(&v, (1000.0, 1000.0)), (40.0, 460.0));
        v.zoom = 0.0;
        assert_eq!(visible_plane_rect(&v).w, 1920.0, "a zero zoom does not divide by zero");
    }
}
