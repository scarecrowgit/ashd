//! Rendering (docs/design/pages/det-10-clipboard-mod-v.html): header 50 (search + filter
//! chips), body 380 (list rows 36 | 320 preview pane), footer 36 (key hints).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Bounds, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ObjectFit, ParentElement as _, Render, ScrollDelta, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, canvas, div, fill, hsla, img, point,
    px, size,
};
use ashd_svc_clipboard::{Entry, Kind};
use ashd_tokens::{Rgba, motion};
use ashd_ui::{
    ButtonKind, Icon, Look, Theme, TooltipExt as _, anim, button_small, color, empty_state, follow, hover_wash, icon,
    icon_button_sized, keycap, label, mix, overlay_panel, presence::Timing, state_layer, theme,
};

use crate::{
    ClearUnpinned, ClipboardPanel, PageDown, PageUp, Pick1, Pick2, Pick3, Pick4, Pick5, Pick6, Pick7, Pick8, Pick9,
    PrevFilter, RemoveEntry, SaveEntry, TogglePin,
    model::{Filter, MAX_VISIBLE, age_label, counts, fit, human_size, meta_line, row_text},
};

use ashd_tokens::metrics::CLIPBOARD_W as PANEL_W;
const HEADER_H: f32 = 50.0;
const BODY_H: f32 = 380.0;
const FOOTER_H: f32 = 36.0;
// The panel height the shared metric promises.
const _: () = assert!(HEADER_H + BODY_H + FOOTER_H == ashd_tokens::metrics::CLIPBOARD_H);
const PANE_W: f32 = 320.0;
const ROW_H: f32 = 36.0;
/// The thumbnail box (inside its 1 px border).
const THUMB_W: f32 = 286.0;
const THUMB_H: f32 = 160.0;
const SWATCH_H: f32 = 96.0;
const CHECK_CELL: f32 = 8.0;

fn clear() -> Hsla {
    hsla(0., 0., 0., 0.)
}

fn rgba(c: ashd_svc_clipboard::Colour) -> Hsla {
    color(Rgba { r: c.r, g: c.g, b: c.b, a: c.a })
}

impl Render for ClipboardPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        div().size_full().flex().items_center().justify_center().child(
            overlay_panel(cx)
                .id("clipboard-panel")
                .occlude()
                .key_context("Clipboard")
                .on_action(cx.listener(|this, _: &TogglePin, _, cx| this.toggle_pin(cx)))
                .on_action(cx.listener(|this, _: &RemoveEntry, _, cx| this.remove_selected(cx)))
                .on_action(cx.listener(|this, _: &SaveEntry, _, cx| this.save_selected(cx)))
                .on_action(cx.listener(|this, _: &ClearUnpinned, _, cx| this.clear_unpinned(cx)))
                .on_action(cx.listener(|this, _: &PrevFilter, _, cx| this.set_filter(this.filter.prev(), cx)))
                .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(-1, cx)))
                .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(1, cx)))
                .on_action(cx.listener(|this, _: &Pick1, _, cx| this.pick(0, cx)))
                .on_action(cx.listener(|this, _: &Pick2, _, cx| this.pick(1, cx)))
                .on_action(cx.listener(|this, _: &Pick3, _, cx| this.pick(2, cx)))
                .on_action(cx.listener(|this, _: &Pick4, _, cx| this.pick(3, cx)))
                .on_action(cx.listener(|this, _: &Pick5, _, cx| this.pick(4, cx)))
                .on_action(cx.listener(|this, _: &Pick6, _, cx| this.pick(5, cx)))
                .on_action(cx.listener(|this, _: &Pick7, _, cx| this.pick(6, cx)))
                .on_action(cx.listener(|this, _: &Pick8, _, cx| this.pick(7, cx)))
                .on_action(cx.listener(|this, _: &Pick9, _, cx| this.pick(8, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        use gpui::Focusable as _;
                        this.input.focus_handle(cx).focus(window, cx)
                    }),
                )
                .w(px(PANEL_W))
                .flex()
                .flex_col()
                .overflow_hidden()
                .font_family(t.font_sans.clone())
                .text_color(t.fg)
                .child(self.render_header(&t, cx))
                .child(self.render_body(&t, cx))
                .child(self.render_footer(&t, cx)),
        )
    }
}

