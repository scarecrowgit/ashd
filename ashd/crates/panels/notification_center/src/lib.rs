//! Notification centre with groups and DND (det-05). Modal, 400 px, growing up from the
//! bell in the status bar.
//!
//! Owner: notification centre. Contract: ARCHITECTURE.md "Panels" — `NotificationCenterPanel`
//! is registered by `ashd-shell` with `reg.register::<NotificationCenterPanel>()`; it
//! answers `Request::Notifications`. The bar's bell sends that request with its own bounds
//! as anchor.
//!
//! Top to bottom: header ("Notifications", "Clear all"), the do-not-disturb row (toggle;
//! click the text to edit the schedule), then the history grouped by app — newest group
//! first, newest item first, scrollable — or the "All caught up" empty state.
//!
//! * [`groups`] — grouping, ages, group glyphs (no GPUI, unit-tested).
//! * [`schedule`] — the DND window and its persistence under the ashd state dir. The panel
//!   re-evaluates it once a minute (a timer task that lives as long as the shell) and on
//!   every edit; it switches DND on when the window opens and off when it closes, and
//!   never fights a manual toggle in between.
//!
//! "Unread": the bar's bell dot compares the service's `seq` with the highest one it saw
//! while this panel was the open modal (`BarPanel::seen_seq`), so nothing needs persisting
//! here.

pub mod groups;
pub mod schedule;

use std::time::Duration;

