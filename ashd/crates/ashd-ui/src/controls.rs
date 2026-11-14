//! Controls of docs/design/pages/sys-04-components-ash.html: buttons (all kinds and
//! states), fields, checkbox, radio, segmented control with icons, tabs, badges, status
//! dots, keyboard-focus ring.
//!
//! Every function reads the [`Theme`](crate::Theme) global. Live behaviour (hover, press)
//! comes from GPUI's `hover` / `active` styles; the `*_state` variants render a *forced*
//! [`ControlState`] so galleries and tests can show every state side by side.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Div, ElementId, FocusHandle, FontWeight, Hsla, InteractiveElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, hsla, prelude::*, px,
};

use crate::{
    icons::{Icon, icon},
    metrics,
    theme::{Theme, theme},
};

/// A control state, for rendering one explicitly (the live components derive hover and
/// pressed from the pointer).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ControlState {
    #[default]
    Default,
    Hover,
    Pressed,
    /// Keyboard focus: the default look plus the focus ring.
    Focus,
    /// 40 % opacity, `not-allowed` cursor, no hover/press.
    Disabled,
}

impl ControlState {
    pub fn from_disabled(disabled: bool) -> Self {
        if disabled { Self::Disabled } else { Self::Default }
    }
}

/// Fully transparent.
fn clear() -> Hsla {
    hsla(0., 0., 0., 0.)
}

// ── focus ring ────────────────────────────────────────────────────────────────

/// Keyboard focus ring (`:focus-visible`: 1px outline in `fg`, 2px offset). GPUI has no
/// `outline`, so it is drawn as two spread shadows; the offset is filled with `gap` (the
/// surface behind the control, `surface` by default).
pub trait FocusRingExt: InteractiveElement + Sized {
    /// Track `handle` and show the ring while it has keyboard focus (not on mouse click).
    /// Give the handle a tab stop (`cx.focus_handle().tab_stop(true)`) to make the control
    /// reachable with Tab.
    fn focus_ring(self, handle: &FocusHandle, cx: &App) -> Self {
        let t = theme(cx);
        self.focus_ring_on(handle, t.surface, cx)
    }

    /// [`focus_ring`](Self::focus_ring) with an explicit offset-fill colour.
    fn focus_ring_on(self, handle: &FocusHandle, gap: Hsla, cx: &App) -> Self {
        let shadows = theme(cx).focus_ring(gap);
        self.track_focus(handle).focus_visible(move |s| s.shadow(shadows))
    }
}

impl<T: InteractiveElement + Sized> FocusRingExt for T {}

// ── buttons ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// Accent fill, `on-accent` text.
    Primary,
    /// 1px line border.
    Secondary,
    /// No border.
    Ghost,
    /// `err` text on a line border.
    Danger,
}

struct ButtonLook {
    bg: Hsla,
    fg: Hsla,
    border: Hsla,
}

/// Resting, hover and pressed looks of a kind (docs sys-04 "Buttons").
fn button_looks(kind: ButtonKind, t: &Theme) -> [ButtonLook; 3] {
    let look = |bg, fg, border| ButtonLook { bg, fg, border };
    match kind {
        ButtonKind::Primary => [
            look(t.accent, t.on_accent, t.accent),
            look(t.accent_hover(), t.on_accent, t.accent),
            look(t.accent_pressed(), t.on_accent, t.accent),
        ],
        ButtonKind::Secondary => [
            look(clear(), t.fg, t.line),
            look(t.surface_2, t.fg, t.line),
            look(t.line, t.fg, t.line),
        ],
        ButtonKind::Ghost => [
            look(clear(), t.fg, clear()),
            look(t.surface_2, t.fg, clear()),
            look(t.line, t.fg, clear()),
        ],
        ButtonKind::Danger => [
            look(clear(), t.err, t.line),
            look(t.surface_2, t.err, t.line),
            look(t.line, t.err, t.line),
        ],
    }
}

/// What goes inside a button.
enum Content {
    Text(SharedString),
    IconText(Icon, SharedString),
    Icon(Icon),
}

