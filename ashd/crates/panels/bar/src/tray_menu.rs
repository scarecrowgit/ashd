//! A tray item's dbusmenu in the modal overlay (the kit's menu component: 240 wide, rows
//! 30 high, separators, disabled rows, check / radio marks, submenus to the side).
//!
//! The service publishes the menu (`TrayState::menu`); this panel only draws it and sends
//! `menu_click` / `menu_about_to_show`. The bar (through `TrayUi`) opens the modal with the
//! icon's bounds as anchor; the menu grows up from it like every popup of the bar.
//!
//! Submenus open to the side on hover with the 120 ms fade-in / 100 ms grace of the rail
//! flyout (`SUBMENU_IN` / `SUBMENU_OUT`): leaving a submenu's parent row for a sibling only
//! switches after 100 ms, so the pointer may cross other rows on its way into the
//! submenu. Keyboard: Up / Down move (wrapping, separators and disabled rows skipped),
//! Right opens a submenu, Left closes it, Enter / Space activates, Home / End, Escape
//! closes the whole menu.

use std::time::Duration;

use gpui::{
    AnimationExt as _, App, Context, Div, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::*, px,
};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_tray::{MenuKind, MenuNode, Toggle};
use ashd_tokens::{LayoutVariant, metrics, motion::timing};
use ashd_ui::{
    Icon, Theme,
    anim::{EASE_OUT_QUAD, tween},
    app_icon, icon, menu, menu_separator, theme,
};

use crate::{geom, tray::lead_slot, tray::popup_bottom};

/// Clear of the output edge.
const EDGE: f32 = 8.0;
/// Submenus overlap their parent by this much.
const OVERLAP: f32 = 2.0;
/// Border + padding between a menu's outer edge and its rows.
const INSET: f32 = metrics::MENU_PAD + 1.0;

#[derive(Default)]
struct MenuUi {
    /// `TrayState::menu.seq` this state belongs to.
    seq: u64,
    /// Open submenus, outermost first.
    path: Vec<i32>,
    /// Highlighted row.
    cursor: Option<i32>,
    /// Bumped by every hover: a delayed switch applies only if nothing happened since.
    hover: u64,
}

pub struct TrayMenuPanel {
    ctx: PanelContext,
    focus: FocusHandle,
    ui: MenuUi,
}

fn selectable(n: &MenuNode) -> bool {
    n.visible && n.enabled && n.kind == MenuKind::Normal
}

fn row_height(n: &MenuNode) -> f32 {
    if n.kind == MenuKind::Separator { geom::MENU_SEPARATOR_H } else { metrics::MENU_ROW_H }
}

/// Outer height of a menu showing the visible children of `parent`.
fn menu_height(parent: &MenuNode) -> f32 {
    2.0 * INSET + parent.visible_children().map(row_height).sum::<f32>()
}

/// The nodes along `path` (stops at the first id that no longer exists or is not a
/// submenu): `[root, path[0], path[1], …]`.
fn chain<'a>(root: &'a MenuNode, path: &[i32]) -> Vec<&'a MenuNode> {
    let mut out = vec![root];
    for id in path {
        match out.last().and_then(|p| p.children.iter().find(|c| c.id == *id && c.visible && c.is_submenu())) {
            Some(n) => out.push(n),
            None => break,
        }
    }
    out
}

/// Depth of the menu level showing `id`, within the open chain.
fn depth_of(chain: &[&MenuNode], id: i32) -> Option<usize> {
    chain.iter().position(|level| level.visible_children().any(|c| c.id == id))
}

fn step(level: &MenuNode, from: Option<i32>, delta: i32) -> Option<i32> {
    let rows: Vec<i32> = level.visible_children().filter(|c| selectable(c)).map(|c| c.id).collect();
    if rows.is_empty() {
        return None;
    }
    let at = from.and_then(|f| rows.iter().position(|r| *r == f));
    let next = match (at, delta) {
        (None, d) if d >= 0 => 0,
        (None, _) => rows.len() - 1,
        (Some(i), d) => (i as i32 + d).rem_euclid(rows.len() as i32) as usize,
    };
    rows.get(next).copied()
}