use chrono::Local;
use gpui::{
    AnimationExt as _, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Task, Window, div, prelude::*, px,
};
use ashd_ipc::{PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_notifications::{Notification, Urgency};
use ashd_tokens::{LayoutVariant, metrics, motion};
use ashd_ui::{
    ButtonKind, FieldState, Icon, TextInput, TextInputEvent, anim, button_small, field, icon, icon_button_sized, label,
    progress, theme, toggle,
};

use crate::{
    groups::{Group, age_label, group_by_app, group_icon},
    schedule::{STEP_MINUTES, Schedule, Tod},
};

/// How often the age labels refresh while the centre is open.
const AGE_REFRESH: Duration = Duration::from_secs(30);
/// Height of the empty state.
const EMPTY_H: f32 = 170.0;
/// Space kept above the card.
const TOP_GAP: f32 = 12.0;

/// Panel state (fm rule: everything this region needs lives here).
pub struct NotificationCenterPanelState {
    focus: FocusHandle,
    pub schedule: Schedule,
    /// The schedule editor under the DND row is expanded.
    schedule_open: bool,
    /// What the schedule last asked for (`None` = disabled / not evaluated yet).
    applied: Option<bool>,
    /// The once-a-minute schedule timer.
    timer: Option<Task<()>>,
    /// Repaints the ages while the centre is open.
    ticker: Option<Task<()>>,
    /// The one inline-reply field, shown under the notification `reply_for`.
    reply: Entity<TextInput>,
    reply_for: Option<u32>,
}

pub struct NotificationCenterPanel {
    pub ctx: PanelContext,
    pub state: NotificationCenterPanelState,
}

impl NotificationCenterPanel {
    fn is_open(&self, cx: &gpui::App) -> bool {
        self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID)
    }

    // ── schedule ──

    /// Make DND follow the schedule when the window opens or closes.
    fn apply_schedule(&mut self, cx: &mut Context<Self>) {
        let schedule = self.state.schedule;
        if !schedule.enabled {
            self.state.applied = None;
            return;
        }
        let want = schedule.wants_dnd(Tod::now());
        if self.state.applied == Some(want) {
            return;
        }
        let prev = self.state.applied.replace(want);
        let notifications = &self.ctx.services.notifications;
        let dnd = notifications.state.read(cx).dnd;
        if want && !dnd {
            notifications.handle.set_dnd(true);
        } else if !want && prev == Some(true) && dnd {
            notifications.handle.set_dnd(false);
        }
        cx.notify();
    }

    fn start_timer(&mut self, cx: &mut Context<Self>) {
        self.state.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(schedule::until_next_minute()).await;
                if this.update(cx, |this, cx| this.apply_schedule(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Edit the schedule: persist it (off the main thread) and apply it right away.
    fn edit_schedule(&mut self, edit: impl FnOnce(&mut Schedule), cx: &mut Context<Self>) {
        edit(&mut self.state.schedule);
        let (path, schedule) = (schedule::path(), self.state.schedule);
        cx.background_spawn(async move {
            if let Err(e) = schedule::save(&path, &schedule) {
                tracing::warn!("dnd schedule: cannot save {}: {e}", path.display());
            }
        })
        .detach();
        self.apply_schedule(cx);
        cx.notify();
    }

    // ── notification actions ──

    /// A click on an item: its default action (then the centre closes so the app can take
    /// focus), or just dismiss it.
    fn activate(&mut self, n: &Notification, cx: &mut Context<Self>) {
        let handle = &self.ctx.services.notifications.handle;
        let has_default = n.has_default_action();
        if has_default {
            handle.invoke_action(n.id, "default");
        }
        // Resident notifications stay after an action; the user asked to get rid of it.
        handle.dismiss(n.id);
        if has_default {
            self.ctx.shell.close_modal(cx);
        }
        cx.notify();
    }

    fn invoke(&mut self, id: u32, key: &str, cx: &mut Context<Self>) {
        let handle = &self.ctx.services.notifications.handle;
        handle.invoke_action(id, key);
        handle.dismiss(id);
        cx.notify();
    }

    /// Show the reply field under notification `id` and focus it.
    fn begin_reply(&mut self, id: u32, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state.reply_for = Some(id);
        self.state.reply.update(cx, |input, cx| {
            input.set_text("", cx);
            input.set_placeholder(placeholder.to_string(), cx);
        });
        self.state.reply.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn end_reply(&mut self, cx: &mut Context<Self>) {
        self.state.reply_for = None;
        self.state.reply.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// Send the typed text as the inline reply (the notification is then dismissed).
    fn send_reply(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.state.reply_for else { return };
        let text = self.state.reply.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let handle = &self.ctx.services.notifications.handle;
        handle.reply(id, &text);
        handle.dismiss(id);
        self.end_reply(cx);
    }

    fn on_reply_event(&mut self, ev: &TextInputEvent, cx: &mut Context<Self>) {
        match ev {
            TextInputEvent::Submit => self.send_reply(cx),
            TextInputEvent::Cancel => self.end_reply(cx),
            _ => {}
        }
    }

    fn dismiss(&mut self, id: u32, cx: &mut Context<Self>) {
        self.ctx.services.notifications.handle.dismiss(id);
        cx.notify();
    }

    fn clear_group(&mut self, ids: &[u32], cx: &mut Context<Self>) {
        for id in ids {
            self.ctx.services.notifications.handle.dismiss(*id);
        }
        cx.notify();
    }
}

impl Panel for NotificationCenterPanel {
    const ID: PanelId = PanelId("notification-center");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });
    const ANCHORS: &'static [&'static str] = &[ashd_shell_core::anchors::names::BELL];

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.notifications.state, |_, _, cx| cx.notify()).detach();
        let reply = cx.new(|cx| TextInput::new("Reply…", cx));
        cx.subscribe(&reply, |this: &mut Self, _, ev: &TextInputEvent, cx| this.on_reply_event(ev, cx)).detach();
        let state = NotificationCenterPanelState {
            focus: cx.focus_handle(),
            schedule: Schedule::default(),
            schedule_open: false,
            applied: None,
            timer: None,
            ticker: None,
            reply,
            reply_for: None,
        };
        // The schedule is read off the main thread; the timer starts once it is known.
        let path = schedule::path();
        let load = cx.background_executor().spawn(async move { schedule::load(&path) });
        cx.spawn(async move |this, cx| {
            let loaded = load.await;
            this.update(cx, |this, cx| {
                this.state.schedule = loaded;
                this.apply_schedule(cx);
                this.start_timer(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self { ctx, state }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Notifications { op } = request else { return None };
        let open = self.is_open(cx);
        match (op, open) {
            (PanelOp::Open, _) | (PanelOp::Toggle, false) => {
                let anchor = self.ctx.shell.request_anchor(cx);
                self.ctx.shell.open_modal(Self::ID, anchor, cx);
            }
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.focus.focus(window, cx);
        self.state.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_REFRESH).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.state.ticker = None;
        self.state.schedule_open = false;
        self.state.reply_for = None;
        cx.notify();
    }
}

// ── view ──────────────────────────────────────────────────────────────────────

/// Mono caps text (group names, counts) in `color`.
fn caps(text: &str, size: f32, color: gpui::Hsla, cx: &gpui::App) -> Div {
    let t = theme(cx);
    div()
        .flex_none()
        .font_family(t.font_mono.clone())
        .text_size(px(size))
        .line_height(px(size + 3.))
        .text_color(color)
        .child(SharedString::from(text.to_uppercase()))
}

impl NotificationCenterPanel {
    fn header(&self, empty: bool, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(26.))
            .child(ashd_ui::title("Notifications", cx))
            .when(!empty, |d| {
                d.child(
                    button_small("nc-clear-all", "Clear all", ButtonKind::Ghost, cx).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.ctx.services.notifications.handle.dismiss_all();
                            cx.notify();
                        },
                    )),
                )
            })
    }

    fn dnd_subtitle(&self, dnd: bool) -> String {
        let s = &self.state.schedule;
        match (s.enabled, dnd) {
            (true, true) if s.contains(Tod::now()) => format!("on until {}", s.end.label()),
            (true, _) => format!("scheduled {}", s.window_label()),
            (false, true) => "on · until turned off".into(),
            (false, false) => "off · click to schedule".into(),
        }
    }

    fn time_stepper(
        &self,
        id: &'static str,
        caption: &str,
        value: Tod,
        edit: fn(&mut Schedule, i32),
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let step = i32::from(STEP_MINUTES);
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(label(caption, cx))
            .child(icon_button_sized(ElementId::from((id, 0u32)), Icon::Minus, 24., 12., cx).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.edit_schedule(|s| edit(s, -step), cx)),
            ))
            .child(
                div()
                    .w(px(40.))
                    .flex()
                    .justify_center()
                    .font_family(t.font_mono.clone())
                    .text_size(px(12.))
                    .text_color(t.fg)
                    .child(value.label()),
            )
            .child(icon_button_sized(ElementId::from((id, 1u32)), Icon::Plus, 24., 12., cx).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.edit_schedule(|s| edit(s, step), cx)),
            ))
    }

    fn dnd_row(&self, dnd: bool, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let schedule = self.state.schedule;
        let top = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .p(px(10.))
            .child(icon(Icon::Pause, 14.).text_color(if dnd { t.accent } else { t.fg }))
            .child(
                div()
                    .id("nc-dnd-text")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .cursor_pointer()
                    .child(
                        div()
                            .font_family(t.font_sans.clone())
                            .text_size(px(metrics::type_scale::UI.0))
                            .text_color(t.fg)
                            .child("Do not disturb"),
                    )
                    .child(label(self.dnd_subtitle(dnd), cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.schedule_open = !this.state.schedule_open;
                        cx.notify();
                    })),
            )
            .child(toggle(
                "nc-dnd",
                dnd,
                {
                    let handle = self.ctx.services.notifications.handle.clone();
                    move |on, _, _| handle.set_dnd(*on)
                },
                cx,
            ));
        let mut row = div().mt(px(8.)).border_1().border_color(t.line).rounded(t.radius_control).child(top);
        if self.state.schedule_open {
            row = row.child(
                div()
                    .p(px(10.))
                    .border_t_1()
                    .border_color(t.line)
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_family(t.font_sans.clone())
                                    .text_size(px(metrics::type_scale::UI.0))
                                    .text_color(t.fg)
                                    .child("Turn on automatically"),
                            )
                            .child(toggle(
                                "nc-schedule",
                                schedule.enabled,
                                cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.edit_schedule(|s| s.enabled = on, cx);
                                }),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(self.time_stepper(
                                "nc-from",
                                "from",
                                schedule.start,
                                |s, d| s.start = s.start.shifted(d),
                                cx,
                            ))
                            .child(self.time_stepper("nc-to", "to", schedule.end, |s, d| s.end = s.end.shifted(d), cx)),
                    ),
            );
        }
        row
    }

    /// One notification: title + age (a close button on hover), body, progress, actions.
    fn item(&self, n: &Notification, first: bool, now: chrono::DateTime<Local>, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx).clone();
        let id = n.id;
        let critical = n.urgency == Urgency::Critical;
        let group: SharedString = format!("nc-item-{id}").into();
        let s2 = t.surface_2;

        let age = div()
            .relative()
            .flex_none()
            .child(
                caps(&age_label(n.received, now), 10., t.fg_muted, cx).group_hover(group.clone(), |s| s.opacity(0.)),
            )
            .child(
                div()
                    .id(ElementId::from(("nc-close", id)))
                    .absolute()
                    .right(px(-4.))
                    .top(px(-3.))
                    .size(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(t.radius_control)
                    .opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(t.line))
                    .child(icon(Icon::Close, 11.).text_color(t.fg_muted))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.dismiss(id, cx);
                    })),
            );

        let mut el = div()
            .id(ElementId::from(("nc-item", id)))
            .group(group)
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(10.))
            .py(px(8.))
            .rounded(t.radius_control)
            .when(first, |d| d.bg(s2))
            .when(!first, |d| d.hover(move |s| s.bg(s2)))
            .when(critical, |d| d.border_l_2().border_color(t.err))
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(t.font_sans.clone())
                            .text_size(px(metrics::type_scale::UI.0))
                            .line_height(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if critical { t.err } else { t.fg })
                            .child(SharedString::from(if n.summary.is_empty() { n.app_name.clone() } else { n.summary.clone() })),
                    )
                    .child(age),
            );
        if !n.body.is_empty() {
            el = el.child(
                div()
                    .font_family(t.font_sans.clone())
                    .text_size(px(12.))
                    .line_height(px(12. * 1.4))
                    .text_color(t.fg_muted)
                    .line_clamp(3)
                    .text_ellipsis()
                    .child(SharedString::from(n.body.clone())),
            );
        }
        if let Some(p) = n.progress {
            el = el.child(div().mt(px(2.)).child(progress(f32::from(p) / 100.0, metrics::PROGRESS_MID, cx)));
        }
        let mut buttons: Vec<Stateful<Div>> = Vec::new();
        if let Some(placeholder) = n.reply_placeholder.clone() {
            buttons.push(
                button_small(
                    ElementId::from(SharedString::from(format!("nc-{id}-reply"))),
                    "Reply",
                    ButtonKind::Secondary,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_reply(id, &placeholder, window, cx);
                })),
            );
        }
        let action_buttons: Vec<_> = n
            .button_actions()
            .enumerate()
            .map(|(ix, a)| {
                let key = a.key.clone();
                button_small(
                    ElementId::from(SharedString::from(format!("nc-{id}-action-{ix}"))),
                    a.label.clone(),
                    if ix == 0 && n.reply_placeholder.is_none() { ButtonKind::Secondary } else { ButtonKind::Ghost },
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.invoke(id, &key, cx);
                }))
            })
            .collect();
        buttons.extend(action_buttons);
        if !buttons.is_empty() {
            el = el.child(div().flex().gap(px(6.)).mt(px(6.)).children(buttons));
        }
        if self.state.reply_for == Some(id) {
            // Clicks inside the field must not reach the item (that would dismiss it).
            el = el.child(
                div()
                    .id(ElementId::from(("nc-reply", id)))
                    .mt(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_default()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(field(FieldState::Focus, cx).flex_1().min_w_0().child(div().flex_1().child(self.state.reply.clone())))
                    .child(
                        button_small(
                            ElementId::from(SharedString::from(format!("nc-{id}-send"))),
                            "Send",
                            ButtonKind::Primary,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.send_reply(cx);
                        })),
                    ),
            );
        }
        let n = n.clone();
        el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.activate(&n, cx)))
    }

    fn group(&self, ix: usize, group: &Group, last: bool, now: chrono::DateTime<Local>, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let ids: Vec<u32> = group.items.iter().map(|n| n.id).collect();
        let title = group.title().to_string();
        let header = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(4.))
            .pb(px(6.))
            .child(icon(group.items.first().map_or(Icon::Chat, group_icon), 13.).text_color(t.fg_muted))
            .child(caps(&title, 10.5, t.fg, cx))
            .child(caps(&group.items.len().to_string(), 10., t.fg_muted, cx))
            .child(div().flex_1())
            .child(
                div()
                    .id(ElementId::from(("nc-clear-group", ix as u32)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded(t.radius_control)
                    .cursor_pointer()
                    .hover({
                        let s2 = t.surface_2;
                        move |s| s.bg(s2)
                    })
                    .child(icon(Icon::Trash, 13.).text_color(t.fg_muted))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.clear_group(&ids, cx))),
            );
        let mut col = div().flex().flex_col().gap(px(2.)).py(px(12.)).child(header);
        if !last {
            col = col.border_b_1().border_color(t.line);
        }
        for (i, n) in group.items.iter().enumerate() {
            col = col.child(self.item(n, i == 0, now, cx));
        }
        col
    }
}

