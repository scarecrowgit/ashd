//! Notification toasts stacked above the status bar (det-05). Every card is wrapped in
//! `ashd_shell_core::hit_area`; the rest of the passive overlay is click-through.
//!
//! Owner: notifications. Contract: ARCHITECTURE.md "Panels" — `ToastsPanel` is registered by
//! `ashd-shell` with `reg.register::<ToastsPanel>()`.
//!
//! The queue logic lives in [`model`] (no GPUI, unit-tested); this file is the view: the
//! card, the enter / exit / pulse animations, hover-to-pause, click handling, and the timer
//! task that wakes the queue when something is due.
//!
//! Inline reply is not drawn: the passive overlay has no keyboard, so a reply field here
//! could never take input (see `ashd_svc_notifications::ADVERTISE_INLINE_REPLY`).

mod model;

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, BoxShadow, ClickEvent, Context, MouseExitEvent, MouseMoveEvent, Div, FontWeight, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Render, RenderImage, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, Window, div, img, point, prelude::*, px,
};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, hit_area};
use ashd_svc_notifications::{NotificationImage, Urgency};
use ashd_tokens::{
    metrics,
    motion::{EASE_OUT_CUBIC, timing},
};
use ashd_ui::{Icon, anim, icon, progress, theme};

use model::{Queue, Toast, age_label};

/// Slide distance of the enter / exit animation (`translateX(-16px)`).
const SLIDE: f32 = 16.0;
/// Margin between the status bar and the lowest card, and from the left edge.
const EDGE: f32 = 10.0;
/// One pulse cycle of a critical card (`pulse 2.4s ease-out 2`).
const PULSE_MS: u64 = 2400;
/// Ring around a critical card at the top of its pulse (`0 0 0 4px` at 35 %).
const PULSE_RING: f32 = 4.0;
const PULSE_ALPHA: f32 = 0.35;
/// The age labels ("now", "5m") are refreshed at least this often while cards are up.
const AGE_REFRESH: Duration = Duration::from_secs(15);

/// Panel state (fm rule: everything this region needs lives here).
pub struct ToastsPanelState {
    queue: Queue,
    /// Decoded `image-data` pictures, by (id, seq).
    images: HashMap<(u32, u64), Arc<RenderImage>>,
    timer: Option<Task<()>>,
}

pub struct ToastsPanel {
    pub ctx: PanelContext,
    pub state: ToastsPanelState,
}

impl ToastsPanel {
    fn on_state(&mut self, cx: &mut Context<Self>) {
        let was_live = !self.state.queue.is_empty();
        let st = self.ctx.services.notifications.state.read(cx).clone();
        self.state.queue.sync(&st, Instant::now());
        for t in &self.state.queue.toasts {
            let key = (t.n.id, t.n.seq);
            if let std::collections::hash_map::Entry::Vacant(slot) = self.state.images.entry(key)
                && let Some(NotificationImage::Rgba { width, height, data }) = &t.n.image
                && let Some(img) = render_image(*width, *height, data)
            {
                slot.insert(img);
            }
        }
        self.after_change(was_live, cx);
    }

    fn on_timer(&mut self, cx: &mut Context<Self>) {
        let was_live = !self.state.queue.is_empty();
        for id in self.state.queue.tick(Instant::now()) {
            self.ctx.services.notifications.handle.expire(id);
        }
        self.after_change(was_live, cx);
    }

