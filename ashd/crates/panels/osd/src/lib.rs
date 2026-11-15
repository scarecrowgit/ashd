//! OSD (det-06): volume / mic / brightness, bottom-centre above the dock,
//! `motion::timing::OSD_{IN,HOLD,OUT}` (100 / 1200 / 200 ms). Claims `Request::Osd`:
//! performs the change through the audio service handle and then shows the service's new
//! state. (There is no keyboard-layout popup: the bar's layout chip is the indicator.)
//!
//! Owner: notifications. Not interactive, so nothing here needs `hit_area`: the passive
//! overlay is click-through.
//!
//! Brightness: `Osd(Brightness(delta))` steps all displays through the brightness service and
//! shows the first display's level (sliders icon, bar, value); with no controllable display
//! it answers `Response::Error("brightness unavailable")` and shows nothing.

mod model;

use std::time::Duration;

use gpui::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Task, Window, div, px, relative,
};
use ashd_ipc::{OsdRequest, Request, Response, VolumeChange};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement};
use ashd_svc_audio::{Device, Target};
use ashd_tokens::{metrics, motion::timing};
use ashd_ui::{
    Icon, anim::duration_ms, follow, icon,
    presence::{Presence, Timing},
    swap, theme,
};

use model::{Bar, Kind, brightness_bar, volume_bar};

/// If the service has not reported the change this soon, show the OSD anyway (the step may
/// have been a no-op at a limit, or the service may not be running).
const REVEAL_FALLBACK: Duration = Duration::from_millis(150);

fn osd_in() -> Timing {
    Timing::new(timing::OSD_IN, ashd_tokens::motion::EASE_STANDARD)
}

fn osd_out() -> Timing {
    Timing::new(timing::OSD_OUT, ashd_tokens::motion::EASE_STANDARD)
}

/// A request that is waiting for the audio service to report its effect.
struct Pending {
    kind: Kind,
    /// (volume, muted) of the device before the change.
    baseline: Option<(f32, bool)>,
    _fallback: Task<()>,
}

#[derive(Default)]
pub struct OsdState {
    /// What is shown, `None` when hidden.
    pub showing: Option<Kind>,
    /// Fading out (the hold is over).
    leaving: bool,
    /// How far it has faded in (100 ms in, 200 ms out, standard curve). A reveal while it
    /// leaves turns the fade around from where it is.
    presence: Option<Presence>,
    hide: Option<Task<()>>,
    pending: Option<Pending>,
}

pub struct OsdPanel {
    pub ctx: PanelContext,
    pub state: OsdState,
}

fn snapshot(d: Option<&Device>) -> Option<(f32, bool)> {
    d.map(|d| (d.volume, d.muted))
}

impl OsdPanel {
    fn device_snapshot(&self, kind: &Kind, cx: &gpui::App) -> Option<(f32, bool)> {
        let audio = self.ctx.services.audio.state.read(cx);
        match kind {
            Kind::Volume => snapshot(audio.default_sink()),
            Kind::Mic => snapshot(audio.default_source()),
            Kind::Brightness => self.ctx.services.brightness.state.read(cx).primary().map(|d| (d.value, false)),
        }
    }

    /// Ask the service for the change, then show its result once it arrives.
    fn request_change(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let baseline = self.device_snapshot(&kind, cx);
        let fallback_kind = kind.clone();
        let fallback = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REVEAL_FALLBACK).await;
            this.update(cx, |this, cx| {
                if this.state.pending.is_some() {
                    this.state.pending = None;
                    this.reveal(fallback_kind, cx);
                }
            })
            .ok();
        });
        self.state.pending = Some(Pending { kind, baseline, _fallback: fallback });
    }

    fn on_audio(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = &self.state.pending {
            let now = self.device_snapshot(&p.kind, cx);
            if now != p.baseline {
                let kind = p.kind.clone();
                self.state.pending = None;
                self.reveal(kind, cx);
                return;
            }
        }
        if self.state.showing.is_some() {
            cx.notify();
        }
    }

    /// Show (or keep showing) the OSD: the first call fades it in, later calls only push the
    /// end of the hold back.
    fn reveal(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let was_visible = self.state.showing.is_some();
        let now = std::time::Instant::now();
        match self.state.presence.as_mut() {
            Some(p) if was_visible => p.set_present(true, now),
            _ => self.state.presence = Some(Presence::entering(osd_in(), osd_out(), now)),
        }
        self.state.leaving = false;
        self.state.showing = Some(kind);
        // Replacing the task cancels the previous hold.
        self.state.hide = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(duration_ms(timing::OSD_IN) + timing::OSD_HOLD)).await;
            if this.update(cx, |this, cx| {
                this.state.leaving = true;
                if let Some(p) = this.state.presence.as_mut() {
                    p.set_present(false, std::time::Instant::now());
                }
                cx.notify();
            })
            .is_err()
            {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(duration_ms(timing::OSD_OUT))).await;
            this.update(cx, |this, cx| {
                this.state.showing = None;
                this.state.leaving = false;
                this.ctx.shell.refresh_overlays(cx);
                cx.notify();
            })
            .ok();
        }));
        if !was_visible {
            self.ctx.shell.refresh_overlays(cx);
        }
        cx.notify();
    }
}