fn build_button(
    id: ElementId,
    content: Content,
    kind: ButtonKind,
    small: bool,
    state: Option<ControlState>,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let [rest, hover, pressed] = button_looks(kind, t);
    let live = state.is_none();
    let state = state.unwrap_or_default();
    let shown = match state {
        ControlState::Hover => &hover,
        ControlState::Pressed => &pressed,
        _ => &rest,
    };
    let (height, font) = if small {
        (metrics::BUTTON_SM_H, 11.5)
    } else {
        (metrics::BUTTON_H, metrics::type_scale::UI.0)
    };
    let icon_only = matches!(content, Content::Icon(_));
    let fg = shown.fg;
    let mut el = div()
        .id(id)
        .h(px(height))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .border_1()
        .border_color(shown.border)
        .bg(shown.bg)
        .text_color(fg)
        .rounded(t.radius_control)
        .font_family(t.font_sans.clone())
        .text_size(px(font))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .relative();
    el = if icon_only { el.w(px(metrics::ICON_BUTTON)) } else { el.px(px(12.)) };
    el = match content {
        Content::Text(text) => el.child(text),
        Content::IconText(ic, text) => el.child(icon(ic, metrics::icon_size::DENSE).text_color(fg)).child(text),
        Content::Icon(ic) => el.child(icon(ic, metrics::icon_size::DENSE).text_color(fg)),
    };
    match state {
        ControlState::Pressed => el = el.top(px(1.)),
        ControlState::Focus => el = el.shadow(t.focus_ring(t.surface)),
        ControlState::Disabled => el = el.opacity(0.4).cursor_not_allowed(),
        _ => {}
    }
    if live {
        let (h, p) = (hover, pressed);
        el = el
            .cursor_pointer()
            .hover(move |s| s.bg(h.bg).border_color(h.border))
            .active(move |s| s.bg(p.bg).border_color(p.border).top(px(1.)));
    } else if state != ControlState::Disabled {
        el = el.cursor_pointer();
    }
    el
}

/// Text button, 32 high, sans 12.5/500, padding 0 12. Add `.on_click(..)`.
pub fn button(id: impl Into<ElementId>, text: impl Into<SharedString>, kind: ButtonKind, cx: &App) -> Stateful<Div> {
    build_button(id.into(), Content::Text(text.into()), kind, false, None, cx)
}

/// Small text button, 26 high, sans 11.5/500 (toast actions, empty-state actions).
pub fn button_small(id: impl Into<ElementId>, text: impl Into<SharedString>, kind: ButtonKind, cx: &App) -> Stateful<Div> {
    build_button(id.into(), Content::Text(text.into()), kind, true, None, cx)
}

/// Button with a 14px icon before the label (the danger "Delete" button).
pub fn button_with_icon(
    id: impl Into<ElementId>,
    ic: Icon,
    text: impl Into<SharedString>,
    kind: ButtonKind,
    cx: &App,
) -> Stateful<Div> {
    build_button(id.into(), Content::IconText(ic, text.into()), kind, false, None, cx)
}

/// Square 32×32 icon button (14px glyph) of any [`ButtonKind`]; `Secondary` has the line
/// border, `Ghost` none. [`crate::icon_button`] stays the 30×30 ghost button.
pub fn icon_button_kind(id: impl Into<ElementId>, ic: Icon, kind: ButtonKind, cx: &App) -> Stateful<Div> {
    build_button(id.into(), Content::Icon(ic), kind, false, None, cx)
}

/// A text button rendered in a forced [`ControlState`] (no live hover/press).
pub fn button_state(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    kind: ButtonKind,
    state: ControlState,
    small: bool,
    cx: &App,
) -> Stateful<Div> {
    build_button(id.into(), Content::Text(text.into()), kind, small, Some(state), cx)
}

/// [`button_with_icon`] in a forced state.
pub fn button_with_icon_state(
    id: impl Into<ElementId>,
    ic: Icon,
    text: impl Into<SharedString>,
    kind: ButtonKind,
    state: ControlState,
    cx: &App,
) -> Stateful<Div> {
    build_button(id.into(), Content::IconText(ic, text.into()), kind, false, Some(state), cx)
}

/// [`icon_button_kind`] in a forced state.
pub fn icon_button_state(
    id: impl Into<ElementId>,
    ic: Icon,
    kind: ButtonKind,
    state: ControlState,
    cx: &App,
) -> Stateful<Div> {
    build_button(id.into(), Content::Icon(ic), kind, false, Some(state), cx)
}

// ── text field frame ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FieldState {
    #[default]
    Default,
    /// Accent border (the focused field).
    Focus,
    /// `err` border; pair it with [`field_error`].
    Error,
}

