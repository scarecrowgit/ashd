//! `ashd canvas toggle|zoom-in|zoom-out|fit|home` without a running shell: straight to
//! niri (the shell's canvas HUD sends the same actions when it runs). All act on the
//! focused workspace; the zoom/fit/home ones are no-ops on strip workspaces, and on a
//! stock niri (no canvas mode) niri rejects the unknown action with an error.
//!
//! Owner: canvas UI.

use anyhow::Context as _;
use ashd_ipc::CanvasOp;
use ashd_niri::{blocking, niri_ipc::Action};

/// The niri action for `op` (the same mapping as `ashd_panel_canvas_hud::canvas_action`).
pub fn action(op: CanvasOp) -> Action {
    match op {
        CanvasOp::Toggle => Action::ToggleWorkspaceMode {},
        CanvasOp::ZoomIn => Action::CanvasZoomIn { at_cursor: false },
        CanvasOp::ZoomOut => Action::CanvasZoomOut { at_cursor: false },
        CanvasOp::Fit => Action::CanvasFit {},
        CanvasOp::Home => Action::CanvasHome {},
    }
}

pub fn run(op: CanvasOp) -> anyhow::Result<()> {
    blocking::action(action(op)).context("ashd-shell is not running and niri did not take the canvas action")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_has_its_niri_action() {
        assert!(matches!(action(CanvasOp::Toggle), Action::ToggleWorkspaceMode {}));
        assert!(matches!(action(CanvasOp::ZoomIn), Action::CanvasZoomIn { at_cursor: false }));
        assert!(matches!(action(CanvasOp::ZoomOut), Action::CanvasZoomOut { at_cursor: false }));
        assert!(matches!(action(CanvasOp::Fit), Action::CanvasFit {}));
        assert!(matches!(action(CanvasOp::Home), Action::CanvasHome {}));
    }
}
