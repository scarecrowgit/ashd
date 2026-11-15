//! Media popup (det-11): the active MPRIS player's card.
//!
//! Owner: media. Contract: ARCHITECTURE.md "Panels" — `MediaPanel` is registered by
//! `ashd-shell` with `reg.register::<MediaPanel>()`.
//!
//! A 340 px card growing up from the status bar: art (72×72, the real cover when the
//! player gives a local file, else a placeholder), title / CAPS subtitle, a 3 px progress
//! bar with mono times (click or drag to seek when the player can), prev / play-pause /
//! next, the queue when the player has a `TrackList`, and a chip per player to switch the
//! active one. Space toggles play/pause, Left / Right skip, Escape closes (the overlay's
//! `ModalDismiss`).
//!
//! `Request::Media { op }` opens / closes the card; `Request::MediaControl` (media keys:
//! `ashd media play-pause|next|previous|stop`) is answered even while it is closed. The
//! service polls the playback position only while the card is open (see
//! [`ashd_svc_mpris::Handle::set_position_polling`]).

mod format;

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, Bounds, Context, Div, ElementId, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _,
    Pixels, Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, img,
    prelude::*, px, relative,
};
use ashd_ipc::{MediaControl, PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_mpris::{MprisState, Player, PlaybackStatus};
use ashd_tokens::{LayoutVariant, metrics};
use ashd_ui::{Icon, hover_wash, icon, label, theme};

// Card width (det-11: 340): the shared metric.
use ashd_tokens::metrics::MEDIA_W as CARD_W;
/// Queue rows shown ("up next", starting at the current track).
const QUEUE_ROWS: usize = 5;

/// Panel state (fm rule: everything this region needs lives here).
pub struct MediaPanelState {
    focus: FocusHandle,
    /// Where the progress bar was last laid out (window coordinates), for pointer maths.
    bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Fraction under the pointer while the bar is being dragged; the seek is sent on release.
    drag: Option<f32>,
}

pub struct MediaPanel {
    pub ctx: PanelContext,
    pub state: MediaPanelState,
}

impl Panel for MediaPanel {
    const ID: PanelId = PanelId("media");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });
    const ANCHORS: &'static [&'static str] = &[ashd_shell_core::anchors::names::MEDIA, ashd_shell_core::anchors::names::CLOCK];

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.mpris.state, |_, _, cx| cx.notify()).detach();
        // Nobody looks at the position while the card is closed: no polling until it opens.
        ctx.services.mpris.handle.set_position_polling(false);
        let state = MediaPanelState { focus: cx.focus_handle(), bar: Rc::new(Cell::new(None)), drag: None };
        Self { ctx, state }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        match request {
            Request::Media { op } => {
                let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
                match (op, open) {
                    (PanelOp::Open, _) | (PanelOp::Toggle, false) => self.ctx.shell.open_modal(Self::ID, None, cx),
                    (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
                }
                Some(Response::Ok)
            }
            Request::MediaControl(key) => {
                let state = self.ctx.services.mpris.state.read(cx);
                if state.active_player().is_none() {
                    return Some(Response::error("no active media player"));
                }
                let handle = &self.ctx.services.mpris.handle;
                match key {
                    MediaControl::PlayPause => handle.play_pause(),
                    MediaControl::Next => handle.next(),
                    MediaControl::Previous => handle.previous(),
                    MediaControl::Stop => handle.stop(),
                }
                Some(Response::Ok)
            }
            _ => None,
        }
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.drag = None;
        self.state.focus.focus(window, cx);
        self.ctx.services.mpris.handle.set_position_polling(true);
    }

    fn on_close(&mut self, _cx: &mut Context<Self>) {
        self.state.drag = None;
        self.ctx.services.mpris.handle.set_position_polling(false);
    }
}

// ── small view helpers ────────────────────────────────────────────────────────

/// Which kind of transport button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ghost,
    Primary,
}

