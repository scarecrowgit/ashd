//! Shared building blocks (docs/design/pages/sys-04-components-ash.html). Everything reads
//! the [`Theme`](crate::Theme) global; nothing here owns state except [`slider`]'s drag
//! state (kept in GPUI element state, keyed by the slider id).
//!
//! Interactive components return `Stateful<Div>`; attach behaviour with `.on_click(..)`.
//! Components with a value take a callback compatible with `cx.listener`:
//! `toggle("wifi", on, cx.listener(|this, on: &bool, _, cx| …), cx)`.

use gpui::{
    AnyElement, App, Bounds, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, SharedString, Stateful, Window, canvas, div, hsla, prelude::*, px, relative,
};

use crate::{
    anim::motion,
    fx::{fill_bar, follow, hover_wash, mix},
    icons::{Icon, icon},
    metrics,
    presence::Timing,
    theme::theme,
};

/// 120 ms standard: a switch flipping.
const FLIP: Timing = Timing::new(motion::DUR_FAST, motion::EASE_STANDARD);
/// 80 ms: a slider thumb catching up with a value that jumped (a click on the track).
const CATCH_UP: Timing = Timing::new(motion::timing::HOVER, motion::EASE_STANDARD);

fn clear() -> Hsla {
    hsla(0., 0., 0., 0.)
}

/// `color` at `alpha` opacity.
pub fn alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

/// Label role: mono 10.5, UPPERCASE (lower case as written where the theme's
/// `label_caps` is off: Midnight, Dusk), `fg-muted`. (GPUI has no letter-spacing; the
/// +0.08em / +0.01em tracking is dropped.)
pub fn label(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    let text: SharedString = text.into();
    div()
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::LABEL.0))
        .line_height(px(metrics::type_scale::LABEL.1))
        .text_color(t.fg_muted)
        .flex_none()
        .child(t.label_text(&text))
}

/// Mono text at the code size (12).
pub fn mono(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div().font_family(t.font_mono.clone()).text_size(px(metrics::type_scale::CODE.0)).child(text.into())
}

/// Numeric role: mono 16/500 (clock, percentages).
pub fn numeric(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::NUMERIC.0))
        .line_height(px(metrics::type_scale::NUMERIC.1))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.fg)
        .child(text.into())
}

/// Title role: sans 15/600.
pub fn title(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(t.font_sans.clone())
        .text_size(px(metrics::type_scale::TITLE.0))
        .line_height(px(metrics::type_scale::TITLE.1))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.fg)
        .child(text.into())
}

/// Keycap / kbd chip: 20 high, padding 0 6, mono 10.5, `fg`, 1px line border with a 2px
/// bottom border (cheatsheet, hints).
pub fn keycap(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .border_1()
        .border_b_2()
        .border_color(t.line)
        .rounded(t.radius_control)
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::LABEL.0))
        .text_color(t.fg)
        .child(text.into())
}

/// 30×30 ghost icon button. Add `.on_click(..)` yourself.
pub fn icon_button(id: impl Into<ElementId>, ic: Icon, cx: &App) -> Stateful<Div> {
    icon_button_sized(id, ic, 30., 15., cx)
}

pub fn icon_button_sized(id: impl Into<ElementId>, ic: Icon, box_size: f32, icon_size: f32, cx: &App) -> Stateful<Div> {
    let t = theme(cx);
    let (hover_bg, fg) = (t.surface_2, t.fg);
    div()
        .id(id)
        .size(px(box_size))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(t.radius_control)
        .cursor_pointer()
        .child(hover_wash(hover_bg, Some(t.line), t.radius_control))
        .child(icon(ic, icon_size).text_color(fg))
}

/// Segmented control (Scroll | Canvas). Active segment: accent bg, `on-accent` text.
pub fn segmented(
    id: impl Into<ElementId>,
    items: impl IntoIterator<Item = impl Into<SharedString>>,
    selected: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let on_select = std::rc::Rc::new(on_select);
    let id: ElementId = id.into();
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(24.))
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_control)
        .overflow_hidden()
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::LABEL.0))
        .children(items.into_iter().enumerate().map(|(ix, item)| {
            let active = ix == selected;
            let on_select = on_select.clone();
            let text: SharedString = item.into();
            let s2 = t.surface_2;
            div()
                .id(ix)
                .px(px(10.))
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .when(active, |d| d.bg(t.accent).text_color(t.on_accent))
                .when(!active, |d| d.text_color(t.fg_muted).child(hover_wash(s2, None, px(0.))))
                .child(SharedString::from(text.to_uppercase()))
                .on_click(move |_: &ClickEvent, window, cx| on_select(&ix, window, cx))
        }))
}

