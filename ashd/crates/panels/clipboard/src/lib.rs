//! Clipboard manager (det-10): a modal, 860 px wide, centred, over a scrim. Search field,
//! filter chips with counts, the history list and a preview pane for the selected entry.
//!
//! Keys (all in the panel; the mockup's own bindings are not reused):
//! `↑`/`↓` select, `PgUp`/`PgDn` page, `Tab`/`Shift+Tab` cycle the filter, `Enter` (or a
//! double click, or the Paste button) makes the entry the clipboard and closes — the user
//! then pastes with their normal shortcut, nothing here synthesises key presses —
//! `1`–`9` pick a visible row while the search is empty, `Alt+1`–`Alt+9` always,
//! `Alt+P` pin, `Alt+D` remove, `Alt+S` save into `~/Pictures` / `~/Downloads`,
//! `Alt+Shift+D` clear everything unpinned, `Esc` closes.
//!
//! Owner: clipboard. State lives in the service (`ashd-svc-clipboard`); the panel keeps only
//! the filter, query, selection and scroll. Layout: `view.rs`; pure helpers: `model.rs`.

mod model;
mod view;

use gpui::{AppContext as _, Context, Entity, Focusable as _, KeyBinding, Window, actions};
use nucleo_matcher::Matcher;
use ashd_ipc::{PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_clipboard::{ClipboardState, Entry};
use ashd_ui::{TextInput, TextInputEvent};

use model::{Filter, MAX_VISIBLE, quick_pick, scroll_into_view, visible};

actions!(
    clipboard,
    [
        TogglePin,
        RemoveEntry,
        SaveEntry,
        ClearUnpinned,
        PrevFilter,
        PageUp,
        PageDown,
        Pick1,
        Pick2,
        Pick3,
        Pick4,
        Pick5,
        Pick6,
        Pick7,
        Pick8,
        Pick9
    ]
);

pub struct ClipboardPanel {
    pub(crate) ctx: PanelContext,
    pub(crate) input: Entity<TextInput>,
    pub(crate) filter: Filter,
    /// Indices into the service's `entries` that pass the filter and the query, in order.
    pub(crate) rows: Vec<usize>,
    pub(crate) selected: usize,
    /// The selected entry, so a new copy arriving while open does not move the selection.
    pub(crate) selected_id: Option<u64>,
    /// First visible row (the list shows [`MAX_VISIBLE`] at a time).
    pub(crate) scroll: usize,
    pub(crate) open: bool,
    /// Restarts the fade-in on every open.
    pub(crate) open_seq: u64,
    /// The search text before the last change (digits pick rows only from an empty search).
    typed: String,
    matcher: Matcher,
    /// `last_save` / `note` with a `seq` up to these are from before this opening.
    pub(crate) seen_seq: u64,
}

impl Panel for ClipboardPanel {
    const ID: PanelId = PanelId("clipboard");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: true });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("search history", cx));
        cx.subscribe(&input, |this: &mut Self, _, ev: &TextInputEvent, cx| this.on_input_event(ev, cx)).detach();
        cx.observe(&ctx.services.clipboard.state, |this, _, cx| {
            if this.open {
                this.recompute(cx);
            }
        })
        .detach();
        let c = Some("Clipboard");
        cx.bind_keys([
            KeyBinding::new("alt-p", TogglePin, c),
            KeyBinding::new("alt-d", RemoveEntry, c),
            KeyBinding::new("alt-s", SaveEntry, c),
            KeyBinding::new("alt-shift-d", ClearUnpinned, c),
            KeyBinding::new("shift-tab", PrevFilter, c),
            KeyBinding::new("pageup", PageUp, c),
            KeyBinding::new("pagedown", PageDown, c),
            KeyBinding::new("alt-1", Pick1, c),
            KeyBinding::new("alt-2", Pick2, c),
            KeyBinding::new("alt-3", Pick3, c),
            KeyBinding::new("alt-4", Pick4, c),
            KeyBinding::new("alt-5", Pick5, c),
            KeyBinding::new("alt-6", Pick6, c),
            KeyBinding::new("alt-7", Pick7, c),
            KeyBinding::new("alt-8", Pick8, c),
            KeyBinding::new("alt-9", Pick9, c),
        ]);
        Self {
            ctx,
            input,
            filter: Filter::All,
            rows: Vec::new(),
            selected: 0,
            selected_id: None,
            scroll: 0,
            open: false,
            open_seq: 0,
            typed: String::new(),
            matcher: model::matcher(),
            seen_seq: 0,
        }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Clipboard { op } = request else { return None };
        let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
        match (op, open) {
            (PanelOp::Open, true) => {}
            (PanelOp::Open, false) | (PanelOp::Toggle, false) => {
                self.reset(cx);
                self.open = true;
                self.ctx.shell.open_modal(Self::ID, None, cx);
            }
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.open_seq += 1;
        self.input.focus_handle(cx).focus(window, cx);
        self.recompute(cx);
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.reset(cx);
    }
}

