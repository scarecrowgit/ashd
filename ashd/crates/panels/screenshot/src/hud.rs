//! The passive half of the screenshot panel: what stays on screen *after* the capture
//! overlay is gone — the delay countdown, the REC indicator and the result toast. The modal
//! [`crate::ScreenshotPanel`] owns the state ([`Hud`], an entity shared through a global) and
//! its timers; one [`ScreenshotHudPanel`] per display draws the parts that belong to its
//! output (mockup det-08: REC chip 30 high, `err` border, blinking 8 px dot, "REC 00:14"
//! mono, stop hint).

use std::{path::PathBuf, sync::Arc, time::Instant};

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FontWeight, Global, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Render, RenderImage, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window, div, img, px,
};
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, hit_area};
use ashd_tokens::{LayoutVariant, metrics};
use ashd_ui::{
    Icon,
    anim::{EASE_OUT_EXPO, EASE_OUT_QUAD, timing},
    fx::fade_slide,
    icon, label,
    presence::{PresenceList, Timing},
    theme,
};

use crate::capture::clock;

/// The countdown before a delayed capture.
#[derive(Clone, Debug, PartialEq)]
pub struct Countdown {
    pub output: String,
    pub left: u32,
    pub record: bool,
}

/// A recording in progress.
#[derive(Clone, Debug)]
pub struct Recording {
    pub output: String,
    pub started: Instant,
    /// Stop was requested; waiting for the recorder to finalize the file.
    pub stopping: bool,
}

/// What a toast says.
#[derive(Clone)]
pub enum ToastBody {
    Saved { path: PathBuf, thumb: Option<Arc<RenderImage>>, video: bool, detail: String },
    Failed { title: String, message: String },
}

#[derive(Clone)]
pub struct Toast {
    pub id: u64,
    pub output: String,
    pub body: ToastBody,
    /// The pointer is over it: the timer waits.
    pub hovered: bool,
}

/// State shared by the capture controller and the per-display HUD panels.
#[derive(Default)]
pub struct Hud {
    pub countdown: Option<Countdown>,
    pub recording: Option<Recording>,
    pub toast: Option<Toast>,
}

/// What the HUD asks the controller to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudEvent {
    StopRecording,
    CancelCountdown,
}

impl EventEmitter<HudEvent> for Hud {}

impl Hud {
    /// Anything to draw on `output`?
    pub fn visible_on(&self, output: &str) -> bool {
        self.countdown.as_ref().is_some_and(|c| c.output == output)
            || self.recording.as_ref().is_some_and(|r| r.output == output)
            || self.toast.as_ref().is_some_and(|t| t.output == output)
    }
}

struct HudGlobal(Entity<Hud>);

impl Global for HudGlobal {}

/// The shared [`Hud`] entity (created by whichever panel is built first).
pub fn shared(cx: &mut App) -> Entity<Hud> {
    if let Some(g) = cx.try_global::<HudGlobal>() {
        return g.0.clone();
    }
    let hud = cx.new(|_| Hud::default());
    cx.set_global(HudGlobal(hud.clone()));
    hud
}

/// Blink phase of the REC dot: lit for 600 ms, dim for 600 ms (the mockup's
/// `1.2s steps(1)` keyframes).
pub fn dot_lit(elapsed_ms: u128) -> bool {
    (elapsed_ms / 600).is_multiple_of(2)
}

/// What the HUD shows, kept while it leaves (the shared [`Hud`] state is already `None`).
#[derive(Clone)]
enum HudItem {
    Rec(Recording),
    Countdown(Countdown),
    Toast(Toast),
}

/// Passive overlay panel: one per display.
pub struct ScreenshotHudPanel {
    ctx: PanelContext,
    hud: Entity<Hud>,
    /// What is drawn: each part fades in and out (the passive surface stays until the last
    /// one has left).
    items: PresenceList<&'static str, HudItem>,
    /// The surface was live at the last render (to tell the host when the exit is over).
    was_live: bool,
}

impl ScreenshotHudPanel {
    /// The niri output name (else the display id) this instance lives on.
    fn output(&self, cx: &App) -> Option<String> {
        let d = self.ctx.display.as_ref()?;
        Some(self.ctx.shell.display(d.id, cx).unwrap_or_else(|| d.clone()).output_label())
    }
}

