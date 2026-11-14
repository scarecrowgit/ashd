//! Things that float or fill: menu, tooltip, toast card, empty state, window frame
//! (docs/design/pages/sys-04-components-ash.html). Layer-2 surfaces cast the flyout shadow
//! (`0 18px 40px .35`); [`crate::overlay_panel`] (launcher, popovers) casts the larger
//! overlay shadow.

use gpui::{
    AnyElement, AnyView, App, Context, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Stateful, Styled as _, Window, div,
    prelude::*, px,
};

use crate::{
    icons::{Icon, icon},
    metrics,
    theme::theme,
    ui::{label, progress},
};

// ── menu ──────────────────────────────────────────────────────────────────────

/// Menu container: 240 wide, 4px padding, `surface`, 1px `line` border, flyout shadow.
/// Children: [`MenuItem::build`] rows and [`menu_separator`]s. Override the width with
/// `.w(..)`.
pub fn menu(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .w(px(metrics::MENU_W))
        .p(px(metrics::MENU_PAD))
        .flex()
        .flex_col()
        .flex_none()
        .bg(t.surface)
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_panel)
        .shadow(t.shadow_flyout())
        .occlude()
}

/// 1px rule with 4px vertical margin.
pub fn menu_separator(cx: &App) -> Div {
    div().h(px(1.)).w_full().my(px(4.)).flex_none().bg(theme(cx).line)
}

/// A menu row: 30 high, padding 0 10, gap 10, sans 12.5; 14px `fg-muted` icon, label, then
/// a mono 10.5 shortcut or a submenu chevron.
#[derive(Clone)]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    icon: Option<Icon>,
    shortcut: Option<SharedString>,
    submenu: bool,
    destructive: bool,
    disabled: bool,
    selected: bool,
}

impl MenuItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            shortcut: None,
            submenu: false,
            destructive: false,
            disabled: false,
            selected: false,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Right-aligned mono hint (`↵`, `F2`, `del`).
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Shows a chevron instead of a shortcut.
    pub fn submenu(mut self) -> Self {
        self.submenu = true;
        self
    }

    /// `err` text and icon.
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// 40 % opacity, no hover; attach no `on_click`.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Hovered / keyboard-highlighted row (`surface-2`).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The row element; add `.on_click(..)`.
    pub fn build(self, cx: &App) -> Stateful<Div> {
        let t = theme(cx);
        let (fg, icon_color) = if self.destructive { (t.err, t.err) } else { (t.fg, t.fg_muted) };
        let s2 = t.surface_2;
        let mut row = div()
            .id(self.id)
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
            .when(self.selected, |d| d.bg(t.surface_2));
        row = if self.disabled {
            row.opacity(0.4).cursor_not_allowed()
        } else {
            row.cursor_pointer().when(!self.selected, move |d| d.hover(move |s| s.bg(s2)))
        };
        if let Some(ic) = self.icon {
            row = row.child(div().flex().flex_none().child(icon(ic, metrics::icon_size::DENSE).text_color(icon_color)));
        }
        row = row.child(div().flex_1().child(self.label));
        if self.submenu {
            row = row.child(icon(Icon::ChevronRight, 12.).text_color(t.fg_muted));
        } else if let Some(sc) = self.shortcut {
            row = row.child(
                div()
                    .font_family(t.font_mono.clone())
                    .text_size(px(metrics::type_scale::LABEL.0))
                    .text_color(t.fg_muted)
                    .child(sc),
            );
        }
        row
    }
}

// ── tooltip ───────────────────────────────────────────────────────────────────

/// Inverted tooltip: `fg` background, `bg` text, padding 6 8, sans 11.5/500, an optional
/// mono 10.5 shortcut after the text.
pub fn tooltip(text: impl Into<SharedString>, shortcut: Option<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .px(px(8.))
        .py(px(6.))
        .flex()
        .gap(px(8.))
        .bg(t.fg)
        .text_color(t.bg)
        .rounded(t.radius_control)
        .font_family(t.font_sans.clone())
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
        .when_some(shortcut, |d, sc| {
            d.child(
                div()
                    .font_family(t.font_mono.clone())
                    .text_size(px(metrics::type_scale::LABEL.0))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(t.surface_2)
                    .child(sc),
            )
        })
}

/// View behind [`TooltipExt::ashd_tooltip`].
pub struct TooltipView {
    text: SharedString,
    shortcut: Option<SharedString>,
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tooltip(self.text.clone(), self.shortcut.clone(), cx)
    }
}

/// `.ashd_tooltip("Fit all windows", Some("mod+0"))` on any element with an id.
pub trait TooltipExt: gpui::StatefulInteractiveElement + Sized {
    fn ashd_tooltip(self, text: impl Into<SharedString>, shortcut: Option<&str>) -> Self {
        let text: SharedString = text.into();
        let shortcut = shortcut.map(|s| SharedString::from(s.to_string()));
        self.tooltip(move |_, cx| -> AnyView {
            let (text, shortcut) = (text.clone(), shortcut.clone());
            cx.new(|_| TooltipView { text, shortcut }).into()
        })
    }
}