/// 34×18 switch (1px border, 12×12 knob inset 2px). On: accent track and border,
/// `on-accent` knob; off: line border, `fg-muted` knob. A change of `on` (a click, or
/// the service flipping it) glides over 120 ms (standard curve): knob, track and border
/// together; a switch that is drawn already on does not animate.
pub fn toggle(
    id: impl Into<ElementId>,
    on: bool,
    on_toggle: impl Fn(&bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let id: ElementId = id.into();
    let (line, accent, on_accent, muted) = (t.line, t.accent, t.on_accent, t.fg_muted);
    let travel = metrics::TOGGLE_W - 2.0 - metrics::TOGGLE_KNOB - 4.0;
    let (x_off, x_on) = (2.0, 2.0 + travel);
    let radius = t.radius_control;
    let knob_radius = (radius - px(1.)).max(px(0.));
    let fx = follow("toggle-fx", if on { 1.0 } else { 0.0 }, FLIP, move |p| {
        div()
            .absolute()
            .size_full()
            .border_1()
            .border_color(mix(line, accent, p))
            .bg(mix(clear(), accent, p))
            .rounded(radius)
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(x_off + (x_on - x_off) * p))
                    .size(px(metrics::TOGGLE_KNOB))
                    .rounded(knob_radius)
                    .bg(mix(muted, on_accent, p)),
            )
    });
    div()
        .id(id)
        .w(px(metrics::TOGGLE_W))
        .h(px(metrics::TOGGLE_H))
        .flex_none()
        .rounded(radius)
        .cursor_pointer()
        .relative()
        .child(fx)
        .on_click(move |_, window, cx| on_toggle(&!on, window, cx))
}

/// Horizontal meter (cpu/mem in the status bar, volume in the OSD). `value` in 0..=1.
pub fn meter(value: f32, width: f32, cx: &App) -> Div {
    meter_colored(value, width, theme(cx).accent, cx)
}

/// [`meter`] with a state colour (`warn` at 86 % disk, `err` at 8 % battery). The status
/// bar meter is 40 wide ([`metrics::METER_W`]) and 4 high.
pub fn meter_colored(value: f32, width: f32, fill: Hsla, cx: &App) -> Div {
    let t = theme(cx);
    let v = value.clamp(0.0, 1.0);
    let radius = t.radius_control;
    div()
        .w(px(width))
        .h(px(metrics::METER_H))
        .flex_none()
        .bg(t.line)
        .rounded(t.radius_control)
        .child(fill_bar(v, fill, radius))
}

/// Thin progress bar: `height` 2 / 3 / 4 ([`metrics::PROGRESS_THIN`] …), `line` track,
/// accent fill; full width of the parent. The toast timer is the 2px one.
pub fn progress(value: f32, height: f32, cx: &App) -> Div {
    let t = theme(cx);
    let accent = t.accent;
    div()
        .w_full()
        .h(px(height))
        .flex_none()
        .bg(t.line)
        .child(fill_bar(value, accent, px(0.)))
}

#[derive(Default)]
struct SliderState {
    bounds: Option<Bounds<Pixels>>,
    dragging: bool,
}

