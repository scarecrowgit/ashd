//! Bar end (desk-05 l.1745–1771 + det-13 session button): "02 code scroll", the
//! Scroll | Canvas segmented control, │ session button.

use gpui::{
    App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::*, px,
};
use ashd_ipc::{PanelOp, Request};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, Slot, frame};
use ashd_tokens::{DockVariant, metrics};
use ashd_ui::{Icon, icon, theme};

use crate::{BoundsStore, caps, geom, hover_layer, ids, is_open, open_layer, sep};

#[derive(Default)]
pub struct BarEndState {}

pub struct BarEndPanel {
    pub ctx: PanelContext,
    pub state: BarEndState,
    bounds: BoundsStore,
}

impl BarEndPanel {
    /// (number, name, is canvas, id) of the active workspace of this panel's output
    /// (focused one if the output is unknown). The number counts the visible workspaces
    /// (niri's `idx` also counts the hidden scratch workspace).
    fn workspace(&self, cx: &App) -> Option<(u8, Option<String>, bool, u64)> {
        let niri = self.ctx.niri.read(cx);
        let output = self.ctx.display.as_ref().and_then(|d| self.ctx.shell.display(d.id, cx)).and_then(|d| d.output);
        let ws = match output {
            Some(o) => niri.active_workspace(&o),
            None => niri.focused_workspace(),
        }?;
        // a fullscreen workspace has no number or name of its own: it is labelled as its desk
        let desk = niri.desk(ws.id).unwrap_or(ws);
        let n = niri.visible_number(desk.id).map_or(desk.idx.max(1), |n| n.min(u8::MAX as usize) as u8);
        Some((n, desk.name.clone(), niri.is_canvas(ws.id), ws.id))
    }

    /// Set this display's workspace to `mode` (`set-workspace-mode` acts on the focused
    /// workspace: focus it first when it is not).
    fn set_mode(&self, canvas: bool, cx: &mut App) {
        use ashd_niri::niri_ipc::{Action, WorkspaceMode, WorkspaceReferenceArg};
        let Some((_, _, is_canvas, id)) = self.workspace(cx) else { return };
        if is_canvas == canvas {
            return;
        }
        let focused = self.ctx.niri.read(cx).workspaces.get(&id).is_some_and(|w| w.is_focused);
        let mode = if canvas { WorkspaceMode::Canvas } else { WorkspaceMode::Strip };
        let handle = self.ctx.niri_handle.clone();
        cx.spawn(async move |_| {
            if !focused && handle.action(Action::FocusWorkspace { reference: WorkspaceReferenceArg::Id(id) }).await.is_err() {
                return;
            }
            // Fire-and-forget: ashd-niri logs a failure.
            handle.send_action(Action::SetWorkspaceMode { mode });
        })
        .detach();
    }
}

impl Panel for BarEndPanel {
    const ID: PanelId = PanelId("bar-end");
    const PLACEMENT: Placement = Placement::Frame(Slot::BarEnd);

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        // Only the active workspace label depends on niri: skip window-only churn.
        cx.observe(&ctx.niri, {
            let mut last = None;
            move |this: &mut Self, _, cx| {
                let now = this.workspace(cx);
                if now != last {
                    last = now;
                    cx.notify();
                }
            }
        })
        .detach();
        if let Some(shell) = ctx.shell.entity() {
            cx.observe(&shell, |_, _, cx| cx.notify()).detach();
        }
        let bounds = BoundsStore::new(&ctx);
        Self { ctx, state: BarEndState::default(), bounds }
    }
}