impl Panel for OsdPanel {
    const ID: PanelId = PanelId("osd");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        cx.observe(&ctx.services.audio.state, |this, _, cx| this.on_audio(cx)).detach();
        // Same "did the value move" logic as audio: a brightness change reveals the OSD.
        cx.observe(&ctx.services.brightness.state, |this, _, cx| this.on_audio(cx)).detach();
        Self { ctx, state: OsdState::default() }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Osd(osd) = request else { return None };
        let audio = self.ctx.services.audio.handle.clone();
        match osd {
            OsdRequest::Volume(VolumeChange::Step(d)) => {
                self.request_change(Kind::Volume, cx);
                audio.step_volume(Target::DefaultSink, *d);
            }
            OsdRequest::Volume(VolumeChange::Mute) => {
                self.request_change(Kind::Volume, cx);
                audio.toggle_mute(Target::DefaultSink);
            }
            OsdRequest::MicMute => {
                self.request_change(Kind::Mic, cx);
                audio.toggle_mute(Target::DefaultSource);
            }
            OsdRequest::Brightness(d) => {
                if !self.ctx.services.brightness.state.read(cx).available {
                    return Some(Response::Error("brightness unavailable".into()));
                }
                self.request_change(Kind::Brightness, cx);
                self.ctx.services.brightness.handle.step(ashd_svc_brightness::Target::All, *d);
            }
        }
        Some(Response::Ok)
    }

    fn is_live(&self, _cx: &gpui::App) -> bool {
        self.state.showing.is_some()
    }
}

impl OsdPanel {
    /// The card of one variant.
    fn card(&mut self, kind: &Kind, cx: &mut Context<Self>) -> gpui::Div {
        let t = theme(cx).clone();
        let base = div()
            .w(px(metrics::OSD_W))
            .h(px(metrics::OSD_H))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(14.))
            .bg(t.surface)
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_panel)
            .shadow(t.shadow_overlay())
            .text_color(t.fg)
            .font_family(t.font_sans.clone());
        let audio = self.ctx.services.audio.state.read(cx);
        match kind {
            Kind::Volume => {
                let sink = audio.default_sink();
                let bar = volume_bar(sink.map_or(0.0, |d| d.volume), sink.is_some_and(|d| d.muted), sink.is_some());
                self.volume_card(base, bar, Icon::Volume, cx)
            }
            Kind::Brightness => {
                let b = self.ctx.services.brightness.state.read(cx);
                let bar = brightness_bar(b.primary().map_or(0.0, |d| d.value), b.primary().is_some());
                self.volume_card(base, bar, Icon::Sliders, cx)
            }
            Kind::Mic => {
                let muted = audio.default_source().is_some_and(|d| d.muted);
                let text = if muted { "Microphone muted" } else { "Microphone on" };
                // on <-> muted: glyph and words dip and come back (120 ms)
                let glyph_color = if muted { t.fg } else { t.fg_muted };
                base.child(swap("osd-mic", muted, move |p| {
                    div().flex().flex_1().items_center().gap(px(14.)).opacity(p).child(icon(Icon::Activity, 17.).text_color(glyph_color)).child(
                        div().flex_1().text_size(px(13.)).child(text),
                    )
                }))
            }
        }
    }

    fn volume_card(&mut self, base: gpui::Div, bar: Bar, ic: Icon, cx: &mut Context<Self>) -> gpui::Div {
        let t = theme(cx).clone();
        // The fill follows the value while the OSD is up (200 ms standard), from wherever it
        // is when the next step arrives (a held key keeps retargeting it).
        let fill_color = if bar.muted { t.fg_muted } else { t.accent };
        let fill = follow(
            "osd-fill",
            bar.fill,
            Timing::new(timing::OSD_IN * 2, ashd_tokens::motion::EASE_STANDARD),
            move |v| div().absolute().left_0().top_0().bottom_0().w(relative(v)).bg(fill_color),
        );
        let track = div()
            .relative()
            .flex_1()
            .h(px(metrics::SLIDER_TRACK_H))
            .bg(t.line)
            .child(fill)
            .children(bar.tick.map(|x| {
                div().absolute().left(relative(x)).top(px(-3.)).bottom(px(-3.)).w(px(1.)).bg(t.fg)
            }));
        let value_color = if bar.boost { t.warn } else { t.fg };
        let tint = if bar.muted { t.fg_muted } else { t.fg };
        // The glyph cross-fades when the variant (volume / brightness) or the muted state changes.
        let glyph = swap("osd-glyph", (ic as u32, bar.muted), move |p| div().flex().flex_none().opacity(p).child(icon(ic, 17.).text_color(tint)));
        base.text_color(tint)
            .child(glyph)
            .child(track)
            .child(
                div()
                    .font_family(t.font_mono.clone())
                    .text_size(px(12.))
                    .text_color(if bar.muted { t.fg } else { value_color })
                    .child(SharedString::from(bar.text)),
            )
    }
}

impl Render for OsdPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let Some(kind) = self.state.showing.clone() else { return div() };
        let margin = match t.layout {
            ashd_tokens::LayoutVariant::Floating => f32::from(t.gap),
            ashd_tokens::LayoutVariant::Attached => 0.0,
        };
        let now = std::time::Instant::now();
        let fade = self.state.presence.map_or(1.0, |p| p.value(now));
        if self.state.presence.is_some_and(|p| p.is_animating(now)) {
            window.request_animation_frame();
        }
        // switching variant (volume -> mic -> brightness) while it is up: the card dips and returns
        let card = self.card(&kind, cx);
        let shown = swap("osd-card", std::mem::discriminant(&kind), move |p| card.opacity(fade * p));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .pb(px(f32::from(t.bar_h) + margin + (metrics::OSD_BOTTOM - metrics::BAR_H)))
            .child(shown)
    }
}