impl Render for NotificationCenterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let st = self.ctx.services.notifications.state.read(cx).clone();
        let dnd = st.dnd;
        let groups = group_by_app(&st.notifications);
        let empty = groups.is_empty();
        let now = Local::now();

        // ── placement: bottom = bar_h + POPUP_GAP, right edge on the bell ──
        let anchor = self.ctx.shell.modal_anchor(Self::ID, cx);
        let display = self.ctx.shell.modal_display(cx).map(|d| d.size());
        let output_w: f32 = display.map(|s| s.width.into()).unwrap_or(1920.0);
        let output_h: f32 = display.map(|s| s.height.into()).unwrap_or(1080.0);
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let bottom = f32::from(t.bar_h) + margin + metrics::POPUP_GAP;
        let width = metrics::NOTIFICATION_CENTER_W;
        let right_edge = anchor
            .map(|a| f32::from(a.origin.x) + f32::from(a.size.width))
            .unwrap_or(margin + metrics::STATUS_W - 90.0);
        let left = (right_edge - width).min(output_w - width - margin).max(margin);
        let max_h = (output_h - bottom - TOP_GAP - margin).max(200.0);

        let list = if empty {
            let (ic, hint) = if dnd {
                (Icon::Pause, "Do not disturb is on — new notifications arrive silently.")
            } else {
                (Icon::Check, "Nothing new. Notifications from your apps show up here.")
            };
            div()
                .h(px(EMPTY_H))
                .flex_none()
                .child(ashd_ui::empty_state(ic, "All caught up", hint, Vec::new(), cx))
                .into_any_element()
        } else {
            let count = groups.len();
            let mut col = div().px(px(16.)).pb(px(4.)).flex().flex_col();
            for (ix, g) in groups.iter().enumerate() {
                col = col.child(self.group(ix, g, ix + 1 == count, now, cx));
            }
            div().id("nc-list").min_h(px(0.)).overflow_y_scroll().child(col).into_any_element()
        };

        let panel = ashd_ui::overlay_panel(cx)
            .occlude()
            .track_focus(&self.state.focus)
            .key_context("NotificationCenter")
            .absolute()
            .left(px(left))
            .bottom(px(bottom))
            .w(px(width))
            .max_h(px(max_h))
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(t.fg)
            .child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(if empty { 0. } else { 4. }))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(self.header(empty, cx))
                    .child(self.dnd_row(dnd, cx)),
            )
            .child(list)
            .with_animation(
                "nc-in",
                anim::tween(motion::timing::SUBMENU_IN, anim::EASE_STANDARD),
                |el, d| el.opacity(d),
            );
        div().size_full().relative().child(panel)
    }
}
