//! The capture overlay's layout (det-08): the frozen still as the backdrop, a 58 % dim
//! everywhere except the selection (four strips around it — GPUI has no clipped shadow), a
//! 1 px accent outline, four 8×8 accent corner handles, the size chip above the selection,
//! and the toolbar at the bottom centre (bottom 80).

use gpui::{
    AnyElement, Context, CursorStyle, Div, FontWeight, InteractiveElement as _, IntoElement, MouseButton, ObjectFit,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px,
};
use ashd_ipc::CaptureMode;
use ashd_tokens::metrics;
use ashd_ui::{Icon, Segment, alpha, keycap, label, segmented_icons, theme};

use crate::{
    ScreenshotPanel,
    capture::{Handle, Kind, Rect, label_pos, mode_from_index, mode_index, size_label},
};

/// Dim outside the selection (the mockup's `rgba(0,0,0,.58)`; colour from the scrim token).
const DIM_ALPHA: f32 = 0.58;
/// Corner handle size.
const HANDLE: f32 = 8.0;
/// Toolbar distance from the bottom edge.
const TOOLBAR_BOTTOM: f32 = 80.0;

fn place(r: Rect) -> Div {
    div().absolute().left(px(r.x)).top(px(r.y)).w(px(r.w.max(0.0))).h(px(r.h.max(0.0)))
}

fn handle_cursor(h: Handle) -> CursorStyle {
    match h {
        Handle::N | Handle::S => CursorStyle::ResizeUpDown,
        Handle::E | Handle::W => CursorStyle::ResizeLeftRight,
        Handle::NW | Handle::SE => CursorStyle::ResizeUpLeftDownRight,
        Handle::NE | Handle::SW => CursorStyle::ResizeUpRightDownLeft,
    }
}

impl Render for ScreenshotPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.overlay(cx)
    }
}

impl ScreenshotPanel {
    fn overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let Some(s) = self.session.as_ref() else { return div().into_any_element() };
        let (bounds, mode) = (s.bounds, self.prefs.mode);

        // What is highlighted: the region being dragged, the window under the pointer, or
        // the whole output.
        let target: Option<Rect> = match mode {
            CaptureMode::Region => s.selection.filter(|r| r.w > 0.0 || r.h > 0.0),
            CaptureMode::Window => s.hover_window().map(|w| w.rect),
            CaptureMode::Output => Some(Rect::full(bounds)),
        };
        let dim = alpha(t.scrim(), DIM_ALPHA);
        let (w, h) = (bounds.w, bounds.h);

        let mut root = div()
            .size_full()
            .relative()
            .overflow_hidden()
            .occlude()
            .track_focus(&self.focus)
            .key_context("Screenshot")
            .font_family(t.font_sans.clone())
            .text_color(t.fg)
            .cursor(if mode == CaptureMode::Region { CursorStyle::Crosshair } else { CursorStyle::PointingHand })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_key_down(cx.listener(Self::on_key_down))
            // the frozen still, drawn at the output's logical size
            .child(
                img(s.still.image.clone())
                    .absolute()
                    .left_0()
                    .top_0()
                    .w(px(w))
                    .h(px(h))
                    .object_fit(ObjectFit::Fill),
            );

        // dim: everywhere but the highlighted rectangle
        let strips: Vec<Rect> = match target {
            None => vec![Rect::full(bounds)],
            Some(r) if mode == CaptureMode::Output => {
                let _ = r;
                Vec::new()
            }
            Some(r) => vec![
                Rect::new(0.0, 0.0, w, r.y),
                Rect::new(0.0, r.bottom(), w, h - r.bottom()),
                Rect::new(0.0, r.y, r.x, r.h),
                Rect::new(r.right(), r.y, w - r.right(), r.h),
            ],
        };
        for strip in strips.into_iter().filter(|r| r.w > 0.0 && r.h > 0.0) {
            root = root.child(place(strip).bg(dim));
        }