/// Slider with click-to-set and drag. `value` in 0..=1; `on_change` gets the new value
/// on every move while dragging.
pub fn slider(
    id: impl Into<ElementId>,
    value: f32,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id: ElementId = id.into();
    let state = window.use_keyed_state(id.clone(), cx, |_, _| SliderState::default());
    let t = theme(cx).clone();
    let (accent, thumb, radius) = (t.accent, t.fg, t.radius_control);
    let v = value.clamp(0.0, 1.0);
    let on_change = std::rc::Rc::new(on_change);
    let value_at = |state: &SliderState, x: Pixels| -> Option<f32> {
        let b = state.bounds?;
        Some(((x - b.left()) / b.size.width).clamp(0.0, 1.0))
    };
    let dragging = state.read(cx).dragging;
    let (s_down, s_move, s_up, s_bounds) = (state.clone(), state.clone(), state.clone(), state);
    let (c_down, c_move) = (on_change.clone(), on_change);
    div()
        .id(id)
        .h(px(metrics::SLIDER_THUMB_H + 4.))
        .w_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .child(
            div()
                .relative()
                .w_full()
                .h(px(metrics::SLIDER_TRACK_H))
                .bg(t.line)
                .rounded(t.radius_control)
                .child(
                    // Fill and thumb catch up with a value that jumped (a click on the
                    // track, a service answer); a drag follows the pointer at once.
                    follow("slider-fx", v, CATCH_UP, move |v| {
                        div()
                            .absolute()
                            .size_full()
                            .child(div().h_full().w(relative(v)).bg(accent).rounded(radius))
                            .child(
                                div()
                                    .absolute()
                                    .left(relative(v))
                                    .ml(px(-metrics::SLIDER_THUMB_W / 2.))
                                    .top(px(-(metrics::SLIDER_THUMB_H - metrics::SLIDER_TRACK_H) / 2.))
                                    .w(px(metrics::SLIDER_THUMB_W))
                                    .h(px(metrics::SLIDER_THUMB_H))
                                    .bg(thumb),
                            )
                    })
                    .immediate(dragging),
                )
                .child(
                    canvas(
                        move |bounds, _, cx| s_bounds.update(cx, |s, _| s.bounds = Some(bounds)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                ),
        )
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
            let v = s_down.update(cx, |s, _| {
                s.dragging = true;
                value_at(s, ev.position.x)
            });
            if let Some(v) = v {
                c_down(&v, window, cx);
            }
        })
        .on_mouse_move(move |ev: &MouseMoveEvent, window, cx| {
            let v = s_move.read(cx);
            if v.dragging
                && ev.pressed_button == Some(MouseButton::Left)
                && let Some(v) = value_at(v, ev.position.x)
            {
                c_move(&v, window, cx);
            }
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| s_up.update(cx, |s, _| s.dragging = false))
}

/// 30-high list row (sans 12.5): `fg-muted` text, hover `surface-2` + `fg`; `selected` is
/// `surface-2` + `fg`. Children: a 14px icon, a `flex_1` label, a trailing [`label`].
pub fn list_row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let t = theme(cx);
    let (s2, fg) = (t.surface_2, t.fg);
    div()
        .id(id)
        .h(px(metrics::ROW_H))
        .w_full()
        .px(px(10.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .rounded(t.radius_control)
        .font_family(t.font_sans.clone())
        .text_size(px(metrics::type_scale::UI.0))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.surface_2).text_color(t.fg))
        .when(!selected, |d| {
            d.text_color(t.fg_muted).hover(move |s| s.text_color(fg)).child(hover_wash(s2, None, t.radius_control))
        })
}

/// Rich list row, min 40 high: 1px border (`line` when selected, transparent otherwise),
/// `surface-2` fill when selected. Children as in the clipboard list: a `fg-muted` icon, a
/// mono 11.5 text ([`rich_row_text`]), a pin and an age [`label`].
pub fn rich_row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let t = theme(cx);
    let s2 = t.surface_2;
    div()
        .id(id)
        .min_h(px(metrics::RICH_ROW_H))
        .w_full()
        .px(px(10.))
        .py(px(8.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .border_1()
        .border_color(if selected { t.line } else { clear() })
        .rounded(t.radius_control)
        .text_color(t.fg)
        .cursor_pointer()
        .when(selected, |d| d.bg(t.surface_2))
        .when(!selected, |d| d.child(hover_wash(s2, None, t.radius_control)))
}

/// The flexible text of a [`rich_row`]: mono 11.5, one line, ellipsis.
pub fn rich_row_text(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .font_family(t.font_mono.clone())
        .text_size(px(11.5))
        .child(text.into())
}

/// Flat panel (layer 0): `surface`, 1px `frame` border, panel radius. No shadow.
pub fn panel(cx: &App) -> Div {
    let t = theme(cx);
    div().bg(t.surface).border_1().border_color(t.frame).rounded(t.radius_panel)
}

/// Overlay panel (layer 2): like [`panel`] plus the overlay shadow. Menus, popups,
/// launcher.
pub fn overlay_panel(cx: &App) -> Div {
    let t = theme(cx);
    panel(cx).shadow(t.shadow_overlay())
}

/// 1px horizontal divider.
pub fn hline(cx: &App) -> Div {
    div().h(px(1.)).w_full().flex_none().bg(theme(cx).line)
}

/// 1px vertical divider.
pub fn vline(cx: &App) -> Div {
    div().w(px(1.)).h_full().flex_none().bg(theme(cx).line)
}

/// An empty element (for `Option`-less "render nothing" returns).
pub fn empty() -> AnyElement {
    div().into_any_element()
}
