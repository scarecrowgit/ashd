//! OSD (det-06): volume / mic / keyboard layout, bottom-centre above the dock,
//! `motion::timing::OSD_{IN,HOLD,OUT}` (100 / 1200 / 200 ms). Claims `Request::Osd`:
//! performs the change through the audio service handle and then shows the service's new
//! state; the layout chip is triggered by niri's keyboard-layout state, not by a request.
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
    AnimationExt as _, Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Task,
    Window, div, px, relative,
};
use ashd_ipc::{OsdRequest, Request, Response, VolumeChange};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement};
use ashd_svc_audio::{Device, Target};
use ashd_tokens::{metrics, motion::timing};
use ashd_ui::{Icon, anim, icon, theme};

use model::{Bar, Kind, brightness_bar, layout_code, volume_bar};

/// If the service has not reported the change this soon, show the OSD anyway (the step may
/// have been a no-op at a limit, or the service may not be running).
const REVEAL_FALLBACK: Duration = Duration::from_millis(150);

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
    /// Bumped on every hidden -> shown transition: keys the animations.
    epoch: u32,
    /// The bar as last drawn, to animate changes (`from`, `to`, change counter).
    bar: Option<(f32, f32, u32)>,
    hide: Option<Task<()>>,
    pending: Option<Pending>,
    /// Last keyboard layout name niri reported (the first one is not a change).
    layout: Option<String>,
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
            Kind::Layout { .. } => None,
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

    fn on_niri(&mut self, cx: &mut Context<Self>) {
        let now = self.ctx.niri.read(cx).current_layout().map(str::to_owned);
        let changed = matches!((&self.state.layout, &now), (Some(a), Some(b)) if a != b);
        self.state.layout = now.clone();
        if let (true, Some(name)) = (changed, now) {
            self.reveal(Kind::Layout { name }, cx);
        }
    }

    /// Show (or keep showing) the OSD: the first call fades it in, later calls only push the
    /// end of the hold back.
    fn reveal(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let was_visible = self.state.showing.is_some();
        if !was_visible {
            self.state.epoch += 1;
            self.state.bar = None;
        }
        self.state.leaving = false;
        self.state.showing = Some(kind);
        // Replacing the task cancels the previous hold.
        self.state.hide = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(timing::OSD_IN + timing::OSD_HOLD)).await;
            if this.update(cx, |this, cx| {
                this.state.leaving = true;
                cx.notify();
            })
            .is_err()
            {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(timing::OSD_OUT)).await;
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
        cx.observe(&ctx.niri, |this, _, cx| this.on_niri(cx)).detach();
        let layout = ctx.niri.read(cx).current_layout().map(str::to_owned);
        Self { ctx, state: OsdState { layout, ..OsdState::default() } }
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
                base.child(icon(Icon::Activity, 17.).text_color(if muted { t.fg } else { t.fg_muted }))
                    .child(div().flex_1().text_size(px(13.)).child(text))
            }
            Kind::Layout { name } => base
                .border_color(t.accent)
                .child(
                    div()
                        .h(px(22.))
                        .px(px(7.))
                        .flex()
                        .items_center()
                        .bg(t.accent)
                        .text_color(t.on_accent)
                        .font_family(t.font_mono.clone())
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(layout_code(name))),
                )
                .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).child(SharedString::from(name.clone()))),
        }
    }

    fn volume_card(&mut self, base: gpui::Div, bar: Bar, ic: Icon, cx: &mut Context<Self>) -> gpui::Div {
        let t = theme(cx).clone();
        // Animate the fill when the value changes while the OSD is up (EASE_STANDARD).
        let (from, to, change) = match self.state.bar {
            Some((from, to, n)) if (to - bar.fill).abs() > 1e-4 => {
                let _ = from;
                (to, bar.fill, n + 1)
            }
            Some(b) => b,
            None => (bar.fill, bar.fill, 0),
        };
        self.state.bar = Some((from, to, change));
        let fill_color = if bar.muted { t.fg_muted } else { t.accent };
        let fill = div().absolute().left_0().top_0().bottom_0().bg(fill_color);
        let fill = if from == to {
            fill.w(relative(to)).into_any_element()
        } else {
            fill.with_animation(
                SharedString::from(format!("osd-bar-{}-{change}", self.state.epoch)),
                anim::tween(timing::OSD_IN * 2, ashd_tokens::motion::EASE_STANDARD),
                move |el, p| el.w(relative(anim::lerp(from, to, p))),
            )
            .into_any_element()
        };
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
        base.text_color(tint)
            .child(icon(ic, 17.).text_color(tint))
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let Some(kind) = self.state.showing.clone() else { return div() };
        let margin = match t.layout {
            ashd_tokens::LayoutVariant::Floating => f32::from(t.gap),
            ashd_tokens::LayoutVariant::Attached => 0.0,
        };
        let epoch = self.state.epoch;
        let card = self.card(&kind, cx);
        let shown = if self.state.leaving {
            card.with_animation(
                SharedString::from(format!("osd-out-{epoch}")),
                anim::tween(timing::OSD_OUT, ashd_tokens::motion::EASE_STANDARD),
                |el, p| el.opacity(1.0 - p),
            )
            .into_any_element()
        } else {
            card.with_animation(
                SharedString::from(format!("osd-in-{epoch}")),
                anim::tween(timing::OSD_IN, ashd_tokens::motion::EASE_STANDARD),
                |el, p| el.opacity(p),
            )
            .into_any_element()
        };
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