impl Render for BarEndPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (fg, muted) = (t.fg, t.fg_muted);
        let ws = self.workspace(cx);
        let canvas = ws.as_ref().is_some_and(|w| w.2);

        // "02 code scroll": a change of workspace, name or mode dips and comes back (120 ms)
        let label_key = (ws.as_ref().map(|w| (w.0, w.1.clone())), canvas);
        let label = ashd_ui::swap("bar-end-label", label_key, {
            let t = t.clone();
            move |p| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(geom::END_LABEL_GAP))
                    .opacity(p)
                    .children(ws.as_ref().map(|(idx, ..)| caps(format!("{idx:02}"), geom::END_INDEX_SIZE, t.fg, &t)))
                    .children(ws.and_then(|(_, n, ..)| n).filter(|n| !n.is_empty()).map(|n| caps(n, metrics::type_scale::LABEL.0, t.fg_muted, &t)))
                    .child(caps(if canvas { "canvas" } else { "scroll" }, metrics::type_scale::LABEL.0, t.fg_muted, &t))
            }
        });

        // The selected segment is this display's workspace mode (ashd fork; a stock niri
        // always reports Strip). Clicking the other one sets it (`set-workspace-mode`).
        let segment = |id: &'static str, ic: Icon, text: &'static str, active: bool| {
            let (accent, on_accent) = (t.accent, t.on_accent);
            let (radius, sans) = (frame::nested_radius(&t), t.font_sans.clone());
            let timing = ashd_ui::presence::Timing::new(ashd_ui::anim::motion::timing::SWAP, ashd_ui::anim::EASE_STANDARD);
            let p = if active { 1.0 } else { 0.0 };
            div()
                .id(id)
                .relative()
                .h(px(geom::SEG_H))
                .px(px(geom::SEG_PAD_X))
                .flex()
                .items_center()
                .rounded(radius)
                .font_family(sans)
                .text_size(px(geom::SEG_TEXT))
                .font_weight(FontWeight::MEDIUM)
                // the accent plate cross-fades between the two segments, the label with it
                .child(
                    ashd_ui::follow((id, 1u32), p, timing, move |p| {
                        div().absolute().top_0().left_0().size_full().rounded(radius).bg(ashd_ui::mix(gpui::transparent_black(), accent, p))
                    })
                    .fade(),
                )
                .when(!active, |d| d.cursor_pointer().child(hover_layer(&t)))
                .child(
                    ashd_ui::follow((id, 2u32), p, timing, move |p| {
                        let color = ashd_ui::mix(muted, on_accent, p);
                        div()
                            .flex()
                            .items_center()
                            .gap(px(geom::SEG_ICON_GAP))
                            .text_color(color)
                            .child(icon(ic, geom::SEG_ICON).text_color(color))
                            .child(text)
                    })
                    .fade(),
                )
        };
        let modes = div()
            .flex()
            .flex_none()
            .gap(px(geom::SEG_PAD))
            .p(px(geom::SEG_PAD))
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_control)
            .child(segment("bar-mode-scroll", Icon::Columns, "Scroll", !canvas).on_click(cx.listener(|this, _, _, cx| this.set_mode(false, cx))))
            .child(segment("bar-mode-canvas", Icon::Canvas, "Canvas", canvas).on_click(cx.listener(|this, _, _, cx| this.set_mode(true, cx))));

        let session_open = is_open(ids::SESSION, &self.ctx, cx);
        let session = div()
                .id("bar-session")
                .relative()
                .flex_none()
                .size(px(geom::SESSION))
                .flex()
                .items_center()
                .justify_center()
                .rounded(t.radius_control)
                .cursor_pointer()
                .child(open_layer(session_open, &t))
                .when(!session_open, |d| d.child(hover_layer(&t)))
                .child(icon(Icon::Power, geom::SESSION_ICON).text_color(fg))
                .child(self.bounds.track("session"))
                .on_click(cx.listener(|this, _, _, cx| {
                    let anchor = this.bounds.anchor("session", &this.ctx, cx);
                    this.ctx.shell.request(Request::Session { op: PanelOp::Toggle }, anchor, cx);
                }));

        match t.dock {
            // Ash (desk-05 l.1745): "02 code scroll", modes │ session, 14 from the right.
            DockVariant::Bar => div()
                .h_full()
                .pr(px(metrics::BAR_END_INSET - 4.))
                .flex()
                .items_center()
                .gap(px(geom::END_GAP))
                .child(label)
                .child(modes)
                .child(sep(&t))
                .child(session),
            // Ink / Moss (desk-06 l.1745): the third pill — modes │ session (the label is in
            // the launcher pill drawn by the dock).
            DockVariant::Split => frame::frame_box(&t)
                .h_full()
                .px(px(frame::SPLIT_PILL_PAD_X))
                .flex()
                .items_center()
                .gap(px(frame::PILL_GAP))
                .child(modes)
                .child(sep(&t))
                .child(session),
            // Midnight / Dusk (desk-07 l.1690): the tail of the dock pill (shell-core draws
            // the pill and the separator before this).
            DockVariant::Pill => div()
                .h_full()
                .flex()
                .items_center()
                .gap(px(frame::PILL_GAP))
                .child(modes)
                .child(sep(&t))
                .child(session),
        }
    }
}
