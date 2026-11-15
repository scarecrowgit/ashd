//! Layout (docs/design/pages/desk-05 "stack", desk-06 / desk-08 "tabs"): a fixed top block
//! (title, search field, and in Tabs the tab strip) above one scrolling column of rows.
//! Every direct child of the column is one row (a section header, a divider, a card row, a
//! thumbnail row, a clipboard row), so the keyboard selection can be scrolled into view by
//! child index and the first five rows can fade in with a 60 ms stagger.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ObjectFit, ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window, div, img, px,
};
use ashd_svc_clipboard::{Entry, Kind};
use ashd_tokens::{ScratchpadVariant, metrics, motion};
use ashd_ui::{
    ButtonKind, Icon, Theme, TooltipExt as _, alpha, anim, app_icon, button_small, color, follow, hline, hover_ring,
    hover_wash, icon, label, mix, presence::Timing, rich_row, rich_row_text, theme, title,
};

use crate::{
    FlashKind, ScratchpadPanel,
    model::{
        self, HiddenWindow, ImageItem, ItemKey, Lists, Tab, age_label, age_secs, clip_meta, clip_text,
        image_meta, looks_like_command,
    },
};

const PAD: f32 = 16.0;
const SECTION_GAP: f32 = 14.0;
const ROW_GAP: f32 = 8.0;
/// Between clipboard rows (desk-05: `gap: 2px`).
const LIST_GAP: f32 = 2.0;
const FIELD_H: f32 = metrics::INPUT_H;
const THUMB_H: f32 = 60.0;
const PREVIEW_H: f32 = 170.0;
/// The first rows fade in one after the other (docs/design TOKENS.md §9: scratchpad stagger 60).
const STAGGER_MS: u64 = 60;
const STAGGER_ROWS: usize = 5;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// What one render works from: the three lists after the search, the keyboard order and
/// the selection.
pub(crate) struct Data {
    pub lists: Lists,
    pub items: Vec<ItemKey>,
    pub selected: Option<usize>,
    pub query: String,
}

impl Data {
    fn selected_key(&self) -> Option<&ItemKey> {
        self.selected.and_then(|i| self.items.get(i))
    }

    fn is_selected(&self, key: &ItemKey) -> bool {
        self.selected_key() == Some(key)
    }
}

/// One scroll row and whether it holds the selected item.
struct Rows {
    /// Each row with the space below it.
    rows: Vec<(AnyElement, f32)>,
    selected: Option<usize>,
}

impl Rows {
    fn new() -> Self {
        Self { rows: Vec::new(), selected: None }
    }

    fn push(&mut self, el: impl IntoElement, holds_selection: bool) {
        if holds_selection {
            self.selected = Some(self.rows.len());
        }
        self.rows.push((el.into_any_element(), ROW_GAP));
    }

    /// A row that sits close to the next one (the 2 px list gap of the clipboard rows).
    fn push_tight(&mut self, el: impl IntoElement, holds_selection: bool) {
        self.push(el, holds_selection);
        if let Some(last) = self.rows.last_mut() {
            last.1 = LIST_GAP;
        }
    }
}

impl ScratchpadPanel {
    /// The lists, order and selection for the current search, theme variant and tab.
    pub(crate) fn data(&self, cx: &App) -> Data {
        let tabs = self.is_tabs(cx);
        let niri = self.ctx.services.niri.state.read(cx);
        let apps = self.ctx.services.apps.state.read(cx);
        let clip = self.ctx.services.clipboard.state.read(cx);
        let query = self.input.read(cx).text().to_string();
        let windows = model::hidden_windows(niri, &apps.apps, &self.hidden_since, std::time::Instant::now());
        let images = model::merge_images(&clip.entries, &self.files, model::MAX_IMAGES);
        let lists = model::filter(&query, windows, images, &clip.entries, tabs);
        let items = model::items(&lists, tabs.then_some(self.tab));
        let selected = model::selection(&items, self.selected.as_ref(), tabs || !query.trim().is_empty());
        Data { lists, items, selected, query }
    }
}

