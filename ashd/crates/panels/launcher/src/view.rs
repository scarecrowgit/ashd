//! Rendering (docs/design/pages/det-04-launcher.html): input row 52, result rows 44,
//! calc block, status lines, empty state, footer chips.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, Div, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, ScrollDelta, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, Window, div, hsla, px,
};
use ashd_ipc::LauncherMode;
use ashd_tokens::{metrics, motion::timing};
use ashd_ui::{Icon, Theme, anim, app_icon, empty_state, icon, label, overlay_panel, theme};

use crate::{
    AltSubmit, ExitMode, LauncherPanel, SelectNext, SelectPrev,
    calc::CalcOut,
    model::{MAX_VISIBLE, Row, RowIcon, highlight_ranges, mode_label},
};

const ROW_H: f32 = 44.0;
const INPUT_H: f32 = 52.0;
const TILE: f32 = 28.0;
const EMPTY_H: f32 = 200.0;

/// The chips of the footer, in mockup order.
const CHIPS: [LauncherMode; 5] =
    [LauncherMode::Calc, LauncherMode::Files, LauncherMode::Run, LauncherMode::Emoji, LauncherMode::Web];

impl Render for LauncherPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let q = self.query(cx);
        let body: AnyElement = match self.mode {
            Some(LauncherMode::Calc) => self.render_calc(&q, &t, cx),
            _ => self.render_list(&q, &t, cx),
        };
        let footer = self.mode.is_none().then(|| self.render_footer(&t, cx));
        let seq = self.open_seq;
        div().size_full().flex().justify_center().items_start().child(
            overlay_panel(cx)
                .id("launcher-panel")
                .occlude()
                .key_context("Launcher")
                .on_action(cx.listener(|this, _: &AltSubmit, _, cx| this.activate(true, cx)))
                .on_action(cx.listener(|this, _: &ExitMode, _, cx| this.exit_mode(cx)))
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
                .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        use gpui::Focusable as _;
                        this.input.focus_handle(cx).focus(window, cx)
                    }),
                )
                .mt(px(metrics::LAUNCHER_TOP))
                .w(px(metrics::LAUNCHER_W))
                .flex()
                .flex_col()
                .overflow_hidden()
                .font_family(t.font_sans.clone())
                .text_color(t.fg)
                .child(self.render_input_row(&t, cx))
                .child(body)
                .children(footer)
                // 140 ms out-expo, .98 → 1. GPUI cannot scale a div, so the panel grows from
                // 98 % of its width while it fades in.
                .with_animation(
                    ("launcher-in", seq),
                    anim::tween(timing::LAUNCHER_IN, anim::EASE_OUT_EXPO),
                    |el, p| {
                        let s = timing::LAUNCHER_SCALE_FROM + (1.0 - timing::LAUNCHER_SCALE_FROM) * p;
                        el.opacity(p).w(px(metrics::LAUNCHER_W * s))
                    },
                ),
        )
    }
}