/// A 32×32 transport button; disabled ones are dimmed and inert (no `on_click` should be
/// added by the caller when `enabled` is false).
fn transport_button(id: &'static str, ic: Icon, kind: Kind, enabled: bool, cx: &gpui::App) -> Stateful<Div> {
    let t = theme(cx).clone();
    let (fg, hover_bg) = match kind {
        Kind::Primary => (t.on_accent, t.accent),
        Kind::Ghost => (t.fg, t.surface_2),
    };
    div()
        .id(id)
        .size(px(metrics::ICON_BUTTON))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(t.radius_control)
        .border_1()
        .border_color(if kind == Kind::Primary { t.accent } else { gpui::transparent_black() })
        .when(kind == Kind::Primary, |d| d.bg(t.accent))
        .when(!enabled, |d| d.opacity(0.4))
        .when(enabled, |d| {
            let wash = if kind == Kind::Primary { ashd_ui::mix(t.accent, gpui::black(), 0.12) } else { hover_bg };
            d.cursor_pointer().child(hover_wash(wash, None, t.radius_control))
        })
        .child(icon(ic, 14.).text_color(fg))
}

/// The art cell: the cover if there is a local file, else the music glyph. 72×72, 1px line.
fn art(player: Option<&Player>, cx: &gpui::App) -> Div {
    let t = theme(cx).clone();
    let muted = t.fg_muted;
    let placeholder = move || icon(Icon::Music, 26.).text_color(muted).into_any_element();
    let inner: AnyElement = match player.and_then(|p| p.track.art_path.clone()) {
        Some(path) => img(path)
            .size_full()
            .object_fit(ObjectFit::Cover)
            .with_fallback(move || div().size_full().flex().items_center().justify_center().child(placeholder()).into_any_element())
            .into_any_element(),
        None => div().size_full().flex().items_center().justify_center().child(placeholder()).into_any_element(),
    };
    div()
        .size(px(72.))
        .flex_none()
        .border_1()
        .border_color(t.line)
        .rounded(t.radius_control)
        .overflow_hidden()
        .text_color(t.fg_muted)
        .child(inner)
}

/// Mono time readout (11 px, muted).
fn time_text(text: String, cx: &gpui::App) -> Div {
    let t = theme(cx);
    div().font_family(t.font_mono.clone()).text_size(px(11.)).text_color(t.fg_muted).child(SharedString::from(text))
}

/// A truncating CAPS label that may shrink (`ashd_ui::label` is `flex_none`).
fn subtitle_label(text: String, cx: &gpui::App) -> Div {
    label(text, cx).flex_shrink(1.).min_w_0().max_w_full().truncate()
}

impl MediaPanel {
    fn seek_fraction(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if let Some(b) = self.state.bar.get() {
            self.state.drag = Some(format::fraction_at(f32::from(x), f32::from(b.left()), f32::from(b.size.width)));
            cx.notify();
        }
    }

    /// Mouse down on the bar: start dragging if the active player can seek.
    fn begin_seek(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let state = self.ctx.services.mpris.state.read(cx);
        if state.active_player().is_some_and(|p| p.can_seek && p.track.length.is_some()) {
            self.seek_fraction(x, cx);
        }
    }

