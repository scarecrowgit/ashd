//! Single-line text field (adapted from the gpui `input` example).
//!
//! ```ignore
//! let input = cx.new(|cx| TextInput::new("filter  /", cx));
//! cx.subscribe_in(&input, window, |this, input, ev: &TextInputEvent, window, cx| { .. }).detach();
//! // render: div().child(input.clone());  focus: input.focus_handle(cx).focus(window, cx)
//! ```
//! Font, size and colour are inherited from the parent element's text style.
//! While focused it adds the `TextInput` key context. Call [`bind_keys`] once at startup.
//!
//! Ported from fm (gpui 0.2.2). Changes for the pinned gpui: `ShapedLine::paint` takes
//! `TextAlign` + `align_width`; focusing is `handle.focus(window, cx)`; `IntoElement` is
//! a blanket impl for every `Element` (fm's manual impl removed).

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point,
    prelude::*, px, relative, size,
};
use unicode_segmentation::*;

use crate::{theme::theme, ui::alpha};

use gpui::TextAlign;

actions!(
    text_input,
    [
        Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut,
        Copy, Submit, Escape, Tab, Up, Down, DeleteWordBack
    ]
);

pub fn bind_keys(cx: &mut App) {
    let c = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("ctrl-v", Paste, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-x", Cut, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("enter", Submit, c),
        KeyBinding::new("escape", Escape, c),
        KeyBinding::new("tab", Tab, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("ctrl-backspace", DeleteWordBack, c),
        KeyBinding::new("ctrl-w", DeleteWordBack, c),
    ]);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputEvent {
    /// Text changed (typing, paste, delete, `set_text` does NOT emit).
    Changed,
    /// `enter`
    Submit,
    /// `esc`
    Cancel,
    /// `tab` — autocomplete hook.
    Tab,
    /// `up` / `down` — dropdown navigation hook.
    Up,
    Down,
    /// Lost focus.
    Blur,
}

impl EventEmitter<TextInputEvent> for TextInput {}

/// What a masked [`TextInput`] shows for each character of its secret.
pub const MASK_CHAR: char = '•';
const MASK_LEN: usize = MASK_CHAR.len_utf8();

/// The secret of a masked [`TextInput`] (a password). Lives only in a
/// [`zeroize::Zeroizing`] buffer: every edit builds a fresh exact-size buffer and the old
/// one is wiped, so no stale copy stays on the heap. Deliberately neither `Debug` nor
/// `Clone`; the only way out is [`MaskedText::take`].
pub struct MaskedText {
    secret: zeroize::Zeroizing<String>,
    chars: usize,
}

impl Default for MaskedText {
    fn default() -> Self {
        Self::new()
    }
}

impl MaskedText {
    pub fn new() -> Self {
        Self { secret: zeroize::Zeroizing::new(String::new()), chars: 0 }
    }

    /// Number of characters (= bullets shown).
    pub fn len_chars(&self) -> usize {
        self.chars
    }

    pub fn is_empty(&self) -> bool {
        self.chars == 0
    }

    /// The text shown instead of the secret: one [`MASK_CHAR`] per character.
    pub fn display(&self) -> String {
        MASK_CHAR.to_string().repeat(self.chars)
    }

    /// Replace the characters `chars` (char indices, clamped) with `insert`.
    pub fn splice(&mut self, chars: Range<usize>, insert: &str) {
        let byte = |ci: usize| self.secret.char_indices().nth(ci).map(|(i, _)| i).unwrap_or(self.secret.len());
        let end = chars.end.min(self.chars);
        let start = chars.start.min(end);
        let (a, b) = (byte(start), byte(end));
        let mut next = zeroize::Zeroizing::new(String::with_capacity(self.secret.len() - (b - a) + insert.len()));
        next.push_str(&self.secret[..a]);
        next.push_str(insert);
        next.push_str(&self.secret[b..]);
        self.chars = self.chars - (end - start) + insert.chars().count();
        self.secret = next; // the old buffer is wiped on drop
    }

    /// Move the secret out, leaving this empty.
    pub fn take(&mut self) -> zeroize::Zeroizing<String> {
        self.chars = 0;
        std::mem::replace(&mut self.secret, zeroize::Zeroizing::new(String::new()))
    }

    /// Wipe the secret.
    pub fn clear(&mut self) {
        drop(self.take());
    }
}

/// Display byte range of a masked field → char range of its secret.
fn masked_chars(range: &Range<usize>) -> Range<usize> {
    range.start / MASK_LEN..range.end / MASK_LEN
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    /// Bytes at the end of the text that the cursor cannot enter or edit
    /// (the protected extension in inline rename).
    protected_suffix: usize,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// `Some` in masked (password) mode: the real text; `content` then holds only bullets.
    masked: Option<MaskedText>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        TextInput {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            protected_suffix: 0,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            masked: None,
        }
    }

    /// The text. In masked mode this is the bullets that are shown, never the secret
    /// (use [`Self::take_secret`]).
    pub fn text(&self) -> &str {
        &self.content
    }

    /// Masked (password) mode: typed text goes into a wiped-on-drop [`MaskedText`], the
    /// field renders one [`MASK_CHAR`] per character, and [`Self::text`], the input method
    /// / accessibility (`text_for_range`) and copy / cut only ever see the bullets. No
    /// IME pre-edit. Switching the mode clears the field.
    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        self.masked = masked.then(MaskedText::new);
        self.content = "".into();
        self.protected_suffix = 0;
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    pub fn is_masked(&self) -> bool {
        self.masked.is_some()
    }

    /// Move the text out and clear the field. Masked: the secret itself (never copied);
    /// unmasked: a copy (the field's own string cannot be wiped).
    pub fn take_secret(&mut self, cx: &mut Context<Self>) -> zeroize::Zeroizing<String> {
        let out = match &mut self.masked {
            Some(m) => m.take(),
            None => zeroize::Zeroizing::new(self.content.to_string()),
        };
        self.clear(cx);
        out
    }

    /// Empty the field (wiping a masked secret). Does not emit `Changed`.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.masked {
            m.clear();
        }
        self.content = "".into();
        self.protected_suffix = 0;
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    /// Replace `range` (bytes of `content`) with `new_text`; returns the byte length the
    /// insertion has in `content`.
    fn splice_content(&mut self, range: Range<usize>, new_text: &str) -> usize {
        match &mut self.masked {
            Some(m) => {
                m.splice(masked_chars(&range), new_text);
                self.content = m.display().into();
                new_text.chars().count() * MASK_LEN
            }
            None => {
                self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
                new_text.len()
            }
        }
    }

    /// Replace the text and put the cursor at the end. Does not emit `Changed`. In masked
    /// mode the text becomes the secret — but the `SharedString` passed in cannot be
    /// wiped, so only ever pass `""` there (or use [`Self::clear`]).
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text: SharedString = text.into();
        self.content = match &mut self.masked {
            Some(m) => {
                m.clear();
                m.splice(0..0, &text);
                m.display().into()
            }
            None => text,
        };
        self.protected_suffix = 0;
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    /// Select a byte range (clamped to the editable part, snapped to char boundaries).
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let max = self.editable_len();
        let snap = |mut i: usize| {
            i = i.min(max);
            while i > 0 && !self.content.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        self.selected_range = snap(range.start)..snap(range.end);
        self.selection_reversed = false;
        cx.notify();
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.select_range(0..self.content.len(), cx);
    }

    /// Make the last `bytes` of the text read-only (e.g. `.md` during rename).
    pub fn set_protected_suffix(&mut self, bytes: usize, cx: &mut Context<Self>) {
        if self.masked.is_some() {
            return;
        }
        self.protected_suffix = bytes.min(self.content.len());
        let max = self.editable_len();
        self.selected_range = self.selected_range.start.min(max)..self.selected_range.end.min(max);
        cx.notify();
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn editable_len(&self) -> usize {
        self.content.len() - self.protected_suffix.min(self.content.len())
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submit);
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Cancel);
    }

    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Tab);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Up);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Down);
    }

    fn delete_word_back(&mut self, _: &DeleteWordBack, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let head = &self.content[..cursor];
            let trimmed = head.trim_end_matches(|c: char| c == '/' || c.is_whitespace());
            let start = trimmed.rfind(|c: char| c == '/' || c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
            self.selected_range = start..cursor;
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.editable_len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.editable_len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = zeroize::Zeroizing::new(text);
            let flat = zeroize::Zeroizing::new(text.replace("\n", " "));
            self.replace_text_in_range(None, &flat, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        // A masked field never gives its text away (the bullets are useless anyway).
        if !self.selected_range.is_empty() && self.masked.is_none() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && self.masked.is_none() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.editable_len());
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.editable_len());
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let max = self.editable_len();
        let range = range.start.min(max)..range.end.min(max);

        let inserted = self.splice_content(range.clone(), new_text);
        self.selected_range = range.start + inserted..range.start + inserted;
        self.marked_range.take();
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.masked.is_some() {
            // No pre-edit in a password field: only committed text (via
            // `replace_text_in_range`) reaches the secret.
            self.marked_range = None;
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let max = self.editable_len();
        let range = range.start.min(max)..range.end.min(max);

        self.splice_content(range.clone(), new_text);
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;

        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let t = theme(cx);
        let (accent, muted) = (t.accent, t.fg_muted);
        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), muted)
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(1.5), bounds.bottom() - bounds.top()),
                    ),
                    accent,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    alpha(accent, 0.3),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), TextAlign::Left, None, window, cx)
            .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .overflow_hidden()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::delete_word_back))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(m: &mut MaskedText) -> String {
        m.take().to_string()
    }

    #[test]
    fn masked_text_edits_by_character() {
        let mut m = MaskedText::new();
        m.splice(0..0, "hunter");
        m.splice(6..6, "2");
        assert_eq!((m.len_chars(), m.display()), (7, "•••••••".to_string()));
        // multi-byte characters count once
        m.splice(0..1, "Ĥ");
        m.splice(1..3, "ü");
        assert_eq!(m.len_chars(), 6);
        // out-of-range ranges are clamped
        m.splice(5..99, "!");
        assert_eq!(take(&mut m), "Ĥüter!");
        assert!(m.is_empty());
        assert_eq!(m.display(), "");
    }

    #[test]
    fn masked_ranges_map_bullets_to_chars() {
        let shown = MASK_CHAR.to_string().repeat(4);
        assert_eq!(shown.len(), 4 * MASK_LEN);
        assert_eq!(masked_chars(&(0..shown.len())), 0..4);
        assert_eq!(masked_chars(&(MASK_LEN..3 * MASK_LEN)), 1..3);
        assert_eq!(masked_chars(&(2 * MASK_LEN..2 * MASK_LEN)), 2..2);
    }

    #[test]
    fn take_leaves_nothing_and_clear_wipes() {
        let mut m = MaskedText::new();
        m.splice(0..0, "secret");
        let out = m.take();
        assert_eq!(out.as_str(), "secret");
        assert_eq!(m.len_chars(), 0);
        m.splice(0..0, "again");
        m.clear();
        assert!(m.is_empty());
        assert_eq!(take(&mut m), "");
    }
}