        if let Some(r) = target {
            // 1px outline just outside the rectangle (the mockup's `outline`)
            root = root.child(place(Rect::new(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0)).border_1().border_color(t.accent));
            if mode == CaptureMode::Region {
                // the inside moves the selection; invisible edge bands carry the resize
                // cursors; the corners are the 8x8 handles
                if r.is_usable() {
                    root = root.child(place(r).cursor(CursorStyle::OpenHand));
                    for hd in [Handle::N, Handle::E, Handle::S, Handle::W] {
                        let c = hd.point(&r);
                        let band = if matches!(hd, Handle::N | Handle::S) {
                            Rect::new(r.x, c.y - 4.0, r.w, 8.0)
                        } else {
                            Rect::new(c.x - 4.0, r.y, 8.0, r.h)
                        };
                        root = root.child(place(band).cursor(handle_cursor(hd)));
                    }
                }
                for hd in Handle::CORNERS {
                    let c = hd.point(&r);
                    root = root.child(
                        place(Rect::new(c.x - HANDLE / 2.0, c.y - HANDLE / 2.0, HANDLE, HANDLE))
                            .bg(t.accent)
                            .cursor(handle_cursor(hd)),
                    );
                }
            }
            if mode != CaptureMode::Output {
                let p = label_pos(r);
                root = root.child(
                    div()
                        .absolute()
                        .left(px(p.x))
                        .top(px(p.y))
                        .h(px(22.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .bg(t.accent)
                        .text_color(t.on_accent)
                        .font_family(t.font_mono.clone())
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .child(SharedString::from(size_label(r))),
                );
            }
        }

        root.child(self.toolbar(cx)).into_any_element()
    }

    fn toolbar(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let prefs = self.prefs;
        let s_has_selection = self.session.as_ref().is_some_and(|s| s.selection.is_some_and(|r| r.is_usable()));
        let hint = match prefs.mode {
            CaptureMode::Region if s_has_selection => "capture selection",
            CaptureMode::Region => "drag to select",
            CaptureMode::Window if self.session.as_ref().is_some_and(|s| s.windows_unavailable) => "click: focused window",
            CaptureMode::Window => "click a window",
            CaptureMode::Output => "click to capture",
        };
        let no_recorder = prefs.kind == Kind::Record && self.recorder_missing();

        let modes = segmented_icons(
            "shot-mode",
            [
                Segment::new("Region").icon(Icon::Crop),
                Segment::new("Window").icon(Icon::Window),
                Segment::new("Output").icon(Icon::Monitor),
            ],
            mode_index(prefs.mode),
            cx.listener(|this, ix: &usize, _, cx| this.set_mode(mode_from_index(*ix), cx)),
            cx,
        );
        let kinds = segmented_icons(
            "shot-kind",
            [Segment::new("Shot").icon(Icon::Image), Segment::new("Record").icon(Icon::Play)],
            usize::from(prefs.kind == Kind::Record),
            cx.listener(|this, ix: &usize, _, cx| this.set_kind(if *ix == 0 { Kind::Shot } else { Kind::Record }, cx)),
            cx,
        );
        let delay = div()
            .id("shot-delay")
            .cursor_pointer()
            .rounded(t.radius_control)
            .px(px(4.))
            .py(px(3.))
            .hover({
                let s2 = t.surface_2;
                move |s| s.bg(s2)
            })
            .on_click(cx.listener(|this, _, _, cx| this.cycle_delay(cx)))
            .child(label(prefs.delay.label(), cx));

        let key_hint = |key: &str, text: SharedString, cx: &gpui::App| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(keycap(key.to_string(), cx))
                .child(div().text_size(px(11.5)).text_color(t.fg_muted).child(text))
        };

        let toolbar = div()
            .occlude()
            .flex()
            .items_center()
            .gap(px(14.))
            .px(px(10.))
            .py(px(8.))
            .bg(t.surface)
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_panel)
            .shadow(t.shadow_overlay())
            .child(modes)
            .child(kinds)
            .child(delay)
            .child(div().w(px(1.)).h(px(24.)).bg(t.line));
        let toolbar = if no_recorder {
            toolbar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(11.5))
                    .text_color(t.err)
                    .child(ashd_ui::icon(Icon::AlertTriangle, metrics::icon_size::DENSE).text_color(t.err))
                    .child("no screen recorder installed"),
            )
        } else {
            toolbar.child(key_hint("↵", hint.into(), cx))
        };
        let toolbar = toolbar.child(key_hint("esc", "cancel".into(), cx));

        div().absolute().left_0().right_0().bottom(px(TOOLBAR_BOTTOM)).flex().justify_center().child(toolbar)
    }
}