    fn drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if self.state.drag.is_some() {
            self.seek_fraction(x, cx);
        }
    }

    /// Mouse released: send the seek.
    fn end_seek(&mut self, cx: &mut Context<Self>) {
        let Some(fraction) = self.state.drag.take() else { return };
        if let Some(len) = self.ctx.services.mpris.state.read(cx).active_player().and_then(|p| p.track.length) {
            self.ctx.services.mpris.handle.seek_to(len.mul_f32(fraction));
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let handle = &self.ctx.services.mpris.handle;
        let active = self.ctx.services.mpris.state.read(cx).active_player().cloned();
        let Some(p) = active else { return };
        match ev.keystroke.key.as_str() {
            "space" => handle.play_pause(),
            "left" if p.can_go_previous => handle.previous(),
            "right" if p.can_go_next => handle.next(),
            _ => {}
        }
    }

    fn header(&self, player: Option<&Player>, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let (title, subtitle, muted) = match player {
            Some(p) if !p.track.is_blank() => (format::title(&p.track), format::subtitle(p), false),
            Some(p) => ("Nothing playing".to_string(), p.display_name().to_string(), true),
            None => ("Nothing playing".to_string(), "no player running".to_string(), true),
        };
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(art(player, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .truncate()
                            .font_family(t.font_sans.clone())
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if muted { t.fg_muted } else { t.fg })
                            .child(SharedString::from(title)),
                    )
                    .child(subtitle_label(subtitle, cx)),
            )
    }

    fn progress(&self, p: &Player, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let length = p.track.length;
        let can_seek = p.can_seek && length.is_some();
        let fraction = self.state.drag.unwrap_or_else(|| p.progress());
        let elapsed = match (self.state.drag, length) {
            (Some(f), Some(len)) => len.mul_f32(f),
            _ => p.position.min(length.unwrap_or(p.position)),
        };
        let bar = self.state.bar.clone();
        let track = div()
            .relative()
            .w_full()
            .h(px(metrics::PROGRESS_MID))
            .bg(t.line)
            .child(div().h_full().w(relative(fraction)).bg(t.accent))
            .child(canvas(move |bounds, _, _| bar.set(Some(bounds)), |_, _, _, _| {}).absolute().size_full());
        // The hit area is taller than the 3 px bar: easier to grab.
        let hit = div()
            .id("media-seek")
            .w_full()
            .h(px(11.))
            .flex()
            .items_center()
            .when(can_seek, |d| d.cursor_pointer())
            .child(track)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.begin_seek(ev.position.x, cx)),
            );
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(hit)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(time_text(format::clock(elapsed), cx))
                    .child(time_text(length.map_or_else(|| "--:--".to_string(), format::clock), cx)),
            )
    }

    fn controls(&self, p: &Player, cx: &mut Context<Self>) -> Div {
        let playing = p.status == PlaybackStatus::Playing;
        let can_toggle = p.can_control && if playing { p.can_pause } else { p.can_play };
        let mut prev = transport_button("media-prev", Icon::SkipBack, Kind::Ghost, p.can_go_previous, cx);
        if p.can_go_previous {
            prev = prev.on_click(cx.listener(|this, _, _, _| this.ctx.services.mpris.handle.previous()));
        }
        let mut toggle = transport_button(
            "media-toggle",
            if playing { Icon::Pause } else { Icon::Play },
            Kind::Primary,
            can_toggle,
            cx,
        );
        if can_toggle {
            toggle = toggle.on_click(cx.listener(|this, _, _, _| this.ctx.services.mpris.handle.play_pause()));
        }
        let mut next = transport_button("media-next", Icon::SkipForward, Kind::Ghost, p.can_go_next, cx);
        if p.can_go_next {
            next = next.on_click(cx.listener(|this, _, _, _| this.ctx.services.mpris.handle.next()));
        }
        div().flex().justify_center().gap(px(10.)).child(prev).child(toggle).child(next)
    }

    fn queue(&self, p: &Player, cx: &mut Context<Self>) -> Option<Div> {
        if !p.has_track_list || p.queue.is_empty() {
            return None;
        }
        let t = theme(cx).clone();
        let current = p.queue_index();
        let (start, count) = format::queue_window(p.queue.len(), current, QUEUE_ROWS);
        let rows = p.queue.iter().enumerate().skip(start).take(count).map(|(i, track)| {
            let is_current = Some(i) == current;
            let s2 = t.surface_2;
            let id = track.id.clone();
            let mut row = div()
                .id(ElementId::from(("media-queue", i as u32)))
                .h(px(28.))
                .flex()
                .items_center()
                .gap(px(10.))
                .font_family(t.font_sans.clone())
                .text_size(px(metrics::type_scale::UI.0))
                .text_color(if is_current { t.accent } else { t.fg })
                .child(hover_wash(s2, None, t.radius_control))
                .child(
                    div()
                        .font_family(t.font_mono.clone())
                        .text_size(px(10.5))
                        .text_color(t.fg_muted)
                        .child(SharedString::from(format::index_label(i))),
                )
                .child(div().flex_1().min_w_0().truncate().child(SharedString::from(format::title(track))))
                .child(time_text(track.length.map_or_else(String::new, format::clock), cx));
            if let Some(id) = id {
                row = row.cursor_pointer().on_click(cx.listener(move |this, _, _, _| this.ctx.services.mpris.handle.go_to(id.clone())));
            }
            row
        });
        Some(
            div()
                .flex()
                .flex_col()
                .pt(px(10.))
                .border_t_1()
                .border_color(t.line)
                .child(label(format!("queue · {}", chip_name(p)), cx))
                .children(rows),
        )
    }

    fn chips(&self, state: &MprisState, cx: &mut Context<Self>) -> Option<Div> {
        if state.players.len() < 2 {
            return None;
        }
        let t = theme(cx).clone();
        let labels = format::chip_labels(&state.players);
        let chips = state.players.iter().zip(labels).enumerate().map(|(i, (p, text))| {
            let active = state.active.as_deref() == Some(p.bus_name.as_str());
            let pinned = active && state.pinned.is_some();
            let (fg, border) = if active { (t.accent, t.accent) } else { (t.fg_muted, t.line) };
            let hover_fg = t.fg;
            let bus = p.bus_name.clone();
            div()
                .id(ElementId::from(("media-chip", i as u32)))
                .h(px(22.))
                .px(px(8.))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .border_1()
                .border_color(border)
                .rounded(t.radius_control)
                .font_family(t.font_mono.clone())
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .cursor_pointer()
                .when(!active, |d| d.hover(move |s| s.text_color(hover_fg)))
                .when(pinned, |d| d.child(div().size(px(5.)).flex_none().rounded_full().bg(fg)))
                .child(SharedString::from(text))
                .on_click(cx.listener(move |this, _, _, _| {
                    let handle = &this.ctx.services.mpris.handle;
                    // Clicking the pinned, active chip lets go of the pin.
                    if active && pinned { handle.unpin() } else { handle.set_active(bus.clone()) }
                }))
        });
        Some(div().flex().flex_wrap().gap(px(6.)).children(chips))
    }
}

