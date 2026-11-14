//! Layout of the cheatsheet (docs/design/pages/det-11-calendar-media-alt-tab-cheatsheet.html):
//! padding 20, gap 16; header = "Keys" 15/600, filter 240×30, Scroll/Canvas control;
//! grid = 3 columns (gap 32 between, 20 between sections); section title mono 10 CAPS in
//! the accent; rows 26 high, label sans 12.5 muted, key chips gap 3.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, size,
};
use ashd_tokens::WindowMode;
use ashd_ui::{Icon, Theme, empty_state, icon, keycap, overlay_panel, theme};

use crate::{
    CheatsheetPanel, Load,
    keys::{self, ROW_H, Row, Section},
};

/// Panel width. The mock's panel is 724 wide for its short labels; the project's real binds
/// ("Move column to workspace down" + four chips) need three columns of ~365.
pub const PANEL_W: f32 = 1200.0;
/// Space kept free around the panel on every side.
const MARGIN: f32 = 32.0;
const PAD: f32 = 20.0;

impl Render for CheatsheetPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let out = self.ctx.shell.modal_display(cx).map(|d| d.size()).unwrap_or_else(|| size(px(1920.), px(1080.)));
        let (out_w, out_h): (f32, f32) = (out.width.into(), out.height.into());
        let width = PANEL_W.min((out_w - 2.0 * MARGIN).max(320.0));
        let max_h = (out_h - 2.0 * MARGIN).max(240.0);
        let columns = if width >= 900.0 {
            3
        } else if width >= 600.0 {
            2
        } else {
            1
        };
        let body = self.body(&t, columns, cx);
        // Keep the height of the unfiltered content while filtering, so the centred panel
        // does not jump with every keystroke (never more than fits on the output).
        let head = 30.0 + 16.0 + 2.0 * PAD + 2.0;
        let stable_h = self.unfiltered_height(columns).min(max_h - head).max(0.0);

        div().size_full().flex().items_center().justify_center().child(
            overlay_panel(cx)
                .id("cheatsheet-panel")
                .occlude()
                .key_context("Cheatsheet")
                // A click inside keeps the typing target.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        use gpui::Focusable as _;
                        this.input.focus_handle(cx).focus(window, cx)
                    }),
                )
                .w(px(width))
                .max_h(px(max_h))
                .p(px(PAD))
                .flex()
                .flex_col()
                .gap(px(16.))
                .font_family(t.font_sans.clone())
                .text_color(t.fg)
                .child(self.header(&t, cx))
                .child(
                    div()
                        .id("cheatsheet-body")
                        .flex_1()
                        .min_h(px(stable_h))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(body),
                ),
        )
    }
}

impl CheatsheetPanel {
    fn header(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let field = div()
            .h(px(30.))
            .w(px(240.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_control)
            .font_family(t.font_mono.clone())
            .text_size(px(12.))
            .text_color(t.fg)
            .child(icon(Icon::Search, 13.).text_color(t.fg_muted).flex_none())
            .child(self.input.clone());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(ashd_tokens::metrics::type_scale::TITLE.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Keys"),
            )
            .child(div().flex_1())
            .child(field)
            .child(self.mode_control(t, cx))
    }

    /// Height of the tallest column with no filter applied (current mode).
    fn unfiltered_height(&self, columns: usize) -> f32 {
        let Load::Ready(sections) = &self.load else { return 0.0 };
        let all = keys::visible(sections, self.mode, "");
        let heights: Vec<f32> = all.iter().map(|(_, rows)| keys::section_height(rows.len())).collect();
        keys::masonry(&heights, columns)
            .iter()
            .map(|col| col.iter().map(|&i| heights[i]).sum::<f32>() + 20.0 * col.len().saturating_sub(1) as f32)
            .fold(0.0, f32::max)
    }

    /// Scroll / Canvas: 2 px padding and gap, 24 high segments, the active one in the accent.
    fn mode_control(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let seg = |ix: u32, name: &'static str, mode: WindowMode, cx: &mut Context<Self>| {
            let active = self.mode == mode;
            let hover = t.surface_2;
            div()
                .id(("cheatsheet-mode", ix))
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(t.radius_control)
                .cursor_pointer()
                .font_family(t.font_sans.clone())
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .when(active, |d| d.bg(t.accent).text_color(t.on_accent))
                .when(!active, |d| d.text_color(t.fg_muted).hover(move |s| s.bg(hover)))
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
        };
        div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .p(px(2.))
            .border_1()
            .border_color(t.line)
            .rounded(t.radius_control)
            .child(seg(0, "Scroll", WindowMode::Scroll, cx))
            .child(seg(1, "Canvas", WindowMode::Canvas, cx))
    }

    fn body(&self, t: &Theme, columns: usize, cx: &mut Context<Self>) -> AnyElement {
        let empty = |ic: Icon, title: &str, hint: String, cx: &mut Context<Self>| {
            div().h(px(220.)).child(empty_state(ic, title.to_string(), hint, Vec::new(), cx)).into_any_element()
        };
        match &self.load {
            Load::Loading => div().h(px(220.)).into_any_element(),
            Load::Missing => empty(
                Icon::Keyboard,
                "No keybinds file",
                format!(
                    "{} does not exist. The NixOS module writes it when ashd.shell.enable is on.",
                    self.path.display()
                ),
                cx,
            ),
            Load::Invalid(why) => {
                empty(Icon::AlertCircle, "Cannot read the keybinds", format!("{}: {why}", self.path.display()), cx)
            }
            Load::Ready(sections) => {
                let query = self.query(cx);
                let visible = keys::visible(sections, self.mode, &query);
                if visible.is_empty() {
                    let (title, hint) = if query.trim().is_empty() {
                        ("No keybinds".to_string(), format!("{} has no entries.", self.path.display()))
                    } else {
                        (format!("No keys match “{}”", query.trim()), "Try a title or a key, e.g. “workspace” or “shift 3”.".to_string())
                    };
                    return empty(Icon::Search, &title, hint, cx);
                }
                let heights: Vec<f32> = visible.iter().map(|(_, rows)| keys::section_height(rows.len())).collect();
                let cols = keys::masonry(&heights, columns);
                div()
                    .flex()
                    .items_start()
                    .gap(px(32.))
                    .children(cols.into_iter().map(|members| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(20.))
                            .children(members.into_iter().map(|ix| {
                                let (section, rows) = &visible[ix];
                                section_view(section, rows, t, cx)
                            }))
                    }))
                    .into_any_element()
            }
        }
    }
}

fn section_view(section: &Section, rows: &[&Row], t: &Theme, cx: &gpui::App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .h(px(14.))
                .font_family(t.font_mono.clone())
                .text_size(px(10.))
                .text_color(t.accent)
                .child(SharedString::from(section.name.to_uppercase())),
        )
        .children(rows.iter().map(|row| row_view(row, t, cx)))
}

fn row_view(row: &Row, t: &Theme, cx: &gpui::App) -> Div {
    let mut chips = div().flex().flex_none().items_center().gap(px(3.));
    for (ix, combo) in row.combos.iter().enumerate() {
        if ix > 0 {
            chips = chips.child(
                div()
                    .px(px(3.))
                    .font_family(t.font_mono.clone())
                    .text_size(px(10.5))
                    .text_color(t.fg_muted)
                    .child("/"),
            );
        }
        chips = chips.children(combo.iter().map(|key| keycap(SharedString::from(key.clone()), cx)));
    }
    div()
        .min_h(px(ROW_H))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.5))
                .text_color(t.fg_muted)
                .child(SharedString::from(row.title.clone())),
        )
        .child(chips)
}