impl TrayMenuPanel {
    fn tray(&self, cx: &App) -> Option<ashd_svc_tray::OpenMenu> {
        self.ctx.services.tray.state.read(cx).menu.clone()
    }

    fn reset(&mut self) {
        self.ui = MenuUi::default();
    }

    /// Make `id` the highlighted row at `depth`, opening its submenu when it has one.
    fn apply(&mut self, depth: usize, id: i32, open: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.tray(cx) else { return };
        let Some(node) = menu.root.find(id).cloned() else { return };
        self.ui.cursor = Some(id);
        if open && self.ui.path.get(depth) == Some(&id) {
            // back on the row whose submenu is open: only what is below it closes
            self.ui.path.truncate(depth + 1);
            cx.notify();
            return;
        }
        self.ui.path.truncate(depth);
        if open && node.visible && node.enabled && node.is_submenu() {
            self.ui.path.push(id);
            if node.children.is_empty() {
                self.ctx.services.tray.handle.menu_about_to_show(&menu.item, id);
            }
        }
        cx.notify();
    }

    fn hover_row(&mut self, depth: usize, id: i32, cx: &mut Context<Self>) {
        self.ui.hover += 1;
        let generation = self.ui.hover;
        if self.ui.path.len() > depth && self.ui.path.get(depth) != Some(&id) {
            // A submenu is open below: the pointer may be on its way into it.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(timing::SUBMENU_OUT)).await;
                this.update(cx, |this, cx| {
                    if this.ui.hover == generation {
                        this.apply(depth, id, true, cx);
                    }
                })
                .ok();
            })
            .detach();
        } else {
            self.apply(depth, id, true, cx);
        }
    }

    fn activate(&mut self, id: i32, cx: &mut Context<Self>) {
        let Some(menu) = self.tray(cx) else { return };
        let Some(node) = menu.root.find(id) else { return };
        if !node.visible || !node.enabled || node.kind == MenuKind::Separator {
            return;
        }
        if node.is_submenu() {
            let depth = depth_of(&chain(&menu.root, &self.ui.path), id).unwrap_or(0);
            self.ui.hover += 1;
            self.apply(depth, id, true, cx);
            // keyboard / click opens it with the first row highlighted
            if let Some(first) = menu.root.find(id).and_then(|n| step(n, None, 1)) {
                self.ui.cursor = Some(first);
            }
            cx.notify();
            return;
        }
        self.ctx.services.tray.handle.menu_click(&menu.item, id);
        self.ctx.shell.close_modal(cx);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(menu) = self.tray(cx) else { return };
        let levels = chain(&menu.root, &self.ui.path);
        let depth = self.ui.cursor.and_then(|c| depth_of(&levels, c)).unwrap_or(levels.len() - 1);
        let level = levels[depth.min(levels.len() - 1)];
        match ev.keystroke.key.as_str() {
            "down" | "tab" if !ev.keystroke.modifiers.shift => self.move_to(depth, step(level, self.ui.cursor, 1), cx),
            "up" => self.move_to(depth, step(level, self.ui.cursor, -1), cx),
            "tab" => self.move_to(depth, step(level, self.ui.cursor, -1), cx),
            "home" => self.move_to(depth, step(level, None, 1), cx),
            "end" => self.move_to(depth, step(level, None, -1), cx),
            "right" => {
                if let Some(c) = self.ui.cursor
                    && menu.root.find(c).is_some_and(|n| n.is_submenu() && n.enabled)
                {
                    self.activate(c, cx);
                }
            }
            "left" => {
                if depth > 0 {
                    self.ui.cursor = self.ui.path.get(depth - 1).copied();
                    self.ui.path.truncate(depth - 1);
                    self.ui.hover += 1;
                    cx.notify();
                }
            }
            "enter" | "space" => {
                if let Some(c) = self.ui.cursor {
                    self.activate(c, cx);
                }
            }
            _ => {}
        }
    }

    fn move_to(&mut self, depth: usize, id: Option<i32>, cx: &mut Context<Self>) {
        if let Some(id) = id {
            self.ui.hover += 1;
            self.apply(depth, id, false, cx);
        }
    }

    fn row(&self, n: &MenuNode, depth: usize, has_lead: bool, open: bool, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = n.id;
        let highlighted = self.ui.cursor == Some(id) || open;
        let (fg, muted, s2) = (t.fg, t.fg_muted, t.surface_2);
        let lead = match n.toggle {
            Toggle::Check(true) => Some(icon(Icon::Check, metrics::icon_size::DENSE).text_color(fg).into_any_element()),
            Toggle::Radio(true) => Some(div().size(px(6.)).rounded(px(3.)).bg(fg).into_any_element()),
            Toggle::None => n.icon.as_ref().map(|p| app_icon(Some(p.as_path()), metrics::icon_size::DENSE, cx)),
            _ => None,
        };
        let mut row = div()
            .id(ElementId::from(("tray-menu-row", id as u32)))
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
            .text_color(fg)
            .when(highlighted, |d| d.bg(s2));
        row = if n.enabled {
            row.cursor_pointer()
                .when(!highlighted, move |d| d.hover(move |s| s.bg(s2)))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.hover_row(depth, id, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.activate(id, cx)))
        } else {
            row.opacity(0.4).cursor_not_allowed()
        };
        if has_lead {
            row = row.child(lead_slot(lead));
        }
        row = row.child(div().flex_1().min_w_0().truncate().child(SharedString::from(n.label.clone())));
        if n.is_submenu() {
            row = row.child(icon(Icon::ChevronRight, 12.).text_color(muted));
        } else if let Some(sc) = &n.shortcut {
            row = row.child(
                div()
                    .font_family(t.font_mono.clone())
                    .text_size(px(metrics::type_scale::LABEL.0))
                    .text_color(muted)
                    .child(SharedString::from(sc.clone())),
            );
        }
        row
    }

    /// One menu level: `parent`'s visible children, with the deeper open levels nested in
    /// the rows that opened them. `left` / `top`: this level's outer corner, output-local.
    fn level(
        &self,
        levels: &[&MenuNode],
        depth: usize,
        left: f32,
        top: f32,
        output: (f32, f32),
        seq: u64,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let parent = levels[depth];
        let has_lead = parent.visible_children().any(|c| c.toggle != Toggle::None || c.icon.is_some());
        let mut y = top + INSET;
        let mut m = menu(cx);
        for n in parent.visible_children() {
            if n.kind == MenuKind::Separator {
                m = m.child(menu_separator(cx));
                y += geom::MENU_SEPARATOR_H;
                continue;
            }
            let open = levels.get(depth + 1).is_some_and(|next| next.id == n.id);
            let mut row = self.row(n, depth, has_lead, open, t, cx);
            if open {
                // to the side, flipped when the output ends; shifted up when it would end below
                let flip = left + 2.0 * metrics::MENU_W - OVERLAP > output.0 - EDGE && left - metrics::MENU_W + OVERLAP >= EDGE;
                let sub_left = if flip { left - metrics::MENU_W + OVERLAP } else { left + metrics::MENU_W - OVERLAP };
                let sub_h = menu_height(levels[depth + 1]);
                let mut sub_top = y - INSET;
                let overflow = sub_top + sub_h - (output.1 - EDGE);
                if overflow > 0.0 {
                    sub_top = (sub_top - overflow).max(EDGE);
                }
                let nested = self.level(levels, depth + 1, sub_left, sub_top, output, seq, t, cx);
                let rel_left = if flip { -(metrics::MENU_W + INSET - OVERLAP) } else { metrics::MENU_W - INSET - OVERLAP };
                let anim_id = ElementId::NamedInteger(format!("tray-menu-sub-{}", n.id).into(), seq);
                row = row.child(
                    nested
                        .absolute()
                        .left(px(rel_left))
                        .top(px(sub_top - (y - INSET) - INSET))
                        .with_animation(anim_id, tween(timing::SUBMENU_IN, EASE_OUT_QUAD), move |el, p| {
                            el.opacity(p).ml(px(if flip { 4.0 } else { -4.0 } * (1.0 - p)))
                        }),
                );
            }
            m = m.child(row);
            y += metrics::MENU_ROW_H;
        }
        m
    }
}