impl ClipboardPanel {
    // ── header ────────────────────────────────────────────────────────────────

    fn render_header(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let c = counts(&self.state(cx).entries);
        div()
            .h(px(HEADER_H))
            .px(px(16.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(t.line)
            .child(icon(Icon::Clipboard, 16.).text_color(t.fg))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .font_family(t.font_mono.clone())
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(t.fg)
                    .child(self.input.clone()),
            )
            .child(div().flex().flex_none().gap(px(6.)).children(Filter::ALL.map(|f| {
                let active = self.filter == f;
                let accent = t.accent;
                div()
                    .id(("clipboard-chip", f.index() as u32))
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(t.radius_control)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    // The active chip's accent border and text cross-fade between chips.
                    .child(state_layer(
                        "chip-fx",
                        active,
                        Look::new(gpui::transparent_black(), t.line),
                        Look::new(gpui::transparent_black(), t.accent),
                        t.radius_control,
                    ))
                    .when(!active, |d| d.child(hover_wash(t.surface_2, None, t.radius_control)))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(f, cx)))
                    .child(follow(
                        "chip-label",
                        if active { 1.0 } else { 0.0 },
                        Timing::new(motion::timing::SWAP, anim::EASE_STANDARD),
                        {
                            let text = SharedString::from(format!("{} {}", f.label(), c.of(f)));
                            let muted = t.fg_muted;
                            move |p| div().text_color(mix(muted, accent, p)).child(text)
                        },
                    ).fade())
            })))
    }

    // ── body ──────────────────────────────────────────────────────────────────

    fn render_body(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let body = div().h(px(BODY_H)).flex().flex_none();
        let entries = self.state(cx).entries.clone();
        if self.rows.is_empty() {
            return body.child(self.render_empty(t, cx));
        }
        let selected = self.selected_entry(cx);
        body.child(self.render_list(&entries, t, cx))
            .child(selected.map(|e| self.render_pane(&e, t, cx)).unwrap_or_else(|| div().w(px(PANE_W))))
    }

    fn render_empty(&self, _t: &Theme, cx: &mut Context<Self>) -> Div {
        let st = self.state(cx);
        let query = self.query(cx);
        let query = query.trim();
        let (title, hint) = if st.entries.is_empty() && !st.available {
            (
                "Clipboard history is unavailable".to_string(),
                "The compositor offers neither ext-data-control-v1 nor zwlr-data-control, so copies cannot be watched."
                    .to_string(),
            )
        } else if st.entries.is_empty() {
            ("Nothing copied yet".to_string(), "Text, links, colours and images you copy show up here.".to_string())
        } else if !query.is_empty() {
            ("No matches".to_string(), match self.filter {
                Filter::All => format!("Nothing in your history matches “{query}”."),
                f => format!("Nothing in {} matches “{query}”.", f.label()),
            })
        } else {
            let what = match self.filter {
                Filter::Pinned => "Nothing is pinned yet. Select an entry and press Alt+P.".to_string(),
                f => format!("Nothing in {} yet.", f.label()),
            };
            ("Nothing here".to_string(), what)
        };
        div().size_full().child(empty_state(Icon::Clipboard, title, hint, Vec::new(), cx))
    }

    fn render_list(&self, entries: &[Entry], t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let now = now_ms();
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(MAX_VISIBLE)
            .filter_map(|(ix, &e)| entries.get(e).map(|entry| self.render_row(ix, ix - self.scroll, entry, now, t, cx)))
            .collect();
        div()
            .id("clipboard-list")
            .flex_1()
            .min_w_0()
            .h_full()
            .py(px(6.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_r_1()
            .border_color(t.line)
            .on_scroll(cx.listener(|this, ev: &gpui::ScrollEvent, _, cx| {
                let dy = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => f32::from(p.y) / ROW_H,
                };
                // wheel up (positive) scrolls towards the top
                let rows = (-dy).round() as isize;
                if rows != 0 {
                    this.scroll_by(rows, cx);
                }
            }))
            .children(rows)
    }

    fn render_row(&self, ix: usize, offset: usize, e: &Entry, now: u64, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = ix == self.selected;
        let (accent, s2) = (t.accent, t.surface_2);
        let glyph: AnyElement = match (e.kind, e.colour) {
            (Kind::Colour, Some(c)) => {
                div().size(px(14.)).flex_none().border_1().border_color(t.line).bg(rgba(c)).into_any_element()
            }
            (kind, _) => icon(kind_icon(kind), 14.).text_color(t.fg_muted).into_any_element(),
        };
        div()
            .id(("clipboard-row", ix as u32))
            .h(px(ROW_H))
            .px(px(12.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .border_l_2()
            .border_color(if selected { accent } else { clear() })
            .when(selected, |d| d.bg(s2))
            .when(!selected, move |d| d.child(hover_wash(s2, None, t.radius_control)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                this.select(ix, cx);
                if ev.click_count() >= 2 {
                    this.activate_selected(cx);
                }
            }))
            .child(
                div()
                    .w(px(12.))
                    .flex_none()
                    .font_family(t.font_mono.clone())
                    .text_size(px(10.5))
                    .text_color(t.fg_muted)
                    .child(SharedString::from(if offset < 9 { (offset + 1).to_string() } else { String::new() })),
            )
            .child(div().flex().flex_none().child(glyph))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(t.font_mono.clone())
                    .text_size(px(12.))
                    .child(SharedString::from(row_text(e))),
            )
            .when(e.pinned, |d| d.child(icon(Icon::Pin, 12.).text_color(t.fg)))
            .child(
                div()
                    .flex_none()
                    .font_family(t.font_mono.clone())
                    .text_size(px(10.))
                    .text_color(t.fg_muted)
                    .child(SharedString::from(age_label(now, e.copied_at).to_uppercase())),
            )
            .into_any_element()
    }

    // ── preview pane ──────────────────────────────────────────────────────────

    fn render_pane(&self, e: &Entry, t: &Theme, cx: &mut Context<Self>) -> Div {
        let preview: AnyElement = match e.kind {
            Kind::Image => self.image_preview(e, t),
            Kind::Colour if e.colour.is_some() => self.colour_preview(e, t),
            _ => self.text_preview(e, t),
        };
        let fixed = matches!(e.kind, Kind::Image | Kind::Colour);
        let id = e.id;
        let pinned = e.pinned;
        div()
            .w(px(PANE_W))
            .h_full()
            .flex_none()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .overflow_hidden()
            .child(label(meta_line(e), cx).truncate())
            .child(if fixed { preview } else { div().flex_1().min_h_0().flex().flex_col().child(preview).into_any_element() })
            .child(self.detail_grid(e, t, cx))
            .when(fixed, |d| d.child(div().flex_1()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        button_small("clipboard-paste", "Paste", ButtonKind::Primary, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.activate_selected(cx))),
                    )
                    .child(
                        button_small("clipboard-save", "Save…", ButtonKind::Secondary, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.save_selected(cx))),
                    )
                    .child(
                        icon_button_sized("clipboard-pin", Icon::Pin, 32., 14., cx)
                            .border_1()
                            .border_color(if pinned { t.accent } else { clear() })
                            .ashd_tooltip(if pinned { "Unpin" } else { "Pin" }, Some("alt+p"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.ctx.services.clipboard.handle.set_pinned(id, !pinned);
                                cx.notify();
                            })),
                    )
                    .child(
                        icon_button_sized("clipboard-remove", Icon::Trash, 32., 14., cx)
                            .ashd_tooltip("Remove", Some("alt+d"))
                            .on_click(cx.listener(|this, _, _, cx| this.remove_selected(cx))),
                    ),
            )
            .children(self.status_line(id, t, cx))
    }

    /// Thumbnail fitted into 286×160 over a checkerboard.
    fn image_preview(&self, e: &Entry, t: &Theme) -> AnyElement {
        let frame = div().border_1().border_color(t.line).w(px(THUMB_W + 2.)).flex_none();
        let inner = div().w(px(THUMB_W)).h(px(THUMB_H)).flex().items_center().justify_center().bg(t.bg);
        let Some(info) = e.image.as_ref().filter(|i| i.width > 0) else {
            return frame
                .child(
                    inner
                        .flex_col()
                        .gap(px(8.))
                        .child(icon(Icon::Image, 22.).text_color(t.fg_muted))
                        .child(div().text_size(px(11.5)).text_color(t.fg_muted).child("No preview for this format")),
                )
                .into_any_element();
        };
        let (w, h) = fit(info.width, info.height, THUMB_W, THUMB_H);
        let source = info.thumb.clone().unwrap_or_else(|| info.path.clone());
        frame
            .child(
                inner.child(
                    div()
                        .relative()
                        .w(px(w))
                        .h(px(h))
                        .flex_none()
                        .child(checker(t.bg, t.surface_2))
                        .child(img(source).w(px(w)).h(px(h)).object_fit(ObjectFit::Contain)),
                ),
            )
            .into_any_element()
    }

    /// A large swatch (over a checkerboard when translucent).
    fn colour_preview(&self, e: &Entry, t: &Theme) -> AnyElement {
        let c = e.colour.unwrap_or_default();
        div()
            .border_1()
            .border_color(t.line)
            .w(px(THUMB_W + 2.))
            .h(px(SWATCH_H + 2.))
            .flex_none()
            .relative()
            .child(checker(t.bg, t.surface_2))
            .child(div().absolute().size_full().bg(rgba(c)))
            .into_any_element()
    }

    /// Scrollable mono text (text, code, links, paths: the full value).
    fn text_preview(&self, e: &Entry, t: &Theme) -> AnyElement {
        let mut text = e.preview.clone();
        if e.truncated {
            text.push_str(&format!("\n… {} in total, showing the first {}", human_size(e.size), human_size(text.len() as u64)));
        }
        div()
            .id(("clipboard-preview", e.id as u32))
            .flex_1()
            .min_h_0()
            .border_1()
            .border_color(t.line)
            .bg(t.bg)
            .p(px(8.))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .font_family(t.font_mono.clone())
            .text_size(px(12.))
            .line_height(px(17.))
            .text_color(t.fg)
            .child(SharedString::from(text))
            .into_any_element()
    }

    fn detail_grid(&self, e: &Entry, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mut rows: Vec<(&str, String)> = Vec::new();
        match (e.kind, e.colour) {
            (Kind::Colour, Some(c)) => {
                rows.push(("hex", c.hex()));
                rows.push(("rgb", c.rgb()));
                rows.push(("hsl", c.hsl()));
            }
            _ => rows.push(("type", e.kind.label().to_string())),
        }
        rows.push(("copied", copied_label(e.copied_at)));
        let _ = cx;
        div().flex().flex_none().flex_col().gap(px(4.)).font_family(t.font_mono.clone()).text_size(px(11.5)).children(
            rows.into_iter().map(|(k, v)| {
                div()
                    .flex()
                    .gap(px(4.))
                    .child(div().w(px(70.)).flex_none().text_color(t.fg_muted).child(k))
                    .child(div().flex_1().min_w_0().truncate().child(SharedString::from(v)))
            }),
        )
    }

    /// The newest save result or service note of this opening.
    fn status_line(&self, entry_id: u64, t: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        const NOTE_LIFETIME_MS: u64 = 5 * 60 * 1000;
        let st = self.state(cx);
        let save = st.last_save.as_ref().filter(|s| s.seq > self.seen_seq && s.id == entry_id);
        let note = st.note.as_ref().filter(|n| now_ms().saturating_sub(n.at_ms) < NOTE_LIFETIME_MS);
        // (heading, text, is error): the newer of the save result and the service note
        let (heading, text, error) = match (save, note) {
            (Some(s), n) if n.is_none_or(|n| s.seq >= n.seq) => match (&s.error, &s.path) {
                (Some(e), _) => (Some("save failed"), e.clone(), true),
                (None, Some(p)) => (Some("saved"), tilde(p), false),
                (None, None) => return None,
            },
            (_, Some(n)) => (None, n.text.clone(), false),
            _ => return None,
        };
        let color = if error { t.err } else { t.fg_muted };
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(heading.map(|h| label(h, cx).text_color(color)))
                .child(
                    div()
                        .font_family(t.font_mono.clone())
                        .text_size(px(11.))
                        .text_color(color)
                        .when(heading.is_some(), |d| d.truncate())
                        .child(SharedString::from(text)),
                ),
        )
    }

    // ── footer ────────────────────────────────────────────────────────────────

    fn render_footer(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let hint = |keys: &[&str], text: &str| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .children(keys.iter().map(|k| keycap(SharedString::from(k.to_string()), cx)))
                .child(div().text_size(px(11.5)).text_color(t.fg_muted).child(SharedString::from(text.to_string())))
        };
        div()
            .h(px(FOOTER_H))
            .px(px(16.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(18.))
            .border_t_1()
            .border_color(t.line)
            .child(hint(&["↵"], "copy, then paste yourself"))
            .child(hint(&["1-9"], "quick"))
            .child(hint(&["alt", "p"], "pin"))
            .child(hint(&["alt", "d"], "remove"))
            .child(hint(&["alt", "s"], "save"))
            .child(hint(&["tab"], "filter"))
            .child(hint(&["esc"], "close"))
    }
}