impl Render for ScratchpadPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_open(window, cx);
        let t = theme(cx).clone();
        let tabs = t.scratchpad == ScratchpadVariant::Tabs;
        let d = self.data(cx);
        let focused = self.input.read(cx).is_focused(window);

        let Rows { rows, selected } = if tabs { self.tab_rows(&d, &t, cx) } else { self.stack_rows(&d, &t, cx) };
        if std::mem::take(&mut self.scroll_pending)
            && let Some(ix) = selected
        {
            self.scroll.scroll_to_item(ix);
        }
        let seq = self.open_seq;
        let rows = rows.into_iter().enumerate().map(|(ix, (row, gap))| stagger(row, gap, ix, seq));

        let top = div()
            .flex_none()
            .px(px(PAD))
            .pt(px(PAD))
            .flex()
            .flex_col()
            .gap(px(SECTION_GAP))
            .child(self.render_title(cx))
            .child(self.render_search(&t, focused, cx))
            .child(if tabs { self.render_tabs(&d, &t, cx).into_any_element() } else { divider(cx).into_any_element() });

        div()
            .id("scratchpad")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(t.font_sans.clone())
            .text_color(t.fg)
            // Typing goes to the search field wherever in the panel the click was.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    use gpui::Focusable as _;
                    this.input.focus_handle(cx).focus(window, cx);
                }),
            )
            .child(top)
            .child(
                div()
                    .id("scratchpad-scroll")
                    .flex_1()
                    .min_h_0()
                    .px(px(PAD))
                    .pt(px(if tabs { SECTION_GAP } else { 0. }))
                    .pb(px(PAD))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .children(rows),
            )
    }
}

/// Fade the first rows in with a staggered start (an 80 ms fade under reduced motion).
fn stagger(row: AnyElement, gap: f32, ix: usize, seq: u64) -> AnyElement {
    if ix >= STAGGER_ROWS {
        return div().w_full().flex_none().mb(px(gap)).child(row).into_any_element();
    }
    let delay = if anim::reduced_motion() { 0 } else { ix as u64 * STAGGER_MS };
    let dur = anim::duration_ms(motion::DUR_BASE);
    let total = delay + dur;
    div()
        .w_full()
        .flex_none()
        .mb(px(gap))
        .child(row)
        .with_animation(("scratchpad-in", seq * 8 + ix as u64), Animation::new(Duration::from_millis(total)), move |el, t| {
            let p = ((t * total as f32 - delay as f32) / dur as f32).clamp(0., 1.);
            el.opacity(anim::ease_standard(p))
        })
        .into_any_element()
}

fn divider(cx: &App) -> Div {
    hline(cx).w_auto().mx(px(-PAD))
}

/// A section divider inside the scroll column (full bleed, `line` colour).
fn section_divider(cx: &App) -> Div {
    divider(cx).my(px(SECTION_GAP - ROW_GAP))
}

/// 24 px ghost icon button (header actions).
fn ghost_icon(id: impl Into<gpui::ElementId>, ic: Icon, tint: gpui::Hsla, cx: &App) -> Stateful<Div> {
    let t = theme(cx);
    let hover = t.surface_2;
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(t.radius_control)
        .cursor_pointer()
        .child(hover_wash(hover, Some(t.line), t.radius_control))
        .child(icon(ic, 13.).text_color(tint))
}