    /// Hover is tracked from mouse-move events and the surface's pointer-leave, not from
    /// GPUI's `on_hover`: the passive overlay keeps its last pointer position after the
    /// pointer left (no motion outside the input region), so a re-hit-test would keep
    /// reporting the card as hovered.
    fn set_hover(&mut self, id: u32, hovered: bool, cx: &mut Context<Self>) {
        if self.state.queue.is_hovered(id) == hovered {
            return;
        }
        self.state.queue.set_hover(id, hovered, Instant::now());
        self.after_change(true, cx);
    }

    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if self.state.queue.clear_hover(Instant::now()) {
            self.after_change(true, cx);
        }
    }

    /// The card was clicked: its default action, or just close it.
    fn activate(&mut self, id: u32, has_default: bool, cx: &mut Context<Self>) {
        let handle = &self.ctx.services.notifications.handle;
        if has_default {
            handle.invoke_action(id, "default");
        } else {
            handle.dismiss(id);
        }
        self.leave(id, cx);
    }

    fn invoke(&mut self, id: u32, key: &str, cx: &mut Context<Self>) {
        self.ctx.services.notifications.handle.invoke_action(id, key);
        self.leave(id, cx);
    }

    fn close(&mut self, id: u32, cx: &mut Context<Self>) {
        self.ctx.services.notifications.handle.dismiss(id);
        self.leave(id, cx);
    }

    fn leave(&mut self, id: u32, cx: &mut Context<Self>) {
        self.state.queue.begin_exit(id, Instant::now());
        self.after_change(true, cx);
    }

    /// Re-arm the timer, drop pictures nobody shows any more, wake the overlay host when
    /// the panel went from empty to showing (or back), repaint.
    fn after_change(&mut self, was_live: bool, cx: &mut Context<Self>) {
        let live = !self.state.queue.is_empty();
        let queue = &self.state.queue;
        self.state.images.retain(|(id, seq), _| queue.toasts.iter().any(|t| t.n.id == *id && t.n.seq == *seq));
        let now = Instant::now();
        self.state.timer = None;
        if live {
            let due = queue.next_deadline(now).unwrap_or(now + AGE_REFRESH).min(now + AGE_REFRESH);
            let wait = due.saturating_duration_since(now).max(Duration::from_millis(1));
            self.state.timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |this, cx| this.on_timer(cx)).ok();
            }));
        }
        if live != was_live {
            self.ctx.shell.refresh_overlays(cx);
        }
        cx.notify();
    }
}

impl Panel for ToastsPanel {
    const ID: PanelId = PanelId("toasts");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let seen = ctx.services.notifications.state.read(cx).last_seq;
        cx.observe(&ctx.services.notifications.state, |this, _, cx| this.on_state(cx)).detach();
        // Opening / closing the notification centre hides / brings back the toasts.
        if let Some(shell) = ctx.shell.entity() {
            let mut was_open = false;
            cx.observe(&shell, move |this: &mut Self, _, cx| {
                let open = this.centre_open(cx);
                if open != was_open {
                    was_open = open;
                    this.ctx.shell.refresh_overlays(cx);
                    cx.notify();
                }
            })
            .detach();
        }
        Self {
            ctx,
            state: ToastsPanelState { queue: Queue::new(seen), images: HashMap::new(), timer: None },
        }
    }

    fn is_live(&self, cx: &gpui::App) -> bool {
        !self.state.queue.is_empty() && !self.centre_open(cx)
    }
}

/// The notification centre (a modal): while it is open toasts stand aside — what they
/// would show is in its list, and a passive surface created before the modal would be
/// stacked *under* it (a critical toast half-hidden behind the centre). They come back
/// (those that did not time out meanwhile) when it closes.
const NOTIFICATION_CENTER: PanelId = PanelId("notification-center");

impl ToastsPanel {
    fn centre_open(&self, cx: &gpui::App) -> bool {
        self.ctx.shell.modal(cx).is_some_and(|m| m.panel == NOTIFICATION_CENTER)
    }
}