/// The frame of a text field: 34 high, padding 0 10, 1px border (`line`, `accent` when
/// focused, `err` on error), `surface-2` fill, mono 12. Put a [`TextInput`](crate::TextInput)
/// inside (`.child(input.clone())`, wrapped in a `flex_1` div).
pub fn field(state: FieldState, cx: &App) -> Div {
    let t = theme(cx);
    let border = match state {
        FieldState::Default => t.line,
        FieldState::Focus => t.accent,
        FieldState::Error => t.err,
    };
    div()
        .h(px(metrics::INPUT_H))
        .w_full()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .border_1()
        .border_color(border)
        .bg(t.surface_2)
        .rounded(t.radius_control)
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::CODE.0))
        .text_color(t.fg)
}

/// Error message under a field (sans 11.5, `err`).
pub fn field_error(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div().font_family(t.font_sans.clone()).text_size(px(11.5)).text_color(t.err).child(text.into())
}

/// Caret of the mock's static focused field: 1×14 accent bar.
pub fn field_caret(cx: &App) -> Div {
    div().w(px(1.)).h(px(14.)).flex_none().bg(theme(cx).accent)
}

// ── checkbox / radio ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CheckState {
    #[default]
    Unchecked,
    Checked,
    /// Indeterminate: an 8×2 bar.
    Mixed,
}

impl From<bool> for CheckState {
    fn from(on: bool) -> Self {
        if on { Self::Checked } else { Self::Unchecked }
    }
}

/// The 14×14 box alone (no interaction).
pub fn checkbox_box(state: CheckState, cx: &App) -> Div {
    let t = theme(cx);
    let on = state != CheckState::Unchecked;
    let (fill, border) = if on { (t.accent, t.accent) } else { (clear(), t.fg_muted) };
    let b = div()
        .size(px(metrics::CHECK))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(border)
        .bg(fill)
        .rounded(t.radius_control.min(px(3.)));
    match state {
        CheckState::Unchecked => b,
        CheckState::Checked => b.child(icon(Icon::CheckBold, 10.).text_color(t.on_accent)),
        CheckState::Mixed => b.child(div().w(px(8.)).h(px(2.)).bg(t.on_accent)),
    }
}

/// Checkbox; `on_toggle` receives the new checked value (a mixed box toggles to checked).
pub fn checkbox(
    id: impl Into<ElementId>,
    state: CheckState,
    on_toggle: impl Fn(&bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let next = state != CheckState::Checked;
    div()
        .id(id)
        .flex_none()
        .cursor_pointer()
        .child(checkbox_box(state, cx))
        .on_click(move |_: &ClickEvent, window, cx| on_toggle(&next, window, cx))
}

/// The 14×14 round radio alone: accent ring + 6×6 dot when selected, `fg-muted` ring
/// otherwise.
pub fn radio_dot(selected: bool, cx: &App) -> Div {
    let t = theme(cx);
    let d = div()
        .size(px(metrics::CHECK))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if selected { t.accent } else { t.fg_muted });
    if selected { d.child(div().size(px(6.)).rounded_full().bg(t.accent)) } else { d }
}

/// Radio button; `on_select` fires on click (the `&()` keeps it `cx.listener`-compatible).
pub fn radio(
    id: impl Into<ElementId>,
    selected: bool,
    on_select: impl Fn(&(), &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .cursor_pointer()
        .child(radio_dot(selected, cx))
        .on_click(move |_: &ClickEvent, window, cx| on_select(&(), window, cx))
}

// ── segmented control with icons, tabs ────────────────────────────────────────

/// One segment: optional 14px icon and a label.
#[derive(Clone, Debug)]
pub struct Segment {
    pub icon: Option<Icon>,
    pub label: SharedString,
}

impl Segment {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { icon: None, label: label.into() }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// The sys-04 segmented control: 2px padded, 1px line border, 28 high segments (sans
/// 12/500, optional icon), active segment `accent` fill with `on-accent` text. (The
/// compact mono variant for the bar is [`crate::segmented`].)
pub fn segmented_icons(
    id: impl Into<ElementId>,
    items: impl IntoIterator<Item = Segment>,
    selected: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let on_select = Rc::new(on_select);
    div()
        .id(id)
        .flex()
        .flex_none()
        .gap(px(2.))
        .p(px(2.))
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_control)
        .font_family(t.font_sans.clone())
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .children(items.into_iter().enumerate().map(|(ix, seg)| {
            let active = ix == selected;
            let on_select = on_select.clone();
            let fg = if active { t.on_accent } else { t.fg_muted };
            let s2 = t.surface_2;
            div()
                .id(ix)
                .h(px(metrics::SEGMENT_H))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded((t.radius_control - px(1.)).max(px(0.)))
                .cursor_pointer()
                .text_color(fg)
                .when(active, |d| d.bg(t.accent))
                .when(!active, |d| d.hover(move |s| s.bg(s2)))
                .when_some(seg.icon, |d, ic| d.child(icon(ic, metrics::icon_size::DENSE).text_color(fg)))
                .child(seg.label)
                .on_click(move |_: &ClickEvent, window, cx| on_select(&ix, window, cx))
        }))
}

/// One tab: label and an optional mono count.
#[derive(Clone, Debug)]
pub struct Tab {
    pub label: SharedString,
    pub count: Option<SharedString>,
}

impl Tab {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), count: None }
    }

    pub fn count(mut self, count: impl ToString) -> Self {
        self.count = Some(count.to_string().into());
        self
    }
}