fn kind_icon(kind: Kind) -> Icon {
    match kind {
        Kind::Text | Kind::Colour => Icon::Text,
        Kind::Link => Icon::Link,
        Kind::Path => Icon::Folder,
        Kind::Image => Icon::Image,
        Kind::Code => Icon::Code,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// `14:30` today, `13 Nov 14:30` otherwise (local time).
fn copied_label(ms: u64) -> String {
    use chrono::{Local, TimeZone as _};
    let Some(then) = Local.timestamp_millis_opt(ms as i64).single() else { return String::new() };
    if then.date_naive() == Local::now().date_naive() {
        then.format("%H:%M").to_string()
    } else {
        then.format("%d %b %H:%M").to_string()
    }
}

/// `/home/me/Pictures/x.png` → `~/Pictures/x.png`.
fn tilde(path: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(|p| p.to_path_buf())) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A checkerboard filling its (relative) parent: `base` with `alt` cells.
fn checker(base: Hsla, alt: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            window.paint_quad(fill(bounds, base));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let (cols, rows) = ((w / CHECK_CELL).ceil() as usize, (h / CHECK_CELL).ceil() as usize);
            for r in 0..rows {
                for c in 0..cols {
                    if (r + c) % 2 == 1 {
                        continue;
                    }
                    let cell = Bounds::new(
                        bounds.origin + point(px(c as f32 * CHECK_CELL), px(r as f32 * CHECK_CELL)),
                        size(px(CHECK_CELL), px(CHECK_CELL)),
                    );
                    window.paint_quad(fill(cell.intersect(&bounds), alt));
                }
            }
        },
    )
    .absolute()
    .size_full()
}
