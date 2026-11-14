//! Layout of the theme switcher (det-13): header "Theme switcher" + "live preview" (15/600,
//! label, 1px rule); five cards (5 columns, gap 10, padding 12); the 300px preview;
//! the "apply to" strip. Every card draws its OWN theme: panel colour, radius, sans for the
//! name and mono for the font line.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    BoxShadow, Context, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, point, px, size,
};
use ashd_tokens::{ThemeName, metrics};
use ashd_ui::{CheckState, Icon, Theme, checkbox_box, icon, keycap, label, overlay_panel, theme};

use crate::{
    Status, ThemeSwitcherPanel,
    consumers::Consumer,
    preview::preview,
};

/// Width of the mock's theme column (1840 − 2·56 padding − 640 − 40) plus this panel's padding.
pub const PANEL_W: f32 = 1090.0;
const MARGIN: f32 = 32.0;
const PAD: f32 = 20.0;

impl Render for ThemeSwitcherPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let out = self.ctx.shell.modal_display(cx).map(|d| d.size()).unwrap_or_else(|| size(px(1920.), px(1080.)));
        let (out_w, out_h): (f32, f32) = (out.width.into(), out.height.into());
        let max_h = (out_h - 2.0 * MARGIN).max(240.0);
        let width = PANEL_W.min((out_w - 2.0 * MARGIN).max(480.0));

        let (pv, fading) = self.preview_now();
        if fading {
            window.request_animation_frame();
        }

        let cards = div()
            .flex()
            .gap(px(10.))
            .children((0..ThemeName::ALL.len()).map(|ix| self.card(ix, cx)));

        div().size_full().flex().items_center().justify_center().child(
            overlay_panel(cx)
                .id("theme-switcher-panel")
                .occlude()
                .track_focus(&self.focus)
                .key_context("ThemeSwitcher")
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| this.on_key(ev, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
                )
                .w(px(width))
                .max_h(px(max_h))
                .p(px(PAD))
                .flex()
                .flex_col()
                .gap(px(22.))
                .font_family(t.font_sans.clone())
                .text_color(t.fg)
                .child(self.header(&t, cx))
                .child(
                    div()
                        .id("theme-switcher-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .child(cards.flex_none())
                        .child(preview(&pv).flex_none())
                        .child(self.apply_row(&t, cx).flex_none()),
                ),
        )
    }
}

impl ThemeSwitcherPanel {
    fn header(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let shown = ThemeName::ALL[self.desired_preview()];
        let state = match &self.status {
            Some(Status::Failed(e)) => div().child(label(format!("not saved: {e}"), cx).text_color(t.err)),
            // While another card is previewed the hint wins over the last result.
            _ if shown != self.applied => div().child(label(format!("preview {} · ↵ apply", shown.title()), cx)),
            Some(Status::Applied { theme, files }) => {
                div().child(label(format!("applied {} · {} files", theme.title(), files), cx))
            }
            None => div().child(label("live preview", cx)),
        };
        div()
            .flex()
            .flex_none()
            .items_baseline()
            .gap(px(12.))
            .pb(px(10.))
            .border_b_1()
            .border_color(t.line)
            .child(div().text_size(px(metrics::type_scale::TITLE.0)).font_weight(FontWeight::SEMIBOLD).child("Theme switcher"))
            .child(state)
    }

    /// One theme card, in its own theme's colours and fonts.
    fn card(&self, ix: usize, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let name = ThemeName::ALL[ix];
        let ct = &self.themes[ix];
        let fonts = &ashd_tokens::tokens(name).fonts;
        let selected = ix == self.selected;
        let ring = BoxShadow {
            color: ct.accent,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: false,
        };
        let swatches = [ct.bg, ct.surface_2, ct.line, ct.fg_muted, ct.fg, ct.accent];
        div()
            .id(ElementId::from(("theme-card", ix as u32)))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(12.))
            .bg(ct.surface)
            .border_1()
            .border_color(if selected { ct.accent } else { ct.line })
            .rounded(ct.radius_panel)
            .when(selected, |d| d.shadow(vec![ring]))
            .cursor_pointer()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.set_hover(ix, *hovered, cx)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(ix, cx);
                this.apply(cx);
            }))
            .child(div().flex().h(px(28.)).w_full().children(swatches.map(|c| div().flex_1().bg(c))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .child(
                        div()
                            .font_family(ct.font_sans.clone())
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ct.fg)
                            .child(name.title()),
                    )
                    .child(
                        div()
                            .flex()
                            .opacity(if selected { 1.0 } else { 0.0 })
                            .child(icon(Icon::Check, 13.).text_color(ct.accent)),
                    ),
            )
            .child(
                div()
                    .font_family(ct.font_mono.clone())
                    .text_size(px(10.))
                    .text_color(ct.fg_muted)
                    .truncate()
                    .child(SharedString::from(format!("{} · {}", fonts.sans, fonts.mono))),
            )
    }

    /// "apply to" Shell / kitty / nvim / fm / GTK apps, and the key hints on the right.
    fn apply_row(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let boxes = Consumer::ALL.into_iter().enumerate().map(|(ix, consumer)| {
            let on = self.apply_to.get(consumer);
            div()
                .id(ElementId::from(("apply-to", ix as u32)))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(12.5))
                .when(!consumer.always_on(), |d| {
                    d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.toggle_consumer(consumer, cx)))
                })
                .child(checkbox_box(CheckState::from(on), cx))
                .child(consumer.label())
        });
        let hint = |keys: &[&str], text: &'static str, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().flex().gap(px(3.)).children(keys.iter().map(|k| keycap(SharedString::from(k.to_string()), cx))))
                .child(div().text_size(px(11.5)).text_color(t.fg_muted).child(text))
        };
        div()
            .flex()
            .items_center()
            .gap(px(20.))
            .child(label("apply to", cx))
            .children(boxes)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(hint(&["←", "→"], "select", cx))
                    .child(hint(&["↵"], "apply", cx))
                    .child(hint(&["Esc"], "close", cx)),
            )
    }
}