/// RGBA8 -> the BGRA frame GPUI draws.
fn render_image(width: u32, height: u32, rgba: &[u8]) -> Option<Arc<RenderImage>> {
    let mut bgra = rgba.to_vec();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

/// Card width: the compact det-12 card for network notices, else the det-05 one.
pub fn toast_width(category: Option<&str>) -> f32 {
    if category.is_some_and(|c| c.starts_with("network")) { metrics::TOAST_COMPACT_W } else { metrics::TOAST_W }
}

/// Freedesktop `*.error` / `*.warning` categories get the warn frame (det-12 "no internet").
pub fn is_warning(category: Option<&str>) -> bool {
    category.is_some_and(|c| c.ends_with(".error") || c.ends_with(".warning"))
}

/// The glyph in the 32x32 box when the app sent no picture: a ashd icon named by the
/// notification (shell notices send `ethernet`, `wifi_off` …), else a guess.
fn fallback_icon(t: &Toast) -> Icon {
    if let Some(NotificationImage::Name(name)) = &t.n.image
        && let Some(ic) = Icon::ALL.iter().copied().find(|ic| ic.path().strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")) == Some(name.as_str()))
    {
        return ic;
    }
    let hay = format!(
        "{} {} {}",
        t.n.app_name.to_lowercase(),
        t.n.category.as_deref().unwrap_or("").to_lowercase(),
        t.n.desktop_entry.as_deref().unwrap_or("").to_lowercase()
    );
    if hay.contains("battery") || hay.contains("power") {
        Icon::Battery
    } else if hay.contains("transfer") || hay.contains("download") {
        Icon::Download
    } else if hay.contains("network") || hay.contains("wifi") {
        Icon::Wifi
    } else if hay.contains("volume") || hay.contains("audio") || hay.contains("sound") {
        Icon::Volume
    } else {
        Icon::Chat
    }
}

/// Pulse envelope over one cycle: up in the first 10 %, held to 30 %, gone by 40 %.
fn pulse_ramp(t: f32) -> f32 {
    let ease = |x: f32| anim::ease_out_quad(x.clamp(0.0, 1.0));
    match t {
        t if t < 0.1 => ease(t / 0.1),
        t if t < 0.3 => 1.0,
        t if t < 0.4 => 1.0 - ease((t - 0.3) / 0.1),
        _ => 0.0,
    }
}

impl ToastsPanel {
    fn card(&self, toast: &Toast, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let id = toast.n.id;
        let n = &toast.n;
        let critical = n.urgency == Urgency::Critical;
        let hovered = toast.hovered;
        let has_default = n.has_default_action();

        // Shell notices (det-12): compact width, warn frame for `*.error` categories.
        let warn = !critical && is_warning(n.category.as_deref());
        // 32x32 icon box
        let tint = if critical {
            t.err
        } else if warn {
            t.warn
        } else {
            t.fg
        };
        let glyph = icon(fallback_icon(toast), 15.).text_color(tint).into_any_element();
        let picture = match &n.image {
            Some(NotificationImage::Path(p)) => Some(img(p.clone())),
            Some(NotificationImage::Rgba { .. }) => self.state.images.get(&(id, n.seq)).map(|i| img(i.clone())),
            _ => None,
        };
        let icon_box = div()
            .w(px(32.))
            .h(px(32.))
            .flex_none()
            .border_1()
            .border_color(t.line)
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .text_color(tint)
            .child(match picture {
                Some(pic) => {
                    let fallback = fallback_icon(toast);
                    pic.size_full().object_fit(ObjectFit::Contain).with_fallback(move || icon(fallback, 15.).into_any_element()).into_any_element()
                }
                None => glyph,
            });

        // header: APP NAME ......... age (or the close button on hover)
        let age = age_label(n.received, chrono::Local::now());
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .h(px(14.))
            .font_family(t.font_mono.clone())
            .text_size(px(metrics::type_scale::LABEL.0))
            .text_color(t.fg_muted)
            .child(div().flex_1().min_w_0().truncate().child(SharedString::from(n.app_name.to_uppercase())))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        div()
                            .child(SharedString::from(age.to_uppercase()))
                            .opacity(if hovered { 0. } else { 1. }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("toast-close-{id}")))
                            .absolute()
                            .right(px(-4.))
                            .top(px(-2.))
                            .w(px(18.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(t.radius_control)
                            .opacity(if hovered { 1. } else { 0. })
                            .cursor_pointer()
                            .hover({
                                let s2 = t.surface_2;
                                move |s| s.bg(s2).text_color(gpui::white())
                            })
                            .child(icon(Icon::Close, 11.).text_color(t.fg_muted))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.close(id, cx);
                            })),
                    ),
            );

        let mut text = div().flex().flex_col().gap(px(4.)).flex_1().min_w_0().child(header);
        if !n.summary.is_empty() {
            text = text.child(
                div()
                    .font_family(t.font_sans.clone())
                    .text_size(px(13.))
                    .line_height(px(17.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.fg)
                    .truncate()
                    .child(SharedString::from(n.summary.clone())),
            );
        }
        if !n.body.is_empty() {
            text = text.child(
                div()
                    .font_family(t.font_sans.clone())
                    .text_size(px(12.5))
                    .line_height(px(12.5 * 1.45))
                    .text_color(t.fg_muted)
                    .line_clamp(3)
                    .text_ellipsis()
                    .child(SharedString::from(n.body.clone())),
            );
        }
        if let Some(p) = n.progress {
            text = text.child(div().mt(px(2.)).child(progress(p as f32 / 100.0, metrics::PROGRESS_MID, cx)));
        }
        let buttons: Vec<Stateful<Div>> = n
            .button_actions()
            .enumerate()
            .map(|(ix, a)| {
                let key = a.key.clone();
                div()
                    .id(SharedString::from(format!("toast-{id}-action-{ix}")))
                    .h(px(metrics::BUTTON_SM_H))
                    .px(px(12.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(if ix == 0 { t.line } else { gpui::transparent_black() })
                    .rounded(t.radius_control)
                    .font_family(t.font_sans.clone())
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.fg)
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover({
                        let s2 = t.surface_2;
                        move |s| s.bg(s2)
                    })
                    .child(SharedString::from(a.label.clone()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.invoke(id, &key, cx);
                    }))
            })
            .collect();
        if !buttons.is_empty() {
            text = text.child(div().flex().gap(px(6.)).mt(px(6.)).children(buttons));
        }

        // timer bar (2px, shrinks linearly; frozen while hovered)
        let timer_bar = toast.bar_fraction().map(|from| {
            let track = div().w_full().h(px(metrics::PROGRESS_THIN)).bg(t.line);
            let fill = div().h_full().bg(t.accent);
            // The animation runs for the time left when the timer (re)started, not for the
            // time left now: its start is fixed by the generation in its id.
            match toast.remaining {
                Some(left) if toast.running_since.is_some() => track.child(
                    fill.with_animation(
                        SharedString::from(format!("toast-bar-{id}-{}", toast.generation)),
                        Animation::new(left),
                        move |el, p| el.w(gpui::relative(from * (1.0 - p))),
                    ),
                ),
                _ => track.child(fill.w(gpui::relative(from))),
            }
        });

        let ring_base = t.shadow_overlay();
        let err = t.err;
        let card = div()
            .id(SharedString::from(format!("toast-card-{id}")))
            .w(px(toast_width(n.category.as_deref())))
            .bg(t.surface)
            .border_1()
            .border_color(if critical {
                t.err
            } else if warn {
                t.warn
            } else {
                t.line
            })
            .rounded(t.radius_panel)
            .shadow(ring_base.clone())
            .overflow_hidden()
            .when(has_default, |d| d.cursor_pointer())
            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| this.set_hover(id, true, cx)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.activate(id, has_default, cx)))
            .child(div().p(px(14.)).flex().gap(px(12.)).child(icon_box).child(text))
            .children(timer_bar);

        if critical && !toast.exiting() {
            // Exactly two pulses, then the plain card.
            card.with_animations(
                SharedString::from(format!("toast-pulse-{id}")),
                vec![Animation::new(Duration::from_millis(PULSE_MS)); 2],
                move |el, _, p| {
                    let ramp = pulse_ramp(p);
                    let mut shadows = ring_base.clone();
                    shadows.push(BoxShadow {
                        color: ashd_ui::alpha(err, PULSE_ALPHA * ramp),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(PULSE_RING * ramp),
                        inset: false,
                    });
                    el.shadow(shadows)
                },
            )
            .into_any_element()
        } else {
            card.into_any_element()
        }
    }
}