/// Tab strip: 34 high, 20px apart, a 1px `line` rule under all, 2px accent underline under
/// the selected tab (sans 12.5/500; selected `fg`, others `fg-muted`).
pub fn tabs(
    id: impl Into<ElementId>,
    items: impl IntoIterator<Item = Tab>,
    selected: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let on_select = Rc::new(on_select);
    div()
        .id(id)
        .flex()
        .gap(px(20.))
        .border_b_1()
        .border_color(t.line)
        .font_family(t.font_sans.clone())
        .text_size(px(metrics::type_scale::UI.0))
        .font_weight(FontWeight::MEDIUM)
        .children(items.into_iter().enumerate().map(|(ix, tab)| {
            let active = ix == selected;
            let on_select = on_select.clone();
            let (fg, muted) = (t.fg, t.fg_muted);
            div()
                .id(ix)
                .h(px(metrics::TAB_H))
                .mb(px(-1.))
                .flex()
                .items_baseline()
                .pt(px(9.))
                .gap(px(5.))
                .border_b_2()
                .border_color(if active { t.accent } else { clear() })
                .text_color(if active { t.fg } else { t.fg_muted })
                .cursor_pointer()
                .when(!active, move |d| d.hover(move |s| s.text_color(fg)))
                .child(tab.label)
                .when_some(tab.count, |d, count| {
                    d.child(
                        div()
                            .font_family(t.font_mono.clone())
                            .text_size(px(metrics::type_scale::LABEL.0))
                            .text_color(muted)
                            .child(count),
                    )
                })
                .on_click(move |_: &ClickEvent, window, cx| on_select(&ix, window, cx))
        }))
}

// ── badges, status dots, kbd ──────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BadgeKind {
    /// Line border, `fg` text.
    Plain,
    /// Accent fill, `on-accent` text (counts).
    Accent,
    /// Coloured border and text.
    Ok,
    Warn,
    Err,
    Info,
}

/// Badge: 22 high, padding 0 8, mono 10.5, 1px border.
pub fn badge(text: impl Into<SharedString>, kind: BadgeKind, cx: &App) -> Div {
    let t = theme(cx);
    let (bg, fg, border) = match kind {
        BadgeKind::Plain => (clear(), t.fg, t.line),
        BadgeKind::Accent => (t.accent, t.on_accent, t.accent),
        BadgeKind::Ok => (clear(), t.ok, t.ok),
        BadgeKind::Warn => (clear(), t.warn, t.warn),
        BadgeKind::Err => (clear(), t.err, t.err),
        BadgeKind::Info => (clear(), t.info, t.info),
    };
    div()
        .h(px(metrics::BADGE_H))
        .px(px(8.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .rounded(t.radius_control)
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::LABEL.0))
        .child(text.into())
}

/// 6×6 round status dot.
pub fn status_dot(color: Hsla) -> Div {
    div().size(px(metrics::STATUS_DOT)).flex_none().rounded_full().bg(color)
}

/// Status dot followed by sans 12 text (`connected`, `12% free`).
pub fn status(color: Hsla, text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .font_family(t.font_sans.clone())
        .text_size(px(12.))
        .text_color(t.fg)
        .child(status_dot(color))
        .child(text.into())
}

/// Keyboard chip (alias of [`crate::keycap`]).
pub fn kbd(text: impl Into<SharedString>, cx: &App) -> Div {
    crate::ui::keycap(text, cx)
}

/// A key combination as chips: `kbd_combo(["mod", "shift", "q"], cx)`.
pub fn kbd_combo(keys: impl IntoIterator<Item = impl Into<SharedString>>, cx: &App) -> Div {
    div().flex().flex_none().items_center().gap(px(4.)).children(keys.into_iter().map(|k| crate::ui::keycap(k, cx)))
}
