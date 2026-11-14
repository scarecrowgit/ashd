//! The tray items that do not fit the bar (Passive ones, and Active ones when space runs
//! out) as a menu-style list above the chevron: icon, name, a warn dot for NeedsAttention.
//! Left click activates, middle click is `SecondaryActivate`, right click opens the item's
//! menu (from the chevron: the item has no icon of its own in the bar), the wheel scrolls.
//! Keyboard: Up / Down, Enter, Escape.

use gpui::{
    AnimationExt as _, ClickEvent, Context, Div, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, ScrollEvent, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::*, px,
};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_tray::{Status, TrayItem};
use ashd_tokens::{LayoutVariant, metrics, motion::timing};
use ashd_ui::{
    Theme,
    anim::{EASE_OUT_QUAD, tween},
    app_icon, menu, theme,
};

use crate::tray::{Button, TrayUi, popup_bottom};
use crate::geom;

pub struct TrayOverflowPanel {
    ctx: PanelContext,
    ui: gpui::Entity<TrayUi>,
    focus: FocusHandle,
    selected: usize,
}

impl TrayOverflowPanel {
    /// The hidden items that still exist, in the order the bar hid them.
    fn items(&self, cx: &gpui::App) -> Vec<TrayItem> {
        let state = self.ctx.services.tray.state.read(cx);
        self.ui.read(cx).hidden.iter().filter_map(|id| state.item(id).cloned()).collect()
    }

    fn click(&mut self, item: &TrayItem, button: Button, cx: &mut Context<Self>) {
        // the list goes away first; a menu then opens from the chevron
        let anchor = self.ui.read(cx).chevron;
        self.ctx.shell.close_modal(cx);
        let item = item.clone();
        self.ui.update(cx, |ui, cx| ui.click(&item, button, anchor, cx));
    }

    fn row(&self, ix: usize, item: &TrayItem, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.selected == ix;
        let (s2, warn) = (t.surface_2, t.warn);
        let id = item.id.clone();
        let label = if item.label().is_empty() { "Unnamed".to_string() } else { item.label().to_string() };
        div()
            .id(ElementId::from(("tray-overflow-row", ix as u32)))
            .relative()
            .h(px(metrics::MENU_ROW_H))
            .w_full()
            .px(px(10.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .rounded(t.radius_control)
            .font_family(t.font_sans.clone())
            .text_size(px(metrics::type_scale::UI.0))
            .text_color(t.fg)
            .cursor_pointer()
            .when(selected, |d| d.bg(s2))
            .when(!selected, move |d| d.hover(move |s| s.bg(s2)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.selected != ix {
                    this.selected = ix;
                    cx.notify();
                }
            }))
            .child(app_icon(item.display_icon().map(|p| p.as_path()), geom::TRAY_ICON, cx))
            .child(div().flex_1().min_w_0().truncate().child(SharedString::from(label)))
            .when(item.status == Status::NeedsAttention, |d| {
                d.child(div().flex_none().size(px(geom::TRAY_DOT)).rounded(px(geom::TRAY_DOT / 2.)).bg(warn))
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, ev: &ClickEvent, _, cx| {
                    let button = if ev.is_right_click() { Button::Right } else { Button::Left };
                    if let Some(item) = this.items(cx).into_iter().find(|i| i.id == id) {
                        this.click(&item, button, cx);
                    }
                }
            }))
            .on_aux_click(cx.listener({
                let id = id.clone();
                move |this, ev: &ClickEvent, _, cx| {
                    let button = if ev.is_middle_click() { Button::Middle } else { Button::Right };
                    if let Some(item) = this.items(cx).into_iter().find(|i| i.id == id) {
                        this.click(&item, button, cx);
                    }
                }
            }))
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, _, cx| {
                this.ui.update(cx, |ui, _| ui.scroll(&id, ev));
            }))
    }
}

impl Panel for TrayOverflowPanel {
    const ID: PanelId = PanelId("tray-overflow");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let ui = TrayUi::get(&ctx, cx);
        cx.observe(&ctx.services.tray.state, |this, _, cx| {
            // the last hidden item left: nothing to list
            if this.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID) && this.items(cx).is_empty() {
                this.ctx.shell.close_modal(cx);
            }
            cx.notify();
        })
        .detach();
        Self { ctx, ui, focus: cx.focus_handle(), selected: 0 }
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0;
        self.focus.focus(window, cx);
    }
}

impl Render for TrayOverflowPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().relative();
        let items = self.items(cx);
        if items.is_empty() {
            return root;
        }
        self.selected = self.selected.min(items.len() - 1);
        let t = theme(cx).clone();
        let output_w = self.ctx.shell.modal_display(cx).map_or(1920.0, |d| f32::from(d.size().width));
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let anchor = self.ctx.shell.modal_anchor(<Self as Panel>::ID, cx);
        let max_left = (output_w - metrics::MENU_W - margin.max(8.0)).max(margin);
        let left = anchor.map_or(margin, |a| f32::from(a.origin.x)).clamp(margin, max_left);

        let rows: Vec<_> = items.iter().enumerate().map(|(ix, item)| self.row(ix, item, &t, cx)).collect();
        let count = items.len();
        let panel = menu(cx)
            .absolute()
            .left(px(left))
            .bottom(px(popup_bottom(&t)))
            .track_focus(&self.focus)
            .key_context("TrayOverflow")
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, _, cx| {
                let items = this.items(cx);
                match ev.keystroke.key.as_str() {
                    "down" => this.selected = (this.selected + 1) % count.max(1),
                    "up" => this.selected = (this.selected + count.max(1) - 1) % count.max(1),
                    "home" => this.selected = 0,
                    "end" => this.selected = count.saturating_sub(1),
                    "enter" | "space" => {
                        if let Some(item) = items.get(this.selected).cloned() {
                            this.click(&item, Button::Left, cx);
                        }
                    }
                    _ => return,
                }
                cx.notify();
            }))
            .children(rows)
            .with_animation("tray-overflow-in", tween(timing::SUBMENU_IN, EASE_OUT_QUAD), |el, p| el.opacity(p));
        root.child(panel)
    }
}