impl Panel for TrayMenuPanel {
    const ID: PanelId = PanelId("tray-menu");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.tray.state, |_, _, cx| cx.notify()).detach();
        Self { ctx, focus: cx.focus_handle(), ui: MenuUi::default() }
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset();
        self.focus.focus(window, cx);
    }

    fn on_close(&mut self, _cx: &mut Context<Self>) {
        self.reset();
        self.ctx.services.tray.handle.close_menu();
    }
}

impl Render for TrayMenuPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root_el = div().size_full().relative();
        let Some(menu) = self.tray(cx) else { return root_el };
        if menu.seq != self.ui.seq {
            // a different menu than the one the highlight belongs to
            self.ui = MenuUi { seq: menu.seq, ..MenuUi::default() };
        }
        let t = theme(cx).clone();
        let levels = chain(&menu.root, &self.ui.path);
        let display = self.ctx.shell.modal_display(cx);
        let output = display.map_or((1920.0, 1080.0), |d| (f32::from(d.size().width), f32::from(d.size().height)));
        let anchor = self.ctx.shell.modal_anchor(<Self as Panel>::ID, cx);
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let max_left = (output.0 - metrics::MENU_W - EDGE.max(margin)).max(margin);
        let left = anchor.map_or(margin, |a| f32::from(a.origin.x)).clamp(margin, max_left);
        let height = menu_height(levels[0]);
        let top = (output.1 - popup_bottom(&t) - height).max(EDGE);

        let built = self
            .level(&levels, 0, left, top, output, menu.seq, &t, cx)
            .absolute()
            .left(px(left))
            .top(px(top))
            .track_focus(&self.focus)
            .key_context("TrayMenu")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.on_key(ev, cx)))
            .with_animation("tray-menu-in", tween(timing::SUBMENU_IN, EASE_OUT_QUAD), |el, p| el.opacity(p));
        root_el.child(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: i32, label: &str) -> MenuNode {
        MenuNode { id, label: label.into(), enabled: true, visible: true, ..Default::default() }
    }

    fn sample() -> MenuNode {
        let mut sep = n(2, "");
        sep.kind = MenuKind::Separator;
        let mut off = n(3, "off");
        off.enabled = false;
        let mut hidden = n(4, "hidden");
        hidden.visible = false;
        let mut sub = n(5, "sub");
        sub.submenu = true;
        sub.children = vec![n(6, "inner a"), n(7, "inner b")];
        MenuNode { id: 0, visible: true, enabled: true, children: vec![n(1, "a"), sep, off, hidden, sub, n(8, "z")], ..Default::default() }
    }

    #[test]
    fn heights_count_separators_and_skip_hidden() {
        let root = sample();
        // a, sep, off, sub, z = 4 rows + 1 separator
        let want = 2.0 * INSET + 4.0 * metrics::MENU_ROW_H + geom::MENU_SEPARATOR_H;
        assert_eq!(menu_height(&root), want);
    }

    #[test]
    fn stepping_skips_separators_disabled_and_hidden_and_wraps() {
        let root = sample();
        assert_eq!(step(&root, None, 1), Some(1));
        assert_eq!(step(&root, Some(1), 1), Some(5), "separator, disabled and hidden skipped");
        assert_eq!(step(&root, Some(5), 1), Some(8));
        assert_eq!(step(&root, Some(8), 1), Some(1), "wraps");
        assert_eq!(step(&root, Some(1), -1), Some(8), "wraps backwards");
        assert_eq!(step(&root, None, -1), Some(8));
        assert_eq!(step(&n(0, ""), None, 1), None);
    }

    #[test]
    fn the_chain_follows_open_submenus_and_stops_at_stale_ids() {
        let root = sample();
        assert_eq!(chain(&root, &[]).len(), 1);
        let c = chain(&root, &[5]);
        assert_eq!(c.iter().map(|l| l.id).collect::<Vec<_>>(), [0, 5]);
        assert_eq!(chain(&root, &[5, 99]).len(), 2, "unknown id");
        assert_eq!(chain(&root, &[1]).len(), 1, "not a submenu");
        assert_eq!(depth_of(&c, 6), Some(1));
        assert_eq!(depth_of(&c, 1), Some(0));
        assert_eq!(depth_of(&c, 404), None);
    }
}