impl<T: gpui::StatefulInteractiveElement + Sized> TooltipExt for T {}

// ── toast ─────────────────────────────────────────────────────────────────────

/// Toast card container: `surface`, 1px `line` border (`err` when `critical`), flyout
/// shadow, clipped; [`metrics::TOAST_CARD_W`] wide by default (notification toasts set
/// [`metrics::TOAST_W`]).
pub fn toast_card(critical: bool, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .w(px(metrics::TOAST_CARD_W))
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .overflow_hidden()
        .bg(t.surface)
        .border_1()
        .border_color(if critical { t.err } else { t.line })
        .rounded(t.radius_panel)
        .shadow(t.shadow_flyout())
}

/// A complete toast: 32×32 icon box, source and time labels, title (13/600), body (12.5
/// `fg-muted`), small action buttons and the 2px timer bar along the bottom.
pub struct Toast {
    icon: Icon,
    source: SharedString,
    time: SharedString,
    title: SharedString,
    body: Option<SharedString>,
    actions: Vec<AnyElement>,
    timer: Option<f32>,
    critical: bool,
}

impl Toast {
    pub fn new(icon: Icon, title: impl Into<SharedString>) -> Self {
        Self {
            icon,
            source: SharedString::default(),
            time: SharedString::from("now"),
            title: title.into(),
            body: None,
            actions: Vec::new(),
            timer: None,
            critical: false,
        }
    }

    /// App / channel label (rendered as an uppercase [`label`]).
    pub fn source(mut self, source: impl Into<SharedString>) -> Self {
        self.source = source.into();
        self
    }

    pub fn time(mut self, time: impl Into<SharedString>) -> Self {
        self.time = time.into();
        self
    }

    pub fn body(mut self, body: impl Into<SharedString>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Remaining time as a fraction (1 = full), drawn as the bottom bar.
    pub fn timer(mut self, remaining: f32) -> Self {
        self.timer = Some(remaining);
        self
    }

    /// `err` border and icon.
    pub fn critical(mut self, critical: bool) -> Self {
        self.critical = critical;
        self
    }

    /// Add an action (use [`crate::button_small`]).
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn build(self, cx: &App) -> Div {
        let t = theme(cx);
        let icon_color = if self.critical { t.err } else { t.fg };
        let mut text = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(label(self.source, cx))
                    .child(label(self.time, cx)),
            )
            .child(
                div()
                    .font_family(t.font_sans.clone())
                    .text_size(px(metrics::type_scale::BODY.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.fg)
                    .child(self.title),
            );
        if let Some(body) = self.body {
            text = text.child(
                div()
                    .font_family(t.font_sans.clone())
                    .text_size(px(metrics::type_scale::UI.0))
                    .line_height(px(18.))
                    .text_color(t.fg_muted)
                    .child(body),
            );
        }
        if !self.actions.is_empty() {
            text = text.child(div().flex().gap(px(6.)).mt(px(6.)).children(self.actions));
        }
        toast_card(self.critical, cx)
            .child(
                div()
                    .p(px(14.))
                    .flex()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(32.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(if self.critical { t.err } else { t.line })
                            .child(icon(self.icon, 15.).text_color(icon_color)),
                    )
                    .child(text),
            )
            .when_some(self.timer, |d, remaining| d.child(progress(remaining, metrics::PROGRESS_THIN, cx)))
    }
}

// ── empty state ───────────────────────────────────────────────────────────────

/// Centred empty state: 22px `fg-muted` icon, title (14/600), hint (12.5 `fg-muted`, max
/// 280 wide) and an optional row of small buttons. Fills its parent.
pub fn empty_state(
    ic: Icon,
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    actions: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    div()
        .size_full()
        .p(px(24.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .text_center()
        .font_family(t.font_sans.clone())
        .child(icon(ic, metrics::icon_size::EMPTY).text_color(t.fg_muted))
        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(t.fg).child(title.into()))
        .child(
            div()
                .max_w(px(280.))
                .text_size(px(metrics::type_scale::UI.0))
                .line_height(px(19.))
                .text_color(t.fg_muted)
                .child(hint.into()),
        )
        .when(!actions.is_empty(), |d| d.child(div().flex().gap(px(6.)).children(actions)))
}

// ── window frame ──────────────────────────────────────────────────────────────

/// Window box (layer 1): `surface-2`, 1px accent frame when focused, `frame` otherwise,
/// window radius, clipped. Children: [`window_titlebar`] + content.
pub fn window_frame(focused: bool, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(t.surface_2)
        .border_1()
        .border_color(if focused { t.accent } else { t.frame })
        .rounded(t.radius_window)
}

/// 28 high title row: mono 11 title on the left, `fg-muted` info on the right, 1px rule.
pub fn window_titlebar(title: impl Into<SharedString>, info: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .h(px(28.))
        .flex_none()
        .px(px(12.))
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(t.line)
        .font_family(t.font_mono.clone())
        .text_size(px(11.))
        .child(div().text_color(t.fg).child(title.into()))
        .child(div().text_color(t.fg_muted).child(info.into()))
}
