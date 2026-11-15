//! Canvas HUD (desk-05 canvas branch, `left 14px; top 12px`): a chip at the top-left of
//! the main area while the display's active workspace is a canvas — workspace label ·
//! "canvas" · "focus: <window>" · zoom percent, plus the real zoom/fit keys from
//! `keybinds.json` when they exist. Not interactive (no hit area: click-through).
//!
//! Also handles `Request::Canvas(..)` (`ashd canvas toggle|zoom-in|zoom-out|fit|home` and
//! the bar's mode switch) by sending the niri actions for the focused workspace.
//!
//! [`common`] holds what the minimap and strip header share with it (main-area geometry,
//! the shared live-view entity, key hints, hover tracking).
//!
//! Owner: canvas UI.

pub mod common;

use std::sync::Arc;

use gpui::{App, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, div, px};
use ashd_ipc::{CanvasOp, Request, Response};
use ashd_niri::{LiveViews, niri_ipc::Action};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement};
use ashd_ui::theme;

use common::{KeyBinds, ModeFade, SWAP_TRAVEL};

/// Chip offset inside the main area (desk-05 `left 14px; top 12px`).
pub const HUD_LEFT: f32 = 14.0;
pub const HUD_TOP: f32 = 12.0;
/// Chip padding / gap (desk-05 `padding: 6px 10px; gap: 12px`).
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 6.0;
const GAP: f32 = 12.0;

/// The niri action for a `ashd canvas …` op (all act on the focused workspace).
pub fn canvas_action(op: CanvasOp) -> Action {
    match op {
        CanvasOp::Toggle => Action::ToggleWorkspaceMode {},
        CanvasOp::ZoomIn => Action::CanvasZoomIn { at_cursor: false },
        CanvasOp::ZoomOut => Action::CanvasZoomOut { at_cursor: false },
        CanvasOp::Fit => Action::CanvasFit {},
        CanvasOp::Home => Action::CanvasHome {},
    }
}

/// What the chip shows; the panel re-renders only when this changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HudModel {
    pub workspace: u64,
    pub label: String,
    pub focus: Option<String>,
    /// Rounded zoom percent of the live view.
    pub zoom_pct: Option<u32>,
    pub hint: Option<String>,
}

pub struct CanvasHudPanel {
    pub ctx: PanelContext,
    views: Entity<Arc<LiveViews>>,
    binds: Entity<KeyBinds>,
    model: Option<HudModel>,
    /// The chip comes and goes with the mode (280 ms out-cubic).
    fade: ModeFade<HudModel>,
}

impl CanvasHudPanel {
    /// The chip content, or `None` when the chip is not shown on this display.
    fn compute(&self, cx: &App) -> Option<HudModel> {
        let t = theme(cx);
        let d = common::display_ctx(&self.ctx, t, cx)?;
        let niri = self.ctx.niri.read(cx);
        if common::suppressed(niri, d.output.as_deref()) {
            return None;
        }
        let ws = common::active_workspace(niri, d.output.as_deref())?;
        if !niri.is_canvas(ws.id) {
            return None;
        }
        let views = self.views.read(cx);
        let zoom = views.canvas(ws.id).or_else(|| niri.canvas_view(ws.id)).map(|v| v.zoom);
        let binds = self.binds.read(cx);
        let zoom_keys = binds.pair("canvas-zoom-out", "canvas-zoom-in", true);
        let fit_keys = binds.keys_for("canvas-fit", true);
        let hint = match (zoom_keys, fit_keys) {
            (Some(z), Some(f)) => Some(format!("{z} — zoom · {f} — fit")),
            (Some(z), None) => Some(format!("{z} — zoom")),
            (None, Some(f)) => Some(format!("{f} — fit")),
            (None, None) => None,
        };
        Some(HudModel {
            workspace: ws.id,
            label: common::workspace_label(niri, ws),
            focus: common::focus_window(niri, ws).map(common::window_label),
            zoom_pct: zoom.map(|z| (z * 100.0).round().max(0.0) as u32),
            hint,
        })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = self.compute(cx);
        if next != self.model {
            let flips = next.is_some() != self.model.is_some();
            self.model = next;
            self.fade.sync(self.model.as_ref());
            cx.notify();
            if flips {
                self.ctx.shell.refresh_overlays(cx);
            }
        }
    }
}

impl Panel for CanvasHudPanel {
    const ID: PanelId = PanelId("canvas-hud");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let views = common::live_views(&ctx.niri_handle, cx);
        let binds = common::keybinds(cx);
        cx.observe(&ctx.niri, |this, _, cx| this.refresh(cx)).detach();
        // Live views change every frame of a pan: `refresh` only notifies when the
        // rounded zoom of this display's canvas changed.
        cx.observe(&views, |this, _, cx| this.refresh(cx)).detach();
        cx.observe(&binds, |this, _, cx| this.refresh(cx)).detach();
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |this, _, cx| this.refresh(cx)).detach();
        }
        let mut this = Self { ctx, views, binds, model: None, fade: ModeFade::new(None) };
        this.model = this.compute(cx);
        this.fade = ModeFade::new(this.model.clone());
        this
    }

    fn is_live(&self, _cx: &App) -> bool {
        self.fade.is_live()
    }

    fn on_request(&mut self, request: &Request, _cx: &mut Context<Self>) -> Option<Response> {
        let Request::Canvas(op) = request else { return None };
        // Fire-and-forget like every other shell → niri action; niri ignores canvas-only
        // actions on strip workspaces.
        self.ctx.niri_handle.send_action(canvas_action(*op));
        Some(Response::Ok)
    }
}

impl Render for CanvasHudPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().relative();
        let shown = self.fade.view();
        if self.fade.animating() {
            window.request_animation_frame();
        }
        if self.fade.take_gone() {
            // the exit is over: the overlay host may drop the surface
            self.ctx.shell.refresh_overlays(cx);
        }
        let Some(common::Shown { content: m, fade, travel, .. }) = shown else { return root };
        let t = theme(cx).clone();
        let Some(d) = common::display_ctx(&self.ctx, &t, cx) else { return root };
        let chip = common::card(&t, t.radius_control)
            .absolute()
            .left(d.area.origin.x + px(HUD_LEFT))
            .top(d.area.origin.y + px(HUD_TOP) - px(SWAP_TRAVEL * (1.0 - travel)))
            .opacity(fade)
            .flex()
            .items_center()
            .gap(px(GAP))
            .px(px(PAD_X))
            .py(px(PAD_Y))
            .child(common::label(&m.label, t.fg_muted, &t))
            .child(common::label("canvas", t.fg, &t))
            .children(m.focus.map(|f| common::label(format!("focus: {f}"), t.fg_muted, &t)))
            .children(m.zoom_pct.map(|z| common::label(format!("{z}%"), t.fg_muted, &t)))
            .children(m.hint.map(|h| common::label(h, t.fg_muted, &t)));
        root.child(chip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_ops_map_to_niri_actions() {
        let names: Vec<String> = [CanvasOp::Toggle, CanvasOp::ZoomIn, CanvasOp::ZoomOut, CanvasOp::Fit, CanvasOp::Home]
            .into_iter()
            .map(|op| format!("{:?}", canvas_action(op)))
            .collect();
        assert_eq!(
            names,
            [
                "ToggleWorkspaceMode",
                "CanvasZoomIn { at_cursor: false }",
                "CanvasZoomOut { at_cursor: false }",
                "CanvasFit",
                "CanvasHome"
            ]
        );
    }
}
