//! The throughput sparkline of the Ethernet section (det-12): 48 px high, a new sample every
//! 500 ms, the line scrolls left continuously between samples, download in accent (1.5 px)
//! over upload in muted (1.2 px). The vertical scale is latched once per view and never
//! changes while the section is open; bigger values are clipped at the top.

use gpui::{Bounds, ContentMask, Hsla, IntoElement, PathBuilder, Pixels, Styled as _, canvas, point, px};
use ashd_svc_network::Sample;

/// Height of the chart.
pub const HEIGHT: f32 = 48.0;
/// Samples that fit across the width (15 px apart at the mockup's 704 px, ~7.6 px here).
pub const VISIBLE: usize = 46;
/// Smallest scale (bytes/s): an idle link does not fill the chart with noise.
pub const SCALE_FLOOR: f32 = 125_000.0;
/// The scale is latched once this many samples (2 s) exist.
pub const LATCH_AFTER: usize = 4;

/// Round up to 1, 2 or 5 times a power of ten.
pub fn nice_ceil(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let p = 10f32.powf(v.log10().floor());
    let m = v / p;
    let n = if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    };
    n * p
}

/// The latched scale for a view, from the window max at that moment.
pub fn latch_scale(window_max: f32) -> f32 {
    nice_ceil(window_max.max(SCALE_FLOOR))
}

/// Latch policy: `None` until enough samples exist, then fixed for the rest of the view.
pub fn update_latch(latched: &mut Option<f32>, history_len: usize, window_max: f32) -> f32 {
    if latched.is_none() && history_len >= LATCH_AFTER {
        *latched = Some(latch_scale(window_max));
    }
    latched.unwrap_or(SCALE_FLOOR)
}

/// Polyline in chart coordinates (x from the left edge, y from the top). `frac` is the
/// fraction (0..=1) of the sample interval elapsed since the newest sample: the line has
/// moved `frac` of a step to the left, which makes the scroll continuous.
pub fn points(history: &[Sample], frac: f32, width: f32, height: f32, scale: f32, pick: impl Fn(&Sample) -> f32) -> Vec<(f32, f32)> {
    let n = history.len();
    let step = width / VISIBLE as f32;
    let frac = frac.clamp(0.0, 1.0);
    history
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let x = width - step * (n - 1 - i) as f32 - step * frac;
            let y = (height - 1.0) - (pick(s) / scale).clamp(0.0, 1.0) * (height - 3.0);
            (x, y)
        })
        .filter(|(x, _)| *x > -step)
        .collect()
}

fn stroke(points: &[(f32, f32)], origin: gpui::Point<Pixels>, width: f32, color: Hsla, window: &mut gpui::Window) {
    if points.len() < 2 {
        return;
    }
    let mut b = PathBuilder::stroke(px(width));
    for (i, (x, y)) in points.iter().enumerate() {
        let p = point(origin.x + px(*x), origin.y + px(*y));
        if i == 0 {
            b.move_to(p);
        } else {
            b.line_to(p);
        }
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, color);
    }
}

/// The chart element: fills its parent's width, [`HEIGHT`] high.
pub fn chart(history: Vec<Sample>, frac: f32, scale: f32, rx: Hsla, tx: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, (), window, _| {
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let down = points(&history, frac, w, h, scale, |s| s.rx_bps);
            let up = points(&history, frac, w, h, scale, |s| s.tx_bps);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                // Upload first so the download line sits on top.
                stroke(&up, bounds.origin, 1.2, tx, window);
                stroke(&down, bounds.origin, 1.5, rx, window);
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(rx: f32) -> Sample {
        Sample { rx_bps: rx, tx_bps: 0.0 }
    }

    #[test]
    fn nice_numbers() {
        assert_eq!(nice_ceil(0.7), 1.0);
        assert_eq!(nice_ceil(1.5), 2.0);
        assert_eq!(nice_ceil(3.0), 5.0);
        assert_eq!(nice_ceil(7.0), 10.0);
        assert_eq!(nice_ceil(84_200_000.0), 100_000_000.0);
        assert_eq!(nice_ceil(3_000_000.0), 5_000_000.0);
        assert_eq!(latch_scale(10.0), 200_000.0, "idle floor 125 kB/s rounds to a nice number");
    }

    #[test]
    fn scale_is_latched_once_and_never_changes() {
        let mut l = None;
        // Not enough samples yet: floor, nothing latched.
        assert_eq!(update_latch(&mut l, 2, 9e6), SCALE_FLOOR);
        assert!(l.is_none());
        let first = update_latch(&mut l, 4, 3_000_000.0);
        assert_eq!(first, 5_000_000.0);
        // A later burst does not rescale.
        assert_eq!(update_latch(&mut l, 30, 900_000_000.0), first);
        assert_eq!(update_latch(&mut l, 60, 0.0), first);
    }

    #[test]
    fn points_scroll_continuously_and_clip_at_the_top() {
        let h = vec![s(0.0), s(50.0), s(500.0)];
        let (w, ht) = (460.0, 48.0);
        let p0 = points(&h, 0.0, w, ht, 100.0, |x| x.rx_bps);
        assert_eq!(p0.len(), 3);
        // Newest sample at the right edge, 10 px apart.
        assert_eq!(p0[2].0, w);
        assert!((p0[1].0 - (w - 10.0)).abs() < 1e-3);
        // Bottom for zero, clipped to the top for values above the scale.
        assert_eq!(p0[0].1, ht - 1.0);
        assert_eq!(p0[2].1, 2.0);
        // Half a step later every point moved 5 px left.
        let p1 = points(&h, 0.5, w, ht, 100.0, |x| x.rx_bps);
        for (a, b) in p0.iter().zip(&p1) {
            assert!((a.0 - b.0 - 5.0).abs() < 1e-3);
            assert_eq!(a.1, b.1, "heights must not change while scrolling");
        }
        // The same data at a later frame keeps the same y: no rescale mid-view.
        let p2 = points(&h, 1.0, w, ht, 100.0, |x| x.rx_bps);
        assert_eq!(p2[1].1, p0[1].1);
    }

    #[test]
    fn points_left_of_the_chart_are_dropped() {
        let h: Vec<Sample> = (0..100).map(|i| s(i as f32)).collect();
        let p = points(&h, 0.0, 460.0, 48.0, 100.0, |x| x.rx_bps);
        assert!(p.len() <= VISIBLE + 2);
        assert!(p.iter().all(|(x, _)| *x > -10.0));
    }
}