impl LauncherPanel {
    fn render_input_row(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let lead: AnyElement = match self.mode {
            None => icon(Icon::Search, metrics::icon_size::STATUS).text_color(t.fg_muted).into_any_element(),
            Some(m) => div()
                .id("mode-chip")
                .h(px(22.))
                .px(px(7.))
                .flex()
                .flex_none()
                .items_center()
                .rounded(t.radius_control)
                .bg(t.accent)
                .text_color(t.on_accent)
                .font_family(t.font_mono.clone())
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(SharedString::from(m.prefix().to_string()))
                .on_click(cx.listener(|this, _, _, cx| this.exit_mode(cx)))
                .into_any_element(),
        };
        let empty = self.input.read(cx).text().is_empty();
        let in_mode = self.mode.is_some();
        div()
            .h(px(INPUT_H))
            .px(px(16.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(t.line)
            .child(lead)
            .child({
                let d = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .font_family(t.font_mono.clone())
                    .text_size(px(15.))
                    .line_height(px(20.))
                    .text_color(t.fg)
                    .child(self.input.clone());
                // Backspace in an empty prefix mode leaves the mode (see ExitMode binding).
                if in_mode && empty { d.key_context("LauncherEmpty") } else { d }
            })
            .child(label("esc", cx))
    }

    fn render_list(&self, q: &str, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let visible: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(MAX_VISIBLE)
            .flat_map(|(ix, row)| {
                let mut els = Vec::with_capacity(2);
                if let Some(section) = row.section {
                    els.push(div().px(px(16.)).pt(px(10.)).pb(px(4.)).child(label(section, cx)).into_any_element());
                }
                els.push(self.render_row(ix, row, ix == self.selected, t, cx));
                els
            })
            .collect();

        let mut col = div().flex().flex_col().flex_none();
        if !visible.is_empty() {
            col = col
                .py(px(6.))
                .children(visible)
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
                }));
        }
        let status = self.status_line(q);
        let empty = self.empty_state(q, cx);
        div().flex().flex_col().child(col).children(empty).children(status.map(|s| {
            div()
                .px(px(16.))
                .py(px(8.))
                .text_size(px(metrics::type_scale::UI.0 - 1.0))
                .text_color(t.fg_muted)
                .child(SharedString::from(s))
        })).into_any_element()
    }

    fn render_row(&self, ix: usize, row: &Row, selected: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let glyph: AnyElement = match &row.icon {
            RowIcon::App(path) => app_icon(path.as_deref(), 20., cx),
            RowIcon::Glyph(ic) => icon(*ic, 15.).text_color(t.fg).into_any_element(),
            RowIcon::Text(s) => div().text_size(px(16.)).line_height(px(20.)).child(SharedString::from(s.clone())).into_any_element(),
        };
        let hl = HighlightStyle { color: Some(t.accent), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
        let title = StyledText::new(SharedString::from(row.title.clone()))
            .with_highlights(highlight_ranges(&row.title, &row.matched).into_iter().map(|r| (r, hl)));
        let hint = if selected { Some("↵".to_string()) } else { row.hint.clone() };
        let (accent, s2) = (t.accent, t.surface_2);
        div()
            .id(("launcher-row", ix as u32))
            .h(px(ROW_H))
            .px(px(14.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .border_l_2()
            .border_color(if selected { accent } else { hsla(0., 0., 0., 0.) })
            .bg(if selected { s2 } else { hsla(0., 0., 0., 0.) })
            .cursor_pointer()
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selected != ix {
                    this.selected = ix;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = ix;
                this.activate(false, cx);
            }))
            .child(
                div()
                    .size(px(TILE))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(t.line)
                    .rounded(t.radius_control)
                    .text_color(t.fg)
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_size(px(13.5)).line_height(px(17.)).text_color(t.fg).truncate().child(title))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(14.))
                            .text_color(t.fg_muted)
                            .truncate()
                            .child(SharedString::from(row.subtitle.clone())),
                    ),
            )
            .children(hint.map(|h| {
                div()
                    .flex_none()
                    .font_family(t.font_mono.clone())
                    .text_size(px(10.))
                    .text_color(t.fg_muted)
                    .child(SharedString::from(h.to_uppercase()))
            }))
            .into_any_element()
    }

    /// The muted line under the rows (files status, key hints).
    fn status_line(&self, q: &str) -> Option<String> {
        let typed = !q.trim().is_empty();
        match self.mode {
            None | Some(LauncherMode::Calc) => None,
            Some(LauncherMode::Files) => match (&self.files.results, typed) {
                (_, false) => Some("Type to search files · ⇧↵ opens the containing folder".into()),
                (Some(r), _) if self.rows.is_empty() && !self.files.searching => {
                    let _ = r;
                    None
                }
                (Some(r), _) => {
                    let n = if r.found > r.hits.len() { r.hits.len() } else { r.found };
                    let more = if self.files.searching { " · searching…" } else { "" };
                    Some(format!("{} · {} of {} · ⇧↵ open folder{more}", r.source, n, r.found))
                }
                (None, true) => Some("searching…".into()),
            },
            Some(LauncherMode::Run) => Some(if self.rows.is_empty() {
                "No history yet · type a command and press ↵".into()
            } else {
                "↵ run · ⇧↵ run in a terminal".into()
            }),
            Some(LauncherMode::Emoji) => Some("↵ copy to the clipboard".into()),
            Some(LauncherMode::Web) => Some(if typed {
                "↵ open in your browser".into()
            } else {
                "Type to search the web · start with gh, yt, w, g, cr or nix to pick an engine".into()
            }),
        }
    }

    fn empty_state(&self, q: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.rows.is_empty() {
            return None;
        }
        let q = q.trim();
        let (title, hint) = match self.mode {
            None if !q.is_empty() => (
                "No results".to_string(),
                format!("Press ↵ to run “{q}” as a command, or ⇧↵ to search the web."),
            ),
            Some(LauncherMode::Files) if !q.is_empty() && !self.files.searching && self.files.results.is_some() => {
                ("No files".to_string(), format!("Nothing in your files matches “{q}”."))
            }
            Some(LauncherMode::Emoji) if !self.rows.is_empty() || !q.is_empty() => {
                ("No emoji".to_string(), format!("Nothing is called “{q}”."))
            }
            _ => return None,
        };
        Some(div().h(px(EMPTY_H)).child(empty_state(Icon::Search, title, hint, Vec::new(), cx)).into_any_element())
    }

    fn render_footer(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        div().flex().flex_none().gap(px(14.)).px(px(16.)).py(px(10.)).border_t_1().border_color(t.line).children(
            CHIPS.into_iter().map(|m| {
                div()
                    .id(("launcher-chip", m.prefix() as u32))
                    .flex()
                    .gap(px(6.))
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_mode(Some(m), String::new(), cx)))
                    .child(
                        div()
                            .h(px(20.))
                            .min_w(px(20.))
                            .px(px(6.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_b_2()
                            .border_color(t.line)
                            .rounded(t.radius_control)
                            .font_family(t.font_mono.clone())
                            .text_size(px(10.5))
                            .text_color(t.fg)
                            .child(SharedString::from(m.prefix().to_string())),
                    )
                    .child(div().text_size(px(11.5)).text_color(t.fg_muted).child(mode_label(m)))
            }),
        )
    }

    fn render_calc(&self, q: &str, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mono = t.font_mono.clone();
        let (result, result_color, hint): (String, _, String) = match &self.calc_out {
            CalcOut::Value { text, int } => {
                let int = int.as_ref().map(|i| format!(" · ⇧↵ copy as int ({i})")).unwrap_or_default();
                (text.clone(), t.fg, format!("↵ copy{int}"))
            }
            CalcOut::Error(e) if !q.trim().is_empty() => ("…".to_string(), t.fg_muted, e.clone()),
            _ => ("0".to_string(), t.fg_muted, "type an expression · hex, %, units".to_string()),
        };
        let history = self.calc.history.iter().enumerate().take(5).map(|(i, (expr, res))| {
            let copy = res.clone();
            div()
                .id(("calc-history", i as u32))
                .flex()
                .justify_between()
                .gap(px(12.))
                .cursor_pointer()
                .font_family(mono.clone())
                .text_size(px(12.))
                .on_click(cx.listener(move |this, _, _, cx| this.exec(crate::model::Act::Copy(copy.clone()), cx)))
                .child(div().min_w_0().truncate().text_color(t.fg_muted).child(SharedString::from(expr.clone())))
                .child(div().flex_none().child(SharedString::from(res.clone())))
        });
        let has_history = !self.calc.history.is_empty();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.))
                    .py(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(34.))
                            .line_height(px(40.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(result_color)
                            .truncate()
                            .child(SharedString::from(result)),
                    )
                    .child(label(hint, cx).truncate()),
            )
            .when(has_history, |d| {
                d.child(
                    div()
                        .px(px(16.))
                        .pb(px(14.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(label("history", cx))
                        .children(history),
                )
            })
            .into_any_element()
    }
}
