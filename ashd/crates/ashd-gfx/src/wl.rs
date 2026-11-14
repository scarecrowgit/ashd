//! HiDPI for sctk clients: `wp_fractional_scale_v1` + `wp_viewporter` when the
//! compositor has both (niri does), else the integer `wl_surface` preferred buffer scale.
//!
//! Usage (state type `D` uses `smithay_client_toolkit::delegate_dispatch2!`):
//! ```ignore
//! let scales = ScaleGlobals::bind(&globals, &qh);
//! let mut s = scales.attach(&wl_surface, &qh);   // per surface
//! // FractionalScaleHandler::preferred_scale → s.set_fractional(v120)
//! // CompositorHandler::scale_factor_changed → s.set_integer(factor)
//! s.apply(&wl_surface, logical_w, logical_h);    // before presenting a buffer of s.scale()
//! ```

use smithay_client_toolkit::dispatch2::Dispatch2;
use wayland_client::{Connection, Dispatch, QueueHandle, globals::GlobalList, protocol::wl_surface::WlSurface};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
    viewporter::client::{wp_viewport, wp_viewporter},
};

/// Called with the compositor's preferred scale for a surface, in 1/120 units.
pub trait FractionalScaleHandler: Sized {
    fn preferred_fractional_scale(&mut self, surface: &WlSurface, scale_120: u32);
}

/// User data for globals and objects without events.
#[derive(Debug, Default)]
pub struct NoEvents;

impl<D> Dispatch2<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, D> for NoEvents {
    fn event(
        &self,
        _: &mut D,
        _: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        _: wp_fractional_scale_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
    }
}

impl<D> Dispatch2<wp_viewporter::WpViewporter, D> for NoEvents {
    fn event(&self, _: &mut D, _: &wp_viewporter::WpViewporter, _: wp_viewporter::Event, _: &Connection, _: &QueueHandle<D>) {}
}

impl<D> Dispatch2<wp_viewport::WpViewport, D> for NoEvents {
    fn event(&self, _: &mut D, _: &wp_viewport::WpViewport, _: wp_viewport::Event, _: &Connection, _: &QueueHandle<D>) {}
}

/// User data of a `wp_fractional_scale_v1`: the surface it belongs to.
#[derive(Debug)]
pub struct FractionalScaleData {
    pub surface: WlSurface,
}

impl<D: FractionalScaleHandler> Dispatch2<wp_fractional_scale_v1::WpFractionalScaleV1, D> for FractionalScaleData {
    fn event(
        &self,
        state: &mut D,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.preferred_fractional_scale(&self.surface, scale);
        }
    }
}

/// The two optional globals.
#[derive(Debug, Default)]
pub struct ScaleGlobals {
    pub fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    pub viewporter: Option<wp_viewporter::WpViewporter>,
}

impl ScaleGlobals {
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Self
    where
        D: Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, NoEvents>
            + Dispatch<wp_viewporter::WpViewporter, NoEvents>
            + 'static,
    {
        let fractional = globals.bind(qh, 1..=1, NoEvents).ok();
        let viewporter = globals.bind(qh, 1..=1, NoEvents).ok();
        // Fractional scaling needs both: the buffer is physical-size, the viewport maps it back.
        if fractional.is_some() && viewporter.is_some() {
            Self { fractional, viewporter }
        } else {
            Self { fractional: None, viewporter }
        }
    }

    /// Per-surface scale state.
    pub fn attach<D>(&self, surface: &WlSurface, qh: &QueueHandle<D>) -> SurfaceScale
    where
        D: Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, FractionalScaleData>
            + Dispatch<wp_viewport::WpViewport, NoEvents>
            + 'static,
    {
        let fractional = self
            .fractional
            .as_ref()
            .map(|m| m.get_fractional_scale(surface, qh, FractionalScaleData { surface: surface.clone() }));
        let viewport = fractional.as_ref().and(self.viewporter.as_ref()).map(|v| v.get_viewport(surface, qh, NoEvents));
        SurfaceScale { fractional, viewport, integer: 1, scale_120: None, applied: None }
    }
}

/// Scale of one surface.
#[derive(Debug)]
pub struct SurfaceScale {
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    integer: i32,
    scale_120: Option<u32>,
    applied: Option<(i32, i32, i32)>,
}

impl SurfaceScale {
    /// Current logical → physical factor.
    pub fn scale(&self) -> f64 {
        match (self.viewport.is_some(), self.scale_120) {
            (true, Some(v)) => f64::from(v) / 120.0,
            _ => f64::from(self.integer.max(1)),
        }
    }

    pub fn is_fractional(&self) -> bool {
        self.viewport.is_some()
    }

    /// Returns whether the scale changed.
    pub fn set_fractional(&mut self, scale_120: u32) -> bool {
        let old = self.scale();
        self.scale_120 = Some(scale_120.max(1));
        old != self.scale()
    }

    /// Returns whether the scale changed.
    pub fn set_integer(&mut self, factor: i32) -> bool {
        let old = self.scale();
        self.integer = factor.max(1);
        old != self.scale()
    }

    /// Tell the compositor how the next buffer maps to `logical_w × logical_h`. Call
    /// before presenting a buffer of `physical_size(logical, self.scale())`.
    pub fn apply(&mut self, surface: &WlSurface, logical_w: u32, logical_h: u32) {
        let (w, h) = (logical_w.max(1) as i32, logical_h.max(1) as i32);
        if let Some(vp) = &self.viewport {
            if self.applied != Some((w, h, 0)) {
                surface.set_buffer_scale(1);
                vp.set_destination(w, h);
                self.applied = Some((w, h, 0));
            }
        } else {
            let s = self.integer.max(1);
            if self.applied != Some((w, h, s)) {
                surface.set_buffer_scale(s);
                self.applied = Some((w, h, s));
            }
        }
    }
}

impl Drop for SurfaceScale {
    fn drop(&mut self) {
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_scale_without_viewport() {
        let mut s = SurfaceScale { fractional: None, viewport: None, integer: 1, scale_120: None, applied: None };
        assert_eq!(s.scale(), 1.0);
        assert!(s.set_integer(2));
        assert_eq!(s.scale(), 2.0);
        // fractional values are ignored without a viewport
        assert!(!s.set_fractional(150));
        assert_eq!(s.scale(), 2.0);
    }
}