impl ClipboardPanel {
    pub(crate) fn state<'a>(&self, cx: &'a gpui::App) -> &'a ClipboardState {
        self.ctx.services.clipboard.state.read(cx)
    }

    /// Fresh view for the next opening: empty search, All, newest entry selected, and the
    /// service's old save results / notes marked as seen.
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.filter = Filter::All;
        self.typed.clear();
        self.input.update(cx, |i, cx| i.set_text("", cx));
        self.selected = 0;
        self.selected_id = None;
        self.scroll = 0;
        let st = self.state(cx);
        self.seen_seq = st.last_save.as_ref().map_or(0, |s| s.seq).max(st.note.as_ref().map_or(0, |n| n.seq));
        self.recompute(cx);
    }

    pub(crate) fn query(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_string()
    }

    fn on_input_event(&mut self, ev: &TextInputEvent, cx: &mut Context<Self>) {
        match ev {
            TextInputEvent::Changed => self.on_changed(cx),
            TextInputEvent::Submit => self.activate_selected(cx),
            TextInputEvent::Cancel => self.ctx.shell.close_modal(cx),
            TextInputEvent::Up => self.move_selection(-1, cx),
            TextInputEvent::Down => self.move_selection(1, cx),
            TextInputEvent::Tab => self.set_filter(self.filter.next(), cx),
            TextInputEvent::Blur => {}
        }
    }

    fn on_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.query(cx);
        // A digit typed into an empty search is a quick pick, not a query.
        if self.typed.is_empty()
            && let Some(n) = quick_pick(&text)
        {
            self.input.update(cx, |i, cx| i.set_text("", cx));
            self.pick(n, cx);
            return;
        }
        self.typed = text;
        self.selected = 0;
        self.selected_id = None;
        self.scroll = 0;
        self.recompute(cx);
    }

    pub(crate) fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.selected = 0;
            self.selected_id = None;
            self.scroll = 0;
            self.recompute(cx);
        }
    }

    /// Re-filter and re-search; the selection follows its entry, or clamps.
    pub(crate) fn recompute(&mut self, cx: &mut Context<Self>) {
        let entries = self.state(cx).entries.clone();
        let query = self.query(cx);
        self.rows = visible(&entries, self.filter, &query, &mut self.matcher);
        let found = self.selected_id.and_then(|id| self.rows.iter().position(|&i| entries[i].id == id));
        self.selected = found.unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
        self.selected_id = self.rows.get(self.selected).map(|&i| entries[i].id);
        self.scroll = scroll_into_view(self.scroll, self.selected, self.rows.len());
        cx.notify();
    }

    pub(crate) fn selected_entry(&self, cx: &gpui::App) -> Option<Entry> {
        let row = *self.rows.get(self.selected)?;
        self.state(cx).entries.get(row).cloned()
    }

    pub(crate) fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        if row < self.rows.len() {
            self.selected = row;
            self.selected_id = self.state(cx).entries.get(self.rows[row]).map(|e| e.id);
            self.scroll = scroll_into_view(self.scroll, row, self.rows.len());
            cx.notify();
        }
    }

    pub(crate) fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.select((self.selected as isize + delta).rem_euclid(n) as usize, cx);
    }

    pub(crate) fn page(&mut self, dir: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let to = (self.selected as isize + dir * MAX_VISIBLE as isize).clamp(0, self.rows.len() as isize - 1);
        self.select(to as usize, cx);
    }

    pub(crate) fn scroll_by(&mut self, rows: isize, cx: &mut Context<Self>) {
        let max = self.rows.len().saturating_sub(MAX_VISIBLE) as isize;
        let next = (self.scroll as isize + rows).clamp(0, max) as usize;
        if next != self.scroll {
            self.scroll = next;
            cx.notify();
        }
    }

    /// Quick pick: the `n`-th (0-based) row of the visible window becomes the clipboard.
    pub(crate) fn pick(&mut self, n: usize, cx: &mut Context<Self>) {
        let row = self.scroll + n;
        if row < self.rows.len() {
            self.select(row, cx);
            self.activate_selected(cx);
        }
    }

    /// Enter / Paste: become the clipboard owner with the selected entry and close.
    pub(crate) fn activate_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected_entry(cx) {
            self.ctx.services.clipboard.handle.activate(e.id);
            self.ctx.shell.close_modal(cx);
        }
    }

    pub(crate) fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected_entry(cx) {
            self.ctx.services.clipboard.handle.set_pinned(e.id, !e.pinned);
        }
    }

    pub(crate) fn remove_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected_entry(cx) {
            self.selected_id = None;
            self.ctx.services.clipboard.handle.remove(e.id);
        }
    }

    pub(crate) fn save_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.selected_entry(cx) {
            self.ctx.services.clipboard.handle.save_default(e.id);
        }
    }

    pub(crate) fn clear_unpinned(&mut self, cx: &mut Context<Self>) {
        self.selected_id = None;
        self.ctx.services.clipboard.handle.clear_unpinned();
        cx.notify();
    }
}
