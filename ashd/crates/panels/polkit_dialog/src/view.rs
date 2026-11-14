//! The dialog card (det-09 "polkit"): 440 wide, padding 20, gap 14; header (36×36 icon
//! box with an accent border, title 15/600, message 12.5 muted, action id mono 11 muted);
//! user picker field (34, chevron when there are several identities); password field
//! (34, `bg` fill, accent border when focused / `err` on error, error line 11.5 `err`);
//! footer with the CAPS "details ›" toggle, Cancel (ghost) and Authenticate (primary)
//! 32 px buttons. Details (requesting processes + polkit's details map) unfold below.

use gpui::{
    AnimationExt as _, AnyElement, App, Context, Div, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, deferred, div,
    prelude::FluentBuilder as _, px,
};
use ashd_svc_polkit::{AuthRequest, Phase, ProcessRole};
use ashd_tokens::{metrics, motion};
use ashd_ui::{
    ButtonKind, ControlState, Icon, MenuItem, Theme, anim, button, button_state, field_error, hline, icon, label, menu, spinner,
    theme,
};

use crate::{DIALOG_W, PolkitDialogPanel, primary_label, prompt_label, queue_label};

const TITLE: &str = "Administrator password required";
const PAD: f32 = 20.0;

pub(crate) fn render(
    p: &mut PolkitDialogPanel,
    req: Option<AuthRequest>,
    window: &mut Window,
    cx: &mut Context<PolkitDialogPanel>,
) -> AnyElement {
    let t = theme(cx).clone();
    // Full-output layer over the scrim: occluding it means a click outside the card never
    // reaches the modal host's dismiss handler — the dialog closes only via Cancel/Escape.
    let root = div().size_full().relative().flex().items_center().justify_center().occlude();
    let Some(req) = req else { return root.into_any_element() };
    let queue = p.ctx.services.polkit.state.read(cx).queue_len;
    let input_focused = p.state.input.read(cx).is_focused(window);

    let body = div()
        .p(px(PAD))
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(header(&req, queue, &t, cx))
        .child(user_field(p, &req, &t, cx))
        .child(password_field(p, &req, input_focused, &t, cx))
        .child(footer(p, &req, cx))
        .when(p.state.details_open, |d| d.child(details(&req, &t, cx)));

    let card = ashd_ui::overlay_panel(cx)
        .id("polkit-card")
        .occlude()
        .w(px(DIALOG_W))
        .text_color(t.fg)
        .font_family(t.font_sans.clone())
        .track_focus(&p.state.focus)
        .key_context("PolkitDialog")
        .on_action(cx.listener(|this, _: &crate::EscapeRequest, _, cx| this.escape(cx)))
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
            // Enter in the field is the field's Submit; this covers the dialog's own focus
            // (Retry after a helper failure).
            if ev.keystroke.key == "enter" && !this.state.input.read(cx).is_focused(window) {
                this.authenticate(cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                // A click anywhere on the card closes the menu and gives the focus back
                // to where the phase wants it (the field while prompting).
                this.state.menu_open = false;
                this.state.focused = None;
                cx.notify();
            }),
        )
        .child(body)
        .with_animation(
            SharedString::from(format!("polkit-in-{}", req.id)),
            anim::tween(motion::DUR_FAST, anim::EASE_STANDARD),
            |el, d| el.opacity(d),
        );
    root.child(card).into_any_element()
}

fn header(req: &AuthRequest, queue: usize, t: &Theme, cx: &App) -> Div {
    let icon_box = div()
        .size(px(36.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(t.accent)
        .rounded(t.radius_control)
        .child(icon(Icon::Lock, 16.).text_color(t.accent));
    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(metrics::type_scale::TITLE.0))
                .line_height(px(metrics::type_scale::TITLE.1))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.fg)
                .child(TITLE),
        )
        .child(
            div()
                .text_size(px(metrics::type_scale::UI.0))
                .line_height(px(metrics::type_scale::UI.0 * 1.5))
                .text_color(t.fg_muted)
                .child(req.message.clone()),
        )
        .child(div().font_family(t.font_mono.clone()).text_size(px(11.)).text_color(t.fg_muted).child(req.action_id.clone()));
    div()
        .flex()
        .gap(px(12.))
        .items_start()
        .child(icon_box)
        .child(text)
        .children(queue_label(queue).map(|q| label(q, cx)))
}

/// Frame of the two 34 px fields.
fn field_frame(t: &Theme) -> Div {
    div()
        .h(px(metrics::INPUT_H))
        .w_full()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .border_1()
        .rounded(t.radius_control)
}