/// `Windows  4 hidden ········ [actions]` (24 high).
fn section_header(name: &str, sub: String, actions: Vec<AnyElement>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .h(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .mb(px(SECTION_GAP - ROW_GAP))
        .child(label(name.to_string(), cx).text_color(t.fg))
        .child(label(sub, cx))
        .child(div().flex_1())
        .children(actions)
}

/// A muted line for a section with nothing to show.
fn empty_row(text: impl Into<SharedString>, hint: Option<String>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .px(px(10.))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(div().text_size(px(metrics::type_scale::UI.0)).text_color(t.fg_muted).child(text.into()))
        .children(hint.map(|h| label(h, cx).text_color(alpha(t.fg_muted, 0.75))))
}

impl ScratchpadPanel {
    // ── top block ─────────────────────────────────────────────────────────────

    fn render_title(&self, cx: &App) -> Div {
        div()
            .h(px(20.))
            .flex()
            .items_center()
            .justify_between()
            .child(title("Scratchpad", cx))
            .children(self.hints.panel.clone().map(|k| label(k, cx)))
    }

    fn render_search(&self, t: &Theme, focused: bool, cx: &App) -> Div {
        div()
            .h(px(FIELD_H))
            .px(px(10.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .border_1()
            .border_color(if focused { t.accent } else { t.line })
            .rounded(t.radius_control)
            .child(icon(Icon::Search, 13.).text_color(t.fg_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .font_family(t.font_mono.clone())
                    .text_size(px(11.5))
                    .line_height(px(16.))
                    .text_color(t.fg)
                    .child(self.input.clone()),
            )
            .children(self.hints.clipboard.clone().map(|k| label(k, cx)))
    }

    /// Tabs variant: Clipboard / Images / Windows, 30 high in a 3 px padded frame.
    fn render_tabs(&self, d: &Data, t: &Theme, cx: &mut Context<Self>) -> Div {
        let searching = !d.query.trim().is_empty();
        let count = |tab: Tab| match tab {
            Tab::Clipboard if searching => d.lists.clips.len(),
            Tab::Clipboard => d.lists.total_clips,
            Tab::Images => d.lists.images.len(),
            Tab::Windows => d.lists.windows.len(),
        };
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .p(px(3.))
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_control)
            .children(Tab::ALL.map(|tab| {
                let active = self.tab == tab;
                let (fg, hover_fg, s2) = (if active { t.on_accent } else { t.fg_muted }, t.fg, t.surface_2);
                let (accent, radius) = (t.accent, t.radius_control);
                div()
                    .id(("scratchpad-tab", tab.index() as u32))
                    .flex_1()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .rounded((t.radius_control - px(1.)).max(px(0.)))
                    .cursor_pointer()
                    .text_size(px(metrics::type_scale::UI.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(fg)
                    .child(
                        // The active tab's accent fill cross-fades between tabs.
                        follow(
                            ("scratchpad-tab-fill", tab.index() as u32),
                            if active { 1.0 } else { 0.0 },
                            Timing::new(motion::timing::SWAP, anim::EASE_STANDARD),
                            move |p| {
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .rounded((radius - gpui::px(1.)).max(gpui::px(0.)))
                                    .bg(mix(gpui::transparent_black(), accent, p))
                            },
                        )
                        .fade(),
                    )
                    .when(!active, move |d| {
                        d.hover(move |s| s.text_color(hover_fg)).child(hover_wash(s2, None, (radius - gpui::px(1.)).max(gpui::px(0.))))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(tab.label())
                    .child(
                        div()
                            .font_family(t.font_mono.clone())
                            .text_size(px(metrics::type_scale::LABEL.0))
                            .opacity(0.75)
                            .child(count(tab).to_string()),
                    )
            }))
    }

    // ── Stack: three sections ─────────────────────────────────────────────────

    fn stack_rows(&self, d: &Data, t: &Theme, cx: &mut Context<Self>) -> Rows {
        let mut out = Rows::new();
        self.windows_section(d, t, false, &mut out, cx);
        out.push(section_divider(cx), false);
        self.images_section(d, t, false, &mut out, cx);
        out.push(section_divider(cx), false);
        self.clipboard_section(d, t, false, &mut out, cx);
        out
    }

    // ── Tabs: the selected tab ────────────────────────────────────────────────

    fn tab_rows(&self, d: &Data, t: &Theme, cx: &mut Context<Self>) -> Rows {
        let mut out = Rows::new();
        match self.tab {
            Tab::Clipboard => {
                if let Some(ItemKey::Clip(id)) = d.selected_key()
                    && let Some(e) = d.lists.clips.iter().find(|e| e.id == *id)
                {
                    out.push(self.clip_card(e, t, cx), false);
                }
                self.clipboard_section(d, t, true, &mut out, cx);
            }
            Tab::Images => {
                if let Some(ItemKey::Image(key)) = d.selected_key()
                    && let Some(item) = d.lists.images.iter().find(|i| i.key == *key)
                {
                    out.push(self.image_preview(item, t, cx), false);
                }
                self.images_section(d, t, true, &mut out, cx);
            }
            Tab::Windows => self.windows_section(d, t, true, &mut out, cx),
        }
        out
    }

    // ── Windows ───────────────────────────────────────────────────────────────

    fn windows_section(&self, d: &Data, t: &Theme, tabs: bool, out: &mut Rows, cx: &mut Context<Self>) {
        let n = d.lists.windows.len();
        let plus = ghost_icon("scratchpad-hide-focused", Icon::Plus, t.fg_muted, cx)
            .ashd_tooltip("Hide the focused window here", self.hints.hide.as_deref())
            .on_click(cx.listener(|this, _, _, cx| this.hide_last_window(cx)))
            .into_any_element();
        let count = if d.lists.total_windows == n { n.to_string() } else { format!("{n} of {}", d.lists.total_windows) };
        let (name, sub) = if tabs { ("Hidden windows", count) } else { ("Windows", format!("{count} hidden")) };
        out.push(section_header(name, sub, vec![plus], cx), false);
        if d.lists.windows.is_empty() {
            out.push(self.windows_empty(d, cx), false);
            return;
        }
        // Stack: two columns; Tabs: one.
        let cols = if tabs { 1 } else { 2 };
        for chunk in d.lists.windows.chunks(cols) {
            let holds = chunk.iter().any(|w| d.is_selected(&ItemKey::Window(w.id)));
            let mut row = div().flex().gap(px(ROW_GAP));
            for w in chunk {
                row = row.child(self.window_card(w, d.is_selected(&ItemKey::Window(w.id)), t, cx));
            }
            for _ in chunk.len()..cols {
                row = row.child(div().flex_1().min_w_0());
            }
            out.push(row, holds);
        }
    }

    fn windows_empty(&self, d: &Data, cx: &App) -> Div {
        if d.lists.total_windows > 0 {
            return empty_row("No matches", Some(format!("nothing hidden matches “{}”", d.query.trim())), cx);
        }
        let how = match &self.hints.hide {
            Some(keys) => format!("hide the focused window with {keys}"),
            None => "hide the focused window with the + button".to_string(),
        };
        empty_row("No hidden windows", Some(how), cx)
    }

    /// A hidden window: app icon, title, app id, where it came from and for how long, and
    /// the faint three-line stand-in for a live thumbnail. Click = unhide and focus.
    fn window_card(&self, w: &HiddenWindow, selected: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = w.id;
        let (accent, line) = (t.accent, t.line);
        let glyph = match &w.icon_path {
            Some(p) => app_icon(Some(p), 14., cx),
            None => icon(w.glyph, 14.).text_color(t.fg_muted).into_any_element(),
        };
        let origin = match (&w.origin, w.age) {
            (Some(o), Some(a)) => Some(format!("from {o} · {}", age_secs(a.as_secs()))),
            (Some(o), None) => Some(format!("from {o}")),
            (None, Some(a)) => Some(format!("hidden {}", age_secs(a.as_secs()))),
            (None, None) => None,
        };
        let bar = |pct: f32| div().h(px(3.)).w(gpui::relative(pct)).bg(t.line);
        let close = ghost_icon(("scratchpad-win-close", id), Icon::Close, t.fg_muted, cx)
            .size(px(18.))
            .ashd_tooltip("Close this window", None)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.close_window(id, cx);
            }));
        div()
            .id(("scratchpad-win", id))
            .flex_1()
            .min_w_0()
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .bg(t.surface_2)
            .border_1()
            .border_color(if selected { t.accent } else { t.line })
            .rounded(t.radius_control)
            .cursor_pointer()
            .when(!selected, |d| d.child(hover_ring(alpha(accent, 0.55), px(1.), t.radius_control)))
            .active(move |s| s.border_color(line))
            .ashd_tooltip(format!("{} — click to bring it back", w.title), None)
            .on_click(cx.listener(move |this, _, _, cx| this.activate(ItemKey::Window(id), cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(glyph)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(metrics::type_scale::UI.0))
                            .child(SharedString::from(w.title.clone())),
                    )
                    .child(close),
            )
            .child(
                div()
                    .truncate()
                    .font_family(t.font_mono.clone())
                    .text_size(px(10.))
                    .text_color(t.fg_muted)
                    .child(SharedString::from(w.app_id.clone())),
            )
            .child(div().flex().flex_col().gap(px(3.)).child(bar(0.9)).child(bar(0.64)).child(bar(0.78)))
            .children(origin.map(|o| label(o, cx).truncate()))
            .into_any_element()
    }

    // ── Images ────────────────────────────────────────────────────────────────

    fn images_section(&self, d: &Data, t: &Theme, tabs: bool, out: &mut Rows, cx: &mut Context<Self>) {
        let folder = ghost_icon("scratchpad-open-shots", Icon::Folder, t.fg_muted, cx)
            .ashd_tooltip("Open the screenshots folder", None)
            .on_click(cx.listener(|this, _, _, cx| this.open_screenshots_folder(cx)))
            .into_any_element();
        out.push(section_header("Images", d.lists.images.len().to_string(), vec![folder], cx), false);
        if d.lists.images.is_empty() {
            let (text, hint) = if d.lists.total_images > 0 {
                ("No matches", format!("no image matches “{}”", d.query.trim()))
            } else {
                ("No recent images", "screenshots and copied images show up here".to_string())
            };
            out.push(empty_row(text, Some(hint), cx), false);
            return;
        }
        let _ = tabs;
        for (row_ix, chunk) in d.lists.images.chunks(3).enumerate() {
            let holds = chunk.iter().any(|i| d.is_selected(&ItemKey::Image(i.key.clone())));
            let mut row = div().flex().gap(px(ROW_GAP));
            for (col, item) in chunk.iter().enumerate() {
                let selected = d.is_selected(&ItemKey::Image(item.key.clone()));
                row = row.child(self.image_cell(row_ix * 3 + col, item, selected, t, cx));
            }
            for _ in chunk.len()..3 {
                row = row.child(div().flex_1().min_w_0());
            }
            out.push(row, holds);
        }
    }

    /// A 96×60 thumbnail with its name. Click = copy, right click = open the folder.
    fn image_cell(&self, ix: usize, item: &ImageItem, selected: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (open_key, copy_key) = (item.key.clone(), item.key.clone());
        let flash = self.flash.as_ref().filter(|(k, _)| *k == ItemKey::Image(item.key.clone())).map(|(_, kind)| *kind);
        let (accent, line) = (t.accent, t.line);
        let muted = t.fg_muted;
        let tip = {
            let meta = image_meta(item);
            if meta.is_empty() { item.name.clone() } else { format!("{} — {meta}", item.name) }
        };
        div()
            .id(("scratchpad-img", ix as u32))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(5.))
            .cursor_pointer()
            .ashd_tooltip(tip, None)
            .on_click(cx.listener(move |this, _, _, cx| this.activate(ItemKey::Image(copy_key.clone()), cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| this.open_image_location(&open_key, cx)),
            )
            .child(
                div()
                    .relative()
                    .h(px(THUMB_H))
                    .w_full()
                    .border_1()
                    .border_color(if selected { accent } else { line })
                    .rounded(t.radius_control)
                    .overflow_hidden()
                    .bg(t.bg)
                    .when(!selected, |d| d.child(hover_ring(alpha(accent, 0.55), px(1.), t.radius_control)))
                    .child(
                        img(item.thumb.clone())
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .with_fallback(move || icon(Icon::Image, 16.).text_color(muted).into_any_element()),
                    )
                    .when_some(flash, |d, kind| {
                        d.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(alpha(t.bg, 0.72))
                                .child(label(flash_text(kind), cx).text_color(accent)),
                        )
                    }),
            )
            .child(
                div()
                    .truncate()
                    .font_family(t.font_mono.clone())
                    .text_size(px(9.5))
                    .text_color(t.fg_muted)
                    .child(SharedString::from(item.name.clone())),
            )
            .into_any_element()
    }

    /// Tabs: the selected image, large, with its name and size.
    fn image_preview(&self, item: &ImageItem, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let muted = t.fg_muted;
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .h(px(PREVIEW_H))
                    .w_full()
                    .border_1()
                    .border_color(t.accent)
                    .rounded(t.radius_control)
                    .overflow_hidden()
                    .bg(t.bg)
                    .child(
                        img(item.path.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                            .with_fallback(move || icon(Icon::Image, 22.).text_color(muted).into_any_element()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(t.font_mono.clone())
                            .text_size(px(metrics::type_scale::LABEL.0))
                            .text_color(t.fg)
                            .child(SharedString::from(item.name.clone())),
                    )
                    .child(label(image_meta(item), cx)),
            )
            .into_any_element()
    }

    // ── Clipboard ─────────────────────────────────────────────────────────────

    fn clipboard_section(&self, d: &Data, t: &Theme, tabs: bool, out: &mut Rows, cx: &mut Context<Self>) {
        let armed = self.clear_armed;
        let more = ghost_icon("scratchpad-clip-more", Icon::External, t.fg_muted, cx)
            .ashd_tooltip("Open the clipboard manager", self.hints.clipboard.as_deref())
            .on_click(cx.listener(|this, _, _, cx| this.open_clipboard_manager(cx)))
            .into_any_element();
        let clear = ghost_icon("scratchpad-clip-clear", Icon::Trash, if armed { t.err } else { t.fg_muted }, cx)
            .ashd_tooltip(if armed { "Click again to clear the unpinned history" } else { "Clear clipboard history" }, None)
            .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx)))
            .into_any_element();
        let (name, sub) = if tabs {
            ("History", "pinned first".to_string())
        } else {
            ("Clipboard", d.lists.total_clips.to_string())
        };
        let sub = if armed { "click again to clear".to_string() } else { sub };
        out.push(section_header(name, sub, vec![more, clear], cx), false);
        if d.lists.clips.is_empty() {
            out.push(self.clipboard_empty(d, cx), false);
            return;
        }
        let now = now_ms();
        for e in &d.lists.clips {
            let selected = d.is_selected(&ItemKey::Clip(e.id));
            out.push_tight(self.clip_row(e, selected, tabs, now, t, cx), selected);
        }
    }

    fn clipboard_empty(&self, d: &Data, cx: &App) -> Div {
        let st = self.ctx.services.clipboard.state.read(cx);
        if d.lists.total_clips > 0 {
            empty_row("No matches", Some(format!("nothing copied matches “{}”", d.query.trim())), cx)
        } else if !st.available {
            empty_row("Clipboard history is unavailable", Some("the compositor offers no data-control protocol".into()), cx)
        } else {
            empty_row("Clipboard is empty", Some("text, links and images you copy show up here".into()), cx)
        }
    }

    /// A 40 px history row. Click = copy it (the row says "copied" for a moment).
    fn clip_row(&self, e: &Entry, selected: bool, tabs: bool, now: u64, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = e.id;
        let flash = self.flash.as_ref().filter(|(k, _)| *k == ItemKey::Clip(id)).map(|(_, kind)| *kind);
        let glyph: AnyElement = match (e.kind, e.colour) {
            (Kind::Colour, Some(c)) => div()
                .size(px(14.))
                .flex_none()
                .border_1()
                .border_color(t.line)
                .bg(color(ashd_tokens::Rgba { r: c.r, g: c.g, b: c.b, a: c.a }))
                .into_any_element(),
            (kind, _) => icon(kind_icon(kind), 14.).text_color(t.fg_muted).into_any_element(),
        };
        let pinned = e.pinned;
        let trailing: AnyElement = match flash {
            Some(kind) => label(flash_text(kind), cx).text_color(t.accent).into_any_element(),
            None => label(age_label(now, e.copied_at), cx).into_any_element(),
        };
        // The selected Stack row offers pin and delete in place; Tabs has the card.
        let actions = (selected && !tabs).then(|| {
            div()
                .flex()
                .flex_none()
                .child(
                    ghost_icon(("scratchpad-clip-pin", id), Icon::Pin, if pinned { t.accent } else { t.fg_muted }, cx)
                        .size(px(22.))
                        .ashd_tooltip(if pinned { "Unpin" } else { "Pin" }, None)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_pin(id, pinned, cx);
                        })),
                )
                .child(
                    ghost_icon(("scratchpad-clip-del", id), Icon::Trash, t.fg_muted, cx)
                        .size(px(22.))
                        .ashd_tooltip("Remove from history", None)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.delete_clip(id, cx);
                        })),
                )
        });
        rich_row(("scratchpad-clip", id), selected, cx)
            .when(selected, |d| d.border_color(t.line))
            .on_click(cx.listener(move |this, _, _, cx| this.activate(ItemKey::Clip(id), cx)))
            .child(div().flex().flex_none().child(glyph))
            .child(rich_row_text(clip_text(e), cx))
            .children(actions)
            .when(pinned && !(selected && !tabs), |d| d.child(icon(Icon::Pin, 12.).text_color(t.fg)))
            .child(trailing)
            .into_any_element()
    }

    /// Tabs: the selected entry as a card — what it is, its text, Paste / Pin / Run / Delete.
    fn clip_card(&self, e: &Entry, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = e.id;
        let pinned = e.pinned;
        let can_run = looks_like_command(e);
        let armed = self.run_armed == Some(id);
        let body: AnyElement = match (e.kind, e.image.as_ref()) {
            (Kind::Image, Some(info)) => {
                let muted = t.fg_muted;
                div()
                    .h(px(110.))
                    .w_full()
                    .border_1()
                    .border_color(t.line)
                    .overflow_hidden()
                    .bg(t.bg)
                    .child(
                        img(info.thumb.clone().unwrap_or_else(|| info.path.clone()))
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                            .with_fallback(move || icon(Icon::Image, 18.).text_color(muted).into_any_element()),
                    )
                    .into_any_element()
            }
            (Kind::Colour, _) if e.colour.is_some() => {
                let c = e.colour.unwrap_or_default();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .size(px(32.))
                            .border_1()
                            .border_color(t.line)
                            .bg(color(ashd_tokens::Rgba { r: c.r, g: c.g, b: c.b, a: c.a })),
                    )
                    .child(div().font_family(t.font_mono.clone()).text_size(px(13.)).child(c.hex()))
                    .into_any_element()
            }
            _ => div()
                .max_h(px(80.))
                .overflow_hidden()
                .font_family(t.font_mono.clone())
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(t.fg)
                .child(SharedString::from(card_text(e)))
                .into_any_element(),
        };
        let terminal = self.terminal_argv(cx);
        let run_note = armed.then(|| {
            let term = terminal.first().cloned().unwrap_or_else(|| "a terminal".into());
            label(format!("again to run in {term} · esc cancels"), cx).text_color(t.warn)
        });
        div()
            .flex_none()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .border_1()
            .border_color(t.accent)
            .bg(t.surface_2)
            .rounded(t.radius_control)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(8.))
                    .child(label(format!("selected · {}", clip_meta(e)), cx).min_w_0().truncate())
                    .child(label(age_label(now_ms(), e.copied_at), cx)),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(
                        button_small(("scratchpad-paste", id), "Paste", ButtonKind::Primary, cx)
                            .flex_1()
                            .ashd_tooltip("Make it the clipboard, then paste as usual", None)
                            .on_click(cx.listener(move |this, _, _, cx| this.activate(ItemKey::Clip(id), cx))),
                    )
                    .child(
                        button_small(("scratchpad-pin", id), if pinned { "Unpin" } else { "Pin" }, ButtonKind::Secondary, cx)
                            .flex_1()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_pin(id, pinned, cx))),
                    )
                    .when(can_run, |d| {
                        d.child(
                            button_small(
                                ("scratchpad-run", id),
                                if armed { "Run?" } else { "Run" },
                                if armed { ButtonKind::Danger } else { ButtonKind::Secondary },
                                cx,
                            )
                            .flex_1()
                            .ashd_tooltip("Run this command line in a terminal (asks twice)", None)
                            .on_click(cx.listener(move |this, _, _, cx| this.run_clip(id, cx))),
                        )
                    })
                    .child(
                        button_small(("scratchpad-delete", id), "Delete", ButtonKind::Secondary, cx)
                            .flex_1()
                            .on_click(cx.listener(move |this, _, _, cx| this.delete_clip(id, cx))),
                    ),
            )
            .children(run_note)
            .into_any_element()
    }
}

fn flash_text(kind: FlashKind) -> &'static str {
    match kind {
        FlashKind::Copied => "copied",
        FlashKind::Opened => "opened",
    }
}

/// The card's text: the preview, a few hundred characters of it.
fn card_text(e: &Entry) -> String {
    let text: String = e.preview.chars().take(320).collect();
    if e.truncated || text.len() < e.preview.len() { format!("{text}…") } else { text }
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