impl Panel for ScreenshotHudPanel {
    const ID: PanelId = PanelId("screenshot-hud");
    const PLACEMENT: Placement = Placement::Passive;

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let hud = shared(cx);
        cx.observe(&hud, |_, _, cx| cx.notify()).detach();
        let items = PresenceList::new(
            Timing::new(timing::ITEM_IN, EASE_OUT_EXPO),
            Timing::new(timing::ITEM_OUT, EASE_OUT_QUAD),
        )
        .animate_first();
        Self { ctx, hud, items, was_live: false }
    }

    fn is_live(&self, cx: &App) -> bool {
        // Parts that are still leaving keep the surface.
        !self.items.is_empty() || self.output(cx).is_some_and(|o| self.hud.read(cx).visible_on(&o))
    }
}

impl Render for ScreenshotHudPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let Some(output) = self.output(cx) else { return div() };
        let hud = self.hud.read(cx);
        let mut current: Vec<(&'static str, HudItem)> = Vec::new();
        if let Some(r) = hud.recording.clone().filter(|r| r.output == output) {
            current.push(("rec", HudItem::Rec(r)));
        }
        if let Some(c) = hud.countdown.clone().filter(|c| c.output == output) {
            current.push(("countdown", HudItem::Countdown(c)));
        }
        if let Some(x) = hud.toast.clone().filter(|x| x.output == output) {
            current.push(("toast", HudItem::Toast(x)));
        }
        let now = std::time::Instant::now();
        self.items.sync(now, current);
        let shown: Vec<_> = self.items.entries(now).to_vec();
        if self.items.is_animating(now) {
            window.request_animation_frame();
        }
        // The last part has left: tell the overlay host, which then drops the surface.
        let live = !shown.is_empty();
        if self.was_live && !live {
            self.ctx.shell.refresh_overlays(cx);
        }
        self.was_live = live;

        let margin = match t.layout {
            LayoutVariant::Floating => f32::from(t.gap),
            LayoutVariant::Attached => 0.0,
        };
        let scratch = self
            .ctx
            .shell
            .entity()
            .is_some_and(|s| s.read(cx).scratch_visible);
        // The mockup parks the REC chip 20 px from the top and 20 px left of the scratchpad.
        let right = 20.0 + if scratch { metrics::SCRATCH_W + margin } else { 0.0 };

        let mut root = div().size_full().relative().font_family(t.font_sans.clone()).text_color(t.fg);
        for entry in shown {
            let (fade, travel) = (entry.presence.value(now), entry.presence.movement(now));
            match entry.value {
                HudItem::Rec(rec) => {
                    let chip = self.rec_chip(&rec, cx);
                    root = root.child(fade_slide(div().absolute().top(px(20.)).right(px(right)).child(hit_area(chip)), fade, travel, -6.0));
                }
                HudItem::Countdown(c) => {
                    let chip = self.countdown_chip(&c, cx);
                    root = root.child(fade_slide(
                        div().absolute().top(px(20.)).left_0().right_0().flex().justify_center().child(hit_area(chip)),
                        fade,
                        travel,
                        -6.0,
                    ));
                }
                HudItem::Toast(toast) => {
                    // The result toast comes in from the right edge it sits at.
                    root = root.child(
                        div()
                            .absolute()
                            .right(px(right.max(14.0 + margin) - 16.0 * (1.0 - travel)))
                            .bottom(px(f32::from(t.bar_h) + margin + 14.0))
                            .opacity(fade)
                            .child(hit_area(self.toast_card(&toast, cx))),
                    );
                }
            }
        }
        root
    }
}

