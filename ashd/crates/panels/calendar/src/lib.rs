//! Calendar popup from the clock (det-11): month grid with ISO week numbers, an agenda for
//! the selected day read from local `.ics` files, keyboard navigation.
//!
//! Owner: calendar. Contract: ARCHITECTURE.md "Panels" — `CalendarPanel` is registered by
//! `ashd-shell` with `reg.register::<CalendarPanel>()`. Opened by the bar's clock (it asks
//! for the `calendar` modal with the clock's bounds as anchor) and by
//! `Request::Calendar` (`ashd calendar`).
//!
//! Keys: arrows move the selected day (a day outside the month switches the month),
//! PageUp / PageDown change the month (with Shift: the year), Home jumps to today,
//! Escape closes. Click selects a day.
//!
//! [`ics`] is the RFC 5545 reader (read on a background thread when the popup opens),
//! [`model`] the date arithmetic; both are unit-tested without a display.

pub mod ics;
pub mod model;

use std::{collections::HashSet, sync::Arc};

use chrono::{Datelike as _, Duration, Local, NaiveDate};
use gpui::{
    AnimationExt as _, Context, Div, ElementId, FocusHandle, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Window, div, prelude::*, px,
};
use ashd_ipc::{PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_tokens::{LayoutVariant, metrics, motion};
use ashd_ui::{ButtonKind, Icon, Look, anim, button_small, hover_wash, icon_button_kind, label, state_layer, theme};

use crate::{
    ics::{Calendars, Occurrence},
    model::{GRID_WEEKS, WEEKDAYS},
};

// Popup width (det-11: 340): the shared metric.
pub use ashd_tokens::metrics::CALENDAR_W;
/// Agenda rows shown before "+N more".
const AGENDA_ROWS: usize = 5;

/// Panel state (fm rule: everything this region needs lives here).
pub struct CalendarPanelState {
    focus: FocusHandle,
    /// The selected day; the grid shows its month.
    pub selected: NaiveDate,
    /// `None` until the first read finished.
    calendars: Option<Arc<Calendars>>,
    /// Occurrences touching the visible grid, from [`ics::expand`].
    occurrences: Vec<Occurrence>,
    /// Bumped when another month is shown: the new grid fades in (140 ms). 0 = as opened.
    page_seq: u64,
}

pub struct CalendarPanel {
    pub ctx: PanelContext,
    pub state: CalendarPanelState,
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

impl CalendarPanel {
    fn is_open(&self, cx: &gpui::App) -> bool {
        self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID)
    }

    /// Re-expand the events for the visible grid.
    fn refresh_occurrences(&mut self) {
        let sel = self.state.selected;
        self.state.occurrences = match &self.state.calendars {
            Some(c) => ics::expand(&c.events, model::grid_start(sel), model::grid_end(sel)),
            None => Vec::new(),
        };
    }

    /// Select `day`; recompute the occurrences when the month changed.
    fn select(&mut self, day: NaiveDate, cx: &mut Context<Self>) {
        let month_changed = (day.year(), day.month()) != (self.state.selected.year(), self.state.selected.month());
        self.state.selected = day;
        if month_changed {
            self.state.page_seq += 1;
            self.refresh_occurrences();
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        if ev.keystroke.key == "escape" {
            self.ctx.shell.close_modal(cx);
            return;
        }
        if let Some(day) = model::navigate(self.state.selected, today(), &ev.keystroke.key, m.shift) {
            self.select(day, cx);
        }
    }

    /// Days of the grid that carry an event.
    fn busy_days(&self) -> HashSet<NaiveDate> {
        let start = model::grid_start(self.state.selected);
        (0..(GRID_WEEKS * 7) as i64)
            .map(|i| start + Duration::days(i))
            .filter(|d| self.state.occurrences.iter().any(|o| o.on(*d)))
            .collect()
    }
}

impl Panel for CalendarPanel {
    const ID: PanelId = PanelId("calendar");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });
    const ANCHORS: &'static [&'static str] = &[ashd_shell_core::anchors::names::CLOCK];

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let state = CalendarPanelState {
            focus: cx.focus_handle(),
            selected: today(),
            calendars: None,
            occurrences: Vec::new(),
            page_seq: 0,
        };
        Self { ctx, state }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Calendar { op } = request else { return None };
        let open = self.is_open(cx);
        match (op, open) {
            (PanelOp::Open, _) | (PanelOp::Toggle, false) => {
                let anchor = self.ctx.shell.request_anchor(cx);
                self.ctx.shell.open_modal(Self::ID, anchor, cx);
            }
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.focus.focus(window, cx);
        self.state.selected = today();
        self.state.page_seq = 0;
        self.refresh_occurrences();
        // The calendars are read off the main thread; the popup is usable meanwhile.
        let task = cx.background_executor().spawn(async { ics::load_dirs(&ics::calendar_dirs()) });
        cx.spawn(async move |this, cx| {
            let calendars = task.await;
            this.update(cx, |this, cx| {
                this.state.calendars = Some(Arc::new(calendars));
                this.refresh_occurrences();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

// ── view ──────────────────────────────────────────────────────────────────────

/// A grid header / week-number style: mono at `size`.
fn cell_text(text: impl Into<SharedString>, size: f32, color: gpui::Hsla, cx: &gpui::App) -> Div {
    let t = theme(cx);
    div().font_family(t.font_mono.clone()).text_size(px(size)).text_color(color).child(text.into())
}

impl CalendarPanel {
    fn header(&self, cx: &mut Context<Self>) -> Div {
        let sel = self.state.selected;
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(ashd_ui::title(model::month_title(sel), cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        button_small("cal-today", "Today", ButtonKind::Ghost, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.select(today(), cx))),
                    )
                    .child(
                        icon_button_kind("cal-prev", Icon::ChevronLeft, ButtonKind::Ghost, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let d = model::add_months(this.state.selected, -1);
                                this.select(d, cx);
                            })),
                    )
                    .child(
                        icon_button_kind("cal-next", Icon::ChevronRight, ButtonKind::Ghost, cx)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let d = model::add_months(this.state.selected, 1);
                                this.select(d, cx);
                            })),
                    ),
            )
    }

    fn day_cell(&self, day: NaiveDate, busy: bool, today: NaiveDate, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx).clone();
        let selected = day == self.state.selected;
        let is_today = day == today;
        let in_month = (day.year(), day.month()) == (self.state.selected.year(), self.state.selected.month());
        let (fg, dot) = match (is_today, in_month) {
            (true, _) => (t.on_accent, t.on_accent),
            (false, true) => (t.fg, t.accent),
            (false, false) => (t.fg_muted, ashd_ui::alpha(t.accent, 0.5)),
        };
        let s2 = t.surface_2;
        div()
            .id(ElementId::from(("cal-day", day.num_days_from_ce() as u32)))
            .flex_1()
            .min_w_0()
            .h(px(34.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(2.))
            .border_1()
            .border_color(gpui::transparent_black())
            .rounded(t.radius_control)
            .when(is_today, |d| d.bg(t.accent))
            .child(state_layer(
                "cal-sel",
                selected && !is_today,
                Look::new(gpui::transparent_black(), gpui::transparent_black()),
                Look::new(gpui::transparent_black(), t.accent),
                t.radius_control,
            ))
            .when(!is_today, |d| d.cursor_pointer().child(hover_wash(s2, None, t.radius_control)))
            .font_family(t.font_mono.clone())
            .text_size(px(12.5))
            .when(is_today, |d| d.font_weight(FontWeight::MEDIUM))
            .text_color(fg)
            .child(day.day().to_string())
            .child(div().size(px(3.)).flex_none().when(busy, |d| d.bg(dot)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(day, cx)))
    }

    fn grid(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme(cx).clone();
        let busy = self.busy_days();
        let now = today();
        let start = model::grid_start(self.state.selected);

        let header_cell = |text: &str, cx: &gpui::App| {
            div()
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .child(cell_text(text.to_uppercase(), 10., t.fg_muted, cx))
        };
        let mut head = div().flex().gap(px(2.)).child(header_cell("", cx));
        for name in WEEKDAYS {
            head = head.child(header_cell(name, cx));
        }
        let mut grid = div().flex().flex_col().gap(px(2.)).child(head);
        for week in 0..GRID_WEEKS as i64 {
            let row_start = start + Duration::days(week * 7);
            let mut row = div().flex().gap(px(2.)).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(34.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .child(cell_text(model::iso_week(row_start).to_string(), 10., t.fg_muted, cx))
                    // Same footprint as a day cell's event dot, so the baselines agree.
                    .child(div().h(px(3.)).flex_none()),
            );
            for day in 0..7 {
                let date = row_start + Duration::days(day);
                row = row.child(self.day_cell(date, busy.contains(&date), now, cx));
            }
            grid = grid.child(row);
        }
        match self.state.page_seq {
            0 => grid.into_any_element(),
            seq => grid
                .with_animation(
                    ElementId::NamedInteger("cal-grid".into(), seq),
                    anim::tween(motion::timing::PAGE, anim::EASE_STANDARD),
                    |el, p| el.opacity(p),
                )
                .into_any_element(),
        }
    }

    fn agenda(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let sel = self.state.selected;
        let events: Vec<&Occurrence> = self.state.occurrences.iter().filter(|o| o.on(sel)).collect();
        let mut col = div()
            .pt(px(12.))
            .border_t_1()
            .border_color(t.line)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label("agenda", cx))
                    .child(label(model::day_label(sel), cx)),
            );
        if events.is_empty() {
            let text = match &self.state.calendars {
                Some(c) if c.files == 0 => "no calendars found",
                Some(_) => "nothing planned",
                None => "",
            };
            col = col.child(
                div()
                    .h(px(18.))
                    .font_family(t.font_sans.clone())
                    .text_size(px(metrics::type_scale::UI.0))
                    .text_color(t.fg_muted)
                    .child(text),
            );
        } else {
            for o in events.iter().take(AGENDA_ROWS) {
                col = col.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(10.))
                        .child(
                            div()
                                .w(px(84.))
                                .flex_none()
                                .font_family(t.font_mono.clone())
                                .text_size(px(11.))
                                .text_color(t.fg_muted)
                                .child(o.time_label(sel)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(t.font_sans.clone())
                                .text_size(px(metrics::type_scale::UI.0))
                                .text_color(t.fg)
                                .child(SharedString::from(o.summary.clone())),
                        ),
                );
            }
            if events.len() > AGENDA_ROWS {
                col = col.child(label(format!("+{} more", events.len() - AGENDA_ROWS), cx));
            }
        }
        col
    }
}

impl Render for CalendarPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();

        // Placement: bottom = bar_h + POPUP_GAP, left = the clock's left edge.
        let anchor = self.ctx.shell.modal_anchor(Self::ID, cx);
        let output_w: f32 = self.ctx.shell.modal_display(cx).map(|d| d.size().width.into()).unwrap_or(1920.0);
        let margin = if t.layout == LayoutVariant::Floating { f32::from(t.gap) } else { 0.0 };
        let bottom = f32::from(t.bar_h) + margin + metrics::POPUP_GAP;
        let left = anchor.map(|a| f32::from(a.origin.x)).unwrap_or(margin);
        let left = left.min(output_w - CALENDAR_W - margin).max(margin);

        let body = div()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.header(cx))
            .child(self.grid(cx))
            .child(self.agenda(cx));

        let panel = ashd_ui::overlay_panel(cx)
            .occlude()
            .track_focus(&self.state.focus)
            .key_context("Calendar")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.on_key(ev, cx)))
            .absolute()
            .left(px(left))
            .bottom(px(bottom))
            .w(px(CALENDAR_W))
            .text_color(t.fg)
            .child(body);
        div().size_full().relative().child(panel)
    }
}