impl Render for ToastsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        // Floating themes keep a margin around the bar.
        let margin = match t.layout {
            ashd_tokens::LayoutVariant::Floating => f32::from(t.gap),
            ashd_tokens::LayoutVariant::Attached => 0.0,
        };
        let shown: Vec<Toast> =
            if self.centre_open(cx) { Vec::new() } else { self.state.queue.display().into_iter().cloned().collect() };
        let queued = if self.centre_open(cx) { 0 } else { self.state.queue.queued() };
        let cards: Vec<_> = shown
            .iter()
            .map(|toast| {
                let id = toast.n.id;
                let card = self.card(toast, cx);
                let frame = div().relative().child(hit_area(card));
                if toast.exiting() {
                    frame
                        .with_animation(
                            SharedString::from(format!("toast-out-{id}")),
                            anim::tween(timing::TOAST_OUT, EASE_OUT_CUBIC),
                            |el, p| el.opacity(1.0 - p),
                        )
                        .into_any_element()
                } else {
                    frame
                        .with_animation(
                            SharedString::from(format!("toast-in-{id}")),
                            anim::tween(timing::TOAST_IN, EASE_OUT_CUBIC),
                            |el, p| el.opacity(p).left(px(-SLIDE * (1.0 - p))),
                        )
                        .into_any_element()
                }
            })
            .collect();

        let this = cx.entity();
        div().size_full().relative().child(
            gpui::canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let this = this.clone();
                    window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Bubble {
                            this.update(cx, |this, cx| this.clear_hover(cx));
                        }
                    });
                },
            )
            .absolute()
            .size_0(),
        ).child(
            div()
                .absolute()
                .left(px(EDGE + margin))
                .bottom(px(f32::from(t.bar_h) + margin + EDGE))
                // the widest card; compact cards keep their own width (left-aligned)
                .w(px(metrics::TOAST_W))
                .items_start()
                .flex()
                .flex_col()
                .gap(px(metrics::TOAST_GAP))
                .when(queued > 0, |d| d.child(ashd_ui::label(format!("+{queued} waiting"), cx)))
                .children(cards),
        )
    }
}