fn user_field(p: &PolkitDialogPanel, req: &AuthRequest, t: &Theme, cx: &mut Context<PolkitDialogPanel>) -> Div {
    let several = req.identities.len() > 1;
    let enabled = several && req.phase != Phase::Checking;
    let current = req.selected_identity().map(|i| i.label()).unwrap_or_default();
    let open = p.state.menu_open && enabled;
    let mut field = field_frame(t)
        .id("polkit-user")
        .border_color(if open { t.accent } else { t.line })
        .text_size(px(metrics::type_scale::UI.0))
        .text_color(t.fg)
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(current))
        .child(div().flex_1())
        .when(several, |d| {
            d.child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12.).text_color(t.fg_muted))
        });
    if enabled {
        field = field
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.menu_open = !this.state.menu_open;
                cx.notify();
            }));
    } else if several {
        field = field.opacity(0.4);
    }
    let mut block = div().relative().flex().flex_col().gap(px(6.)).child(label("user", cx)).child(field);
    if open {
        let mut list = menu(cx).w(px(DIALOG_W - 2.0 * PAD)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (ix, ident) in req.identities.iter().enumerate() {
            let uid = ident.uid;
            list = list.child(
                MenuItem::new(("polkit-identity", ix as u32), ident.label())
                    .icon(Icon::User)
                    .selected(req.selected == Some(uid))
                    .build(cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_identity(uid, cx))),
            );
        }
        let top = metrics::type_scale::LABEL.1 + 6.0 + metrics::INPUT_H + metrics::POPUP_GAP / 2.0;
        block = block.child(deferred(div().absolute().top(px(top)).left_0().child(list)).with_priority(1));
    }
    block
}

fn password_field(p: &PolkitDialogPanel, req: &AuthRequest, focused: bool, t: &Theme, cx: &App) -> Div {
    let prompting = req.phase == Phase::Prompting;
    let busy = matches!(req.phase, Phase::Checking | Phase::Starting);
    let has_error = req.error.is_some();
    let border = if has_error && !busy {
        t.err
    } else if focused && prompting {
        t.accent
    } else {
        t.line
    };
    let field = field_frame(t)
        .border_color(border)
        .bg(t.bg)
        .font_family(t.font_mono.clone())
        .text_size(px(metrics::type_scale::CODE.0))
        .text_color(t.fg)
        .child(div().flex().flex_1().min_w_0().child(p.state.input.clone()))
        .when(busy, |d| d.child(spinner(SharedString::from(format!("polkit-spin-{}", req.id)), 14., t.fg_muted)))
        .when(req.phase == Phase::Unavailable, |d| d.opacity(0.4));
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(label(prompt_label(req.prompt.as_deref()), cx))
        .child(field)
        .children(req.error.clone().filter(|_| !busy).map(|e| field_error(e, cx)))
        .children(req.info.clone().map(|i| {
            div().text_size(px(11.5)).text_color(t.fg_muted).child(i.trim_end().to_string())
        }))
}

fn footer(p: &PolkitDialogPanel, req: &AuthRequest, cx: &mut Context<PolkitDialogPanel>) -> Div {
    let toggle = div()
        .id("polkit-details")
        .cursor_pointer()
        .on_click(cx.listener(|this, _, _, cx| {
            this.state.details_open = !this.state.details_open;
            cx.notify();
        }))
        .child(label(if p.state.details_open { "details ‹" } else { "details ›" }, cx));
    let label_text = primary_label(req.phase);
    let ready = matches!(req.phase, Phase::Prompting | Phase::Unavailable);
    let primary = if ready {
        button("polkit-auth", label_text, ButtonKind::Primary, cx).on_click(cx.listener(|this, _, _, cx| this.authenticate(cx)))
    } else {
        button_state("polkit-auth", label_text, ButtonKind::Primary, ControlState::Disabled, false, cx)
    };
    div().flex().justify_between().items_center().child(toggle).child(
        div()
            .flex()
            .gap(px(8.))
            .child(button("polkit-cancel", "Cancel", ButtonKind::Ghost, cx).on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
            .child(primary),
    )
}

fn detail_row(key: impl Into<SharedString>, value: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .flex()
        .gap(px(10.))
        .font_family(t.font_mono.clone())
        .text_size(px(11.))
        .line_height(px(16.))
        .child(div().w(px(132.)).flex_none().text_color(t.fg_muted).child(key.into()))
        .child(div().flex_1().min_w_0().text_color(t.fg).child(value.into()))
}

fn details(req: &AuthRequest, t: &Theme, cx: &App) -> Div {
    let mut col = div().flex().flex_col().gap(px(6.)).child(hline(cx));
    for proc_ in &req.processes {
        let role = match proc_.role {
            ProcessRole::Subject => "requested by",
            ProcessRole::Caller => "caller",
        };
        let who = if proc_.exe.is_none() && proc_.comm.is_none() {
            format!("pid {} (gone or not readable)", proc_.pid)
        } else {
            format!("{} (pid {})", proc_.program(), proc_.pid)
        };
        col = col.child(detail_row(role, who, t));
        if let Some(cmd) = &proc_.cmdline {
            col = col.child(detail_row("", cmd.clone(), t));
        }
    }
    col = col.child(detail_row("action", req.action_id.clone(), t));
    if !req.icon_name.is_empty() {
        col = col.child(detail_row("icon", req.icon_name.clone(), t));
    }
    for (k, v) in &req.details {
        col = col.child(detail_row(k.clone(), v.clone(), t));
    }
    col
}