/// `mpd` for the "queue · mpd" label.
fn chip_name(p: &Player) -> String {
    format::chip_labels(std::slice::from_ref(p)).remove(0)
}

impl Render for MediaPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.ctx.services.mpris.state.read(cx).clone();
        let player = state.active_player().cloned();

        // ── placement: bottom = bar_h + POPUP_GAP above the status bar, left from the anchor ──
        let anchor = self.ctx.shell.modal_anchor(Self::ID, cx);
        let output_w: f32 = self.ctx.shell.modal_display(cx).map(|d| d.size().width.into()).unwrap_or(1920.0);
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let bottom = f32::from(t.bar_h) + margin + metrics::POPUP_GAP;
        let left = anchor.map(|a| f32::from(a.origin.x)).unwrap_or(margin);
        let left = left.min(output_w - CARD_W - margin).max(margin);

        let mut body = div().p(px(16.)).flex().flex_col().gap(px(14.)).child(self.header(player.as_ref(), cx));
        if let Some(p) = &player {
            body = body.child(self.progress(p, cx)).child(self.controls(p, cx));
            if let Some(queue) = self.queue(p, cx) {
                body = body.child(queue);
            }
        }
        if let Some(chips) = self.chips(&state, cx) {
            body = body.child(chips);
        }

        let card = ashd_ui::overlay_panel(cx)
            .occlude()
            .track_focus(&self.state.focus)
            .key_context("Media")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.on_key(ev, cx)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| this.drag_to(ev.position.x, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_seek(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_seek(cx)))
            .absolute()
            .left(px(left))
            .bottom(px(bottom))
            .w(px(CARD_W))
            .font_family(t.font_sans.clone())
            .text_color(t.fg)
            .child(body);
        // The root also follows a drag that leaves the card.
        div()
            .size_full()
            .relative()
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| this.drag_to(ev.position.x, cx)))
            .child(card)
    }
}

