//! Scrolling-strip columns of one workspace, derived from window layouts.
//!
//! niri reports, per tiled window, `pos_in_scrolling_layout = (column, tile)` (1-based)
//! and `tile_size`. A column's width is the widest tile in it (tiles in a column share
//! the width; the max guards against transient mismatches during resize animations).
//! `x` is the column's left edge on the strip *without gaps*; add `(index - 1) * gap`
//! for niri's layout gaps. `view_x` is the on-screen position niri reports for the
//! column's first tile when it is in the workspace view.

use niri_ipc::Window;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct StripColumn {
    /// 1-based, as in `Action::FocusColumn`.
    pub index: usize,
    /// Window ids top to bottom.
    pub windows: Vec<u64>,
    pub width: f64,
    /// Left edge on the strip, sum of the widths of the columns before it (no gaps).
    pub x: f64,
    /// Left edge within the current workspace view, if niri reported it.
    pub view_x: Option<f64>,
    /// The column holds the focused window.
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Strip {
    pub columns: Vec<StripColumn>,
    /// Floating windows (not part of the strip), by id.
    pub floating: Vec<u64>,
}

impl Strip {
    /// Total strip width with `gap` between columns.
    pub fn total_width(&self, gap: f64) -> f64 {
        let n = self.columns.len();
        self.columns.iter().map(|c| c.width).sum::<f64>() + gap * n.saturating_sub(1) as f64
    }

    pub fn focused_column(&self) -> Option<&StripColumn> {
        self.columns.iter().find(|c| c.focused)
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The column with niri's 1-based `index` (`Action::FocusColumn`).
    pub fn column(&self, index: usize) -> Option<&StripColumn> {
        self.columns.iter().find(|c| c.index == index)
    }

    /// The column holding window `id`.
    pub fn column_of(&self, id: u64) -> Option<&StripColumn> {
        self.columns.iter().find(|c| c.windows.contains(&id))
    }

    /// Left edge of every column on the strip with `gap` between columns.
    pub fn lefts(&self, gap: f64) -> Vec<f64> {
        self.columns.iter().enumerate().map(|(i, c)| c.x + gap * i as f64).collect()
    }
}

/// Build the strip from the windows of one workspace (any order).
pub fn strip<'a>(windows: impl IntoIterator<Item = &'a Window>) -> Strip {
    strip_with_active(windows, None)
}

/// Like [`strip`]; when no window of the workspace is focused (another output has the
/// keyboard focus) the column holding `active_window` is marked focused.
pub fn strip_with_active<'a>(windows: impl IntoIterator<Item = &'a Window>, active_window: Option<u64>) -> Strip {
    let mut tiled: Vec<(&Window, (usize, usize))> = Vec::new();
    let mut floating = Vec::new();
    for w in windows {
        match w.layout.pos_in_scrolling_layout {
            Some(pos) if !w.is_floating => tiled.push((w, pos)),
            _ => floating.push(w.id),
        }
    }
    floating.sort_unstable();
    tiled.sort_by_key(|(w, pos)| (*pos, w.id));

    let mut columns: Vec<StripColumn> = Vec::new();
    for (w, (col, _tile)) in tiled {
        if columns.last().map(|c| c.index) != Some(col) {
            columns.push(StripColumn { index: col, ..Default::default() });
        }
        let c = columns.last_mut().expect("pushed above");
        if c.view_x.is_none() {
            c.view_x = w.layout.tile_pos_in_workspace_view.map(|p| p.0);
        }
        c.windows.push(w.id);
        c.width = c.width.max(w.layout.tile_size.0);
        c.focused |= w.is_focused;
    }
    if !columns.iter().any(|c| c.focused)
        && let Some(id) = active_window
        && let Some(c) = columns.iter_mut().find(|c| c.windows.contains(&id))
    {
        c.focused = true;
    }
    let mut x = 0.0;
    for c in &mut columns {
        c.x = x;
        x += c.width;
    }
    Strip { columns, floating }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::win;

    #[test]
    fn columns_from_layouts() {
        // Ash reference widths (desk-05): 442 624 927 624 442; column 3 has two tiles.
        let ws = [
            win(1, 1, Some((1, 1)), 442.0, false),
            win(2, 1, Some((2, 1)), 624.0, false),
            win(3, 1, Some((3, 1)), 927.0, true),
            win(4, 1, Some((3, 2)), 927.0, false),
            win(5, 1, Some((4, 1)), 624.0, false),
            win(6, 1, Some((5, 1)), 442.0, false),
            win(7, 1, None, 300.0, false),
        ];
        let s = strip(ws.iter().rev());
        assert_eq!(s.columns.iter().map(|c| c.width).collect::<Vec<_>>(), vec![442.0, 624.0, 927.0, 624.0, 442.0]);
        assert_eq!(s.columns.iter().map(|c| c.x).collect::<Vec<_>>(), vec![0.0, 442.0, 1066.0, 1993.0, 2617.0]);
        assert_eq!(s.columns[2].windows, vec![3, 4]);
        assert_eq!(s.focused_column().map(|c| c.index), Some(3));
        assert_eq!(s.floating, vec![7]);
        // desk-05 script: L = [0,454,1090,2029,2665] with gap 12.
        let lefts: Vec<f64> = s.columns.iter().map(|c| c.x + (c.index - 1) as f64 * 12.0).collect();
        assert_eq!(lefts, vec![0.0, 454.0, 1090.0, 2029.0, 2665.0]);
        assert_eq!(s.total_width(12.0), 3107.0);
    }

    #[test]
    fn helpers_and_edge_cases() {
        let mut a = win(1, 1, Some((1, 1)), 400.0, false);
        let mut b = win(2, 1, Some((2, 1)), 600.0, false);
        let mut c = win(3, 1, Some((2, 2)), 580.0, false); // narrower tile: column width is the widest
        a.layout.tile_pos_in_workspace_view = Some((12.0, 12.0));
        b.layout.tile_pos_in_workspace_view = None;
        c.layout.tile_pos_in_workspace_view = Some((424.0, 500.0)); // first Some of the column wins
        let ws = [c.clone(), a.clone(), b.clone()];
        let s = strip(ws.iter());
        assert_eq!(s.len(), 2);
        assert_eq!(s.column(2).map(|c| (c.width, c.view_x)), Some((600.0, Some(424.0))));
        assert_eq!(s.column(1).and_then(|c| c.view_x), Some(12.0));
        assert!(s.column(3).is_none());
        assert_eq!(s.column_of(3).map(|c| c.index), Some(2));
        assert!(s.column_of(99).is_none());
        assert_eq!(s.lefts(12.0), vec![0.0, 412.0]);
        assert_eq!(s.total_width(0.0), 1000.0);
        assert!(s.focused_column().is_none());

        // The active window marks a column only when nothing is focused.
        let s = strip_with_active(ws.iter(), Some(3));
        assert_eq!(s.focused_column().map(|c| c.index), Some(2));
        let mut focused = a.clone();
        focused.is_focused = true;
        let ws2 = [focused, b, c];
        let s = strip_with_active(ws2.iter(), Some(3));
        assert_eq!(s.columns.iter().filter(|c| c.focused).count(), 1);
        assert_eq!(s.focused_column().map(|c| c.index), Some(1), "real focus beats the active window");
        // An active window that is not tiled here changes nothing.
        assert!(strip_with_active(ws.iter(), Some(42)).focused_column().is_none());

        // Empty input, floating-flagged window with a stale position.
        assert!(strip(std::iter::empty()).is_empty());
        let mut f = win(9, 1, Some((1, 1)), 100.0, false);
        f.is_floating = true;
        let s = strip([f].iter());
        assert!(s.columns.is_empty() && s.floating == vec![9]);
        assert_eq!(s.total_width(12.0), 0.0);
    }

    #[test]
    fn column_indices_may_have_holes() {
        // Windows of a column still opening are absent for a moment: keep niri's indices.
        let ws = [win(1, 1, Some((1, 1)), 300.0, false), win(2, 1, Some((3, 1)), 500.0, false)];
        let s = strip(ws.iter());
        assert_eq!(s.columns.iter().map(|c| c.index).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(s.columns[1].x, 300.0);
    }
}
