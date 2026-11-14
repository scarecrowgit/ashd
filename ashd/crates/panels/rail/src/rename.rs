//! Rename a workspace: a modal (keyboard Exclusive) card at the flyout's place with one
//! text field. Enter → niri `SetWorkspaceName` (empty → `UnsetWorkspaceName`); Escape or
//! a click outside cancels.

use gpui::{
    App, AppContext as _, Context, DisplayId, Entity, Focusable as _, FontWeight, Global, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Window, div, point, prelude::*, px,
};
use ashd_niri::niri_ipc::{Action, WorkspaceReferenceArg};
use ashd_shell_core::{ModalAnchor, ModalStyle, Panel, PanelContext, PanelId, Placement, ShellHandle};
use ashd_tokens::metrics;
use ashd_ui::{TextInput, TextInputEvent, label, theme};

use crate::{geom, workspace_title};

/// What is being renamed (set by the flyout right before it opens this modal).
#[derive(Clone, Debug, PartialEq)]
pub struct RenameTarget {
    pub workspace: u64,
    pub idx: u8,
    pub name: String,
    pub display: Option<DisplayId>,
}

#[derive(Default)]
struct PendingRename(Option<RenameTarget>);

impl Global for PendingRename {}

/// Open the rename modal for `target`, the card placed at `anchor` (output-local), on the
/// display of the rail that was clicked (not necessarily the focused one).
pub fn begin(target: RenameTarget, anchor: Option<ModalAnchor>, shell: &ShellHandle, cx: &mut App) {
    let display = target.display;
    cx.set_global(PendingRename(Some(target)));
    match display {
        Some(d) => shell.open_modal_on(RailRenamePanel::ID, d, anchor, cx),
        None => shell.open_modal(RailRenamePanel::ID, anchor, cx),
    }
}

pub struct RailRenamePanel {
    pub ctx: PanelContext,
    input: Entity<TextInput>,
    target: Option<RenameTarget>,
}

impl RailRenamePanel {
    fn submit(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.target.take() {
            let name = self.input.read(cx).text().trim().to_string();
            let reference = WorkspaceReferenceArg::Id(t.workspace);
            let action = if name.is_empty() {
                Action::UnsetWorkspaceName { reference: Some(reference) }
            } else {
                Action::SetWorkspaceName { name, workspace: Some(reference) }
            };
            self.ctx.shell.niri_action(action, cx);
        }
        self.ctx.shell.close_modal(cx);
    }
}

impl Panel for RailRenamePanel {
    const ID: PanelId = PanelId("rail-rename");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("workspace name", cx));
        cx.subscribe(&input, |this, _, ev: &TextInputEvent, cx| match ev {
            TextInputEvent::Submit => this.submit(cx),
            TextInputEvent::Cancel => this.ctx.shell.close_modal(cx),
            _ => {}
        })
        .detach();
        Self { ctx, input, target: None }
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.target = cx.try_global::<PendingRename>().and_then(|p| p.0.clone());
        let name = self.target.as_ref().map(|t| t.name.clone()).unwrap_or_default();
        self.input.update(cx, |i, cx| {
            i.set_text(name, cx);
            i.select_all_text(cx);
        });
        self.input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.target = None;
        cx.set_global(PendingRename(None));
    }
}

impl Render for RailRenamePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let anchor = self.ctx.shell.modal_anchor(Self::ID, cx);
        let origin =
            anchor.map(|a| a.origin).unwrap_or(point(t.rail_w + px(geom::flyout_offset_x(&t)), px(geom::BUTTONS_TOP)));
        let title = self.target.as_ref().map(|t| workspace_title(t.idx, None)).unwrap_or_default();
        div().size_full().relative().child(
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .occlude()
                .w(px(geom::FLYOUT_W))
                .p(px(geom::FLYOUT_PAD))
                .flex()
                .flex_col()
                .gap(px(geom::FLYOUT_GAP))
                .bg(t.surface)
                .border_1()
                .border_color(t.frame)
                .rounded(t.radius_panel)
                .shadow(t.shadow_flyout())
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .justify_between()
                        .child(
                            div()
                                .font_family(t.font_sans.clone())
                                .text_size(px(metrics::type_scale::TITLE.0))
                                .line_height(px(metrics::type_scale::TITLE.1))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.fg)
                                .child(format!("Rename {title}")),
                        )
                        .child(label("enter · esc", cx)),
                )
                .child(
                    div()
                        .h(px(metrics::INPUT_H))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .border_1()
                        .border_color(t.accent)
                        .rounded(t.radius_control)
                        .bg(t.bg)
                        .font_family(t.font_sans.clone())
                        .text_size(px(metrics::type_scale::BODY.0))
                        .text_color(t.fg)
                        .child(self.input.clone()),
                ),
        )
    }
}