impl ScreenshotHudPanel {
    fn rec_chip(&self, rec: &Recording, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let t = theme(cx).clone();
        let elapsed = rec.started.elapsed();
        let lit = dot_lit(elapsed.as_millis());
        let hud_entity = self.hud.clone();
        div()
            .id("rec-chip")
            .h(px(metrics::CONTROL_H))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .bg(t.surface)
            .border_1()
            .border_color(t.err)
            .rounded(t.radius_control)
            .cursor_pointer()
            .on_click(move |_: &ClickEvent, _, cx| hud_entity.update(cx, |_, cx| cx.emit(HudEvent::StopRecording)))
            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(t.err).opacity(if lit && !rec.stopping { 1.0 } else { 0.2 }))
            .child(
                div()
                    .font_family(t.font_mono.clone())
                    .text_size(px(11.5))
                    .text_color(t.fg)
                    .child(SharedString::from(format!("REC {}", clock(elapsed.as_secs())))),
            )
            .child(label(if rec.stopping { "saving…" } else { "click to stop" }, cx))
    }

    fn countdown_chip(&self, c: &Countdown, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let t = theme(cx).clone();
        let hud_entity = self.hud.clone();
        div()
            .id("countdown-chip")
            .h(px(metrics::CONTROL_H))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(10.))
            .bg(t.surface)
            .border_1()
            .border_color(t.accent)
            .rounded(t.radius_control)
            .cursor_pointer()
            .on_click(move |_: &ClickEvent, _, cx| hud_entity.update(cx, |_, cx| cx.emit(HudEvent::CancelCountdown)))
            .child(icon(if c.record { Icon::Record } else { Icon::Camera }, metrics::icon_size::DENSE).text_color(t.accent))
            .child(
                div()
                    .font_family(t.font_mono.clone())
                    .text_size(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.fg)
                    .child(SharedString::from(c.left.to_string())),
            )
            .child(label(if c.record { "recording starts · click to cancel" } else { "capture · click to cancel" }, cx))
    }
}

impl ScreenshotHudPanel {
    fn toast_card(&self, toast: &Toast, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let t = theme(cx).clone();
        let failed = matches!(toast.body, ToastBody::Failed { .. });
        let shell = self.ctx.shell.clone();
        let hud = self.hud.clone();
        let hud2 = self.hud.clone();
        let card = div()
            .id(SharedString::from(format!("shot-toast-{}", toast.id)))
            .w(px(metrics::TOAST_W))
            .p(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .bg(t.surface)
            .border_1()
            .border_color(if failed { t.err } else { t.line })
            .rounded(t.radius_panel)
            .shadow(t.shadow_overlay())
            .on_hover(move |hovered, _, cx| {
                hud2.update(cx, |h, _| {
                    if let Some(x) = h.toast.as_mut() {
                        x.hovered = *hovered;
                    }
                });
            });
        match &toast.body {
            ToastBody::Saved { path, thumb, video, detail } => {
                let open_dir = path.parent().map(|p| p.to_string_lossy().into_owned());
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let preview = div()
                    .w(px(64.))
                    .h(px(40.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .border_1()
                    .border_color(t.line)
                    .rounded(t.radius_control)
                    .child(match thumb {
                        Some(image) => img(image.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element(),
                        None => icon(if *video { Icon::Video } else { Icon::Image }, 18.).text_color(t.fg_muted).into_any_element(),
                    });
                card.cursor_pointer()
                    .on_click(move |_: &ClickEvent, _, cx| {
                        if let Some(dir) = &open_dir {
                            shell.spawn(["xdg-open".to_string(), dir.clone()], cx);
                        }
                        hud.update(cx, |h, cx| {
                            h.toast = None;
                            cx.notify();
                        });
                        shell.refresh_overlays(cx);
                    })
                    .child(preview)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .flex_1()
                            .min_w_0()
                            .child(label(if *video { "recording saved" } else { "screenshot saved" }, cx))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.fg)
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(t.font_mono.clone())
                                    .text_size(px(10.5))
                                    .text_color(t.fg_muted)
                                    .child(SharedString::from(if detail.is_empty() { dir } else { format!("{detail} · {dir}") })),
                            ),
                    )
            }
            ToastBody::Failed { title, message } => card
                .child(div().flex_none().child(icon(Icon::AlertTriangle, 18.).text_color(t.err)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .flex_1()
                        .min_w_0()
                        .child(label(title.clone(), cx))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .line_height(px(17.))
                                .text_color(t.fg)
                                .line_clamp(3)
                                .child(SharedString::from(message.clone())),
                        ),
                ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dot_steps_every_600_ms() {
        assert!(dot_lit(0));
        assert!(dot_lit(599));
        assert!(!dot_lit(600));
        assert!(!dot_lit(1199));
        assert!(dot_lit(1200));
    }

    #[test]
    fn visibility_is_per_output() {
        let mut h = Hud::default();
        assert!(!h.visible_on("DP-1"));
        h.countdown = Some(Countdown { output: "DP-1".into(), left: 3, record: false });
        assert!(h.visible_on("DP-1"));
        assert!(!h.visible_on("HDMI-A-1"));
        h.countdown = None;
        h.recording = Some(Recording { output: "HDMI-A-1".into(), started: Instant::now(), stopping: false });
        assert!(h.visible_on("HDMI-A-1"));
    }
}
