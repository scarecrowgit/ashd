//! A stand-in for the compositor: used by the nested session when it cannot do gamma.
//!
//! niri only offers `zwlr_gamma_control_manager_v1` on its DRM backend; the nested niri
//! (winit) does not, so the service would just report "unavailable" there and the UI could
//! not be looked at. With `ASHD_NESTED=1` *and* `ASHD_FAKE_NIGHTLIGHT=1` the service runs
//! against this: one pretend output that logs every table it is given (target
//! `ashd_svc_nightlight::fake`, level debug). Nothing here ever talks to a compositor.

use crate::{Gamma, OutputStatus};

pub(crate) struct LogGamma {
    outputs: Vec<OutputStatus>,
}

impl LogGamma {
    pub(crate) fn new() -> Self {
        Self { outputs: vec![OutputStatus { name: "NESTED-1".into(), available: true }] }
    }
}

impl Gamma for LogGamma {
    fn apply(&mut self, rgb: [f32; 3]) {
        tracing::debug!(target: "ashd_svc_nightlight::fake", "gamma multipliers r {:.3} g {:.3} b {:.3}", rgb[0], rgb[1], rgb[2]);
    }
    fn outputs(&self) -> Vec<OutputStatus> {
        self.outputs.clone()
    }
    fn retry_failed(&mut self) -> bool {
        false
    }
    fn has_failed(&self) -> bool {
        false
    }
}
