//! Greeter mode: the lock screen's look for a greetd login.
//!
//! [`GreeterUi`] wraps a [`LockUi`] (clock, footer buttons with their confirm-twice logic,
//! shake / error border, failure delays, fade-out, caret blink, wakeups — all reused) and
//! replaces the single password field by what a login needs: an editable user name, a
//! session line (selectable when several sessions are configured) and a prompt field
//! driven by greetd auth messages (secret prompts show dots, visible ones clear text).
//!
//! Still a pure state machine: the binary translates keys / pointer into
//! [`GreeterInput`] / [`LockPointer`], executes the returned [`GreeterAction`]s against the
//! greetd socket, feeds what greetd answers back as [`GreetdEvent`]s, and starts the
//! session (index from [`GreeterUi::take_start`]) once the fade-out finished.
//!
//! Conversation (all greetd round trips are asynchronous, the UI shows the spinner):
//!
//! ```text
//! Closed ──Enter──▶ Opening ──Prompt──▶ Prompting ──Enter──▶ Answering ──Prompt──▶ Prompting …
//!   ▲                  │                    │                    │
//!   │                  └───Authenticated────┴────────────────────┴──▶ Done ──fade──▶ start
//!   └──────────────── Failed (wrong password / error) · Escape / name edit (cancel)
//! ```
//!
//! A password typed *before* greetd asked for it (name, Tab, password, Enter) is held back
//! and answers the first secret prompt.

use std::time::Instant;

use ashd_gfx::{Draw, Text, vello};
use ashd_tokens::ThemeName;
use zeroize::{Zeroize, Zeroizing};

use crate::{LockAction, LockPointer, LockUi, Phase, PowerAction, greeter_draw, size_key};

/// Longest user name accepted by the field.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest answer to a greetd prompt.
pub const MAX_ANSWER_CHARS: usize = 1024;

/// Which field has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Name,
    Prompt,
}

/// What greetd asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// Answer stays secret: dots.
    Secret,
    /// Answer is shown as typed.
    Visible,
}

/// Keyboard input already translated by the xkb state of the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GreeterInput {
    Char(char),
    Backspace,
    /// Ctrl+U / Ctrl+W / Ctrl+Backspace: clear the field.
    ClearLine,
    /// Ctrl+A: select the whole field (the next character replaces it).
    SelectAll,
    Submit,
    /// Cancels the greetd session, or clears the focused field.
    Escape,
    /// Tab: switch between the name and the prompt field.
    Tab,
    /// Shift+Tab.
    BackTab,
    /// Down: next session.
    NextSession,
    /// Up: previous session.
    PrevSession,
}

/// What the binary must do after an input.
#[derive(Debug, PartialEq, Eq)]
pub enum GreeterAction {
    None,
    Redraw,
    /// greetd `create_session` for this user.
    CreateSession(String),
    /// greetd `post_auth_message_response` with this answer (zeroized on drop).
    Answer(Zeroizing<String>),
    /// greetd `cancel_session`.
    Cancel,
    /// Call logind.
    Power(PowerAction),
}

/// What greetd said, already boiled down by the worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GreetdEvent {
    /// A question that needs an answer.
    Prompt { kind: PromptKind, text: String },
    /// `info` / `error` auth message (already acknowledged by the worker).
    Message { error: bool, text: String },
    /// The session is authenticated; `start_session` may be sent.
    Authenticated,
    /// greetd refused (`auth` = `auth_error`, i.e. wrong password) or `start_session` failed.
    /// The conversation is over; the next Enter opens a new one.
    Failed { auth: bool, text: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Conv {
    Closed,
    /// `create_session` sent.
    Opening,
    Prompting {
        kind: PromptKind,
        text: String,
    },
    /// Answer sent, waiting for greetd.
    Answering,
    Done,
}

/// A text field. Zeroizing for the password; the name field simply shares the type.
#[derive(Default)]
struct Field {
    text: Zeroizing<String>,
    selected: bool,
}

impl Field {
    fn len(&self) -> usize {
        self.text.chars().count()
    }
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    fn clear(&mut self) {
        self.text.zeroize();
        self.selected = false;
    }
    fn push(&mut self, c: char, max: usize) {
        if self.selected {
            self.clear();
        }
        if self.len() < max {
            self.text.push(c);
        }
    }
    fn backspace(&mut self) {
        if self.selected {
            self.clear();
        } else {
            self.text.pop();
        }
    }
    fn take(&mut self) -> Zeroizing<String> {
        self.selected = false;
        std::mem::take(&mut self.text)
    }
}

/// Hit rects (logical px) of the interactive parts of the last frame at a size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UiHits {
    pub name: [f64; 4],
    pub field: [f64; 4],
    pub session: Option<[f64; 4]>,
}

pub struct GreeterUi {
    /// The wrapped lock-screen state: theme, host, status items, `can_suspend`, caps-lock
    /// and layout badges, footer, phases. Set those directly.
    pub lock: LockUi,
    sessions: Vec<String>,
    session: usize,
    focus: Focus,
    name: Field,
    answer: Field,
    conv: Conv,
    queued: Option<Zeroizing<String>>,
    pending_len: usize,
    fatal: bool,
    started: bool,
    ui_hits: Vec<((u32, u32), UiHits)>,
}

impl GreeterUi {
    /// `sessions`: names shown on the session line ("niri"); at least one (an empty list
    /// becomes a single "session"). `user`: the remembered user, if any; with one the
    /// password field starts focused, without the (empty) name field.
    pub fn new(theme: ThemeName, sessions: Vec<String>, user: Option<String>) -> Self {
        let sessions = if sessions.is_empty() { vec!["session".to_string()] } else { sessions };
        let mut name = Field::default();
        let user = user.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
        let focus = if let Some(u) = &user {
            for c in u.chars().take(MAX_NAME_CHARS) {
                name.text.push(c);
            }
            Focus::Prompt
        } else {
            Focus::Name
        };
        let mut lock = LockUi::new(theme, String::new());
        lock.compositor = String::new();
        Self {
            lock,
            sessions,
            session: 0,
            focus,
            name,
            answer: Field::default(),
            conv: Conv::Closed,
            queued: None,
            pending_len: 0,
            fatal: false,
            started: false,
            ui_hits: Vec::new(),
        }
    }

    // ── read access ──────────────────────────────────────────────────────────

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The user name field (untrimmed).
    pub fn user(&self) -> &str {
        &self.name.text
    }

    pub fn sessions(&self) -> &[String] {
        &self.sessions
    }

    pub fn session_index(&self) -> usize {
        self.session
    }

    /// Select a session by index (ignored when out of range).
    pub fn set_session(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.session = index;
        }
    }

    pub fn session_name(&self) -> &str {
        &self.sessions[self.session]
    }

    /// Characters in the focused-or-not answer field.
    pub fn answer_len(&self) -> usize {
        self.answer.len()
    }

    /// The answer as typed (only meaningful for visible prompts; tests).
    pub fn answer_text(&self) -> &str {
        &self.answer.text
    }

    /// The kind of the current prompt; a plain password field while there is none.
    pub fn prompt_kind(&self) -> PromptKind {
        match &self.conv {
            Conv::Prompting { kind, .. } => *kind,
            _ => PromptKind::Secret,
        }
    }

    /// Label of the prompt field, e.g. "Password" (greetd's text without the colon).
    pub fn prompt_label(&self) -> String {
        match &self.conv {
            Conv::Prompting { text, .. } => {
                let t = text.trim().trim_end_matches(':').trim();
                if t.is_empty() { "Password".to_string() } else { t.to_string() }
            }
            _ => "Password".to_string(),
        }
    }

    /// A greetd conversation is open (prompting or waiting).
    pub fn conversation_open(&self) -> bool {
        !matches!(self.conv, Conv::Closed)
    }

    pub fn is_fatal(&self) -> bool {
        self.fatal
    }

    pub(crate) fn shown_len(&self) -> usize {
        if self.lock.phase == Phase::Checking { self.pending_len } else { self.answer.len() }
    }

    pub(crate) fn name_selected(&self) -> bool {
        self.name.selected
    }

    pub(crate) fn answer_selected(&self) -> bool {
        self.answer.selected
    }

    /// Whether the session line is selectable.
    pub fn multiple_sessions(&self) -> bool {
        self.sessions.len() > 1
    }

    // ── fatal errors ─────────────────────────────────────────────────────────

    /// No way to log in (no greetd socket, connection lost): show `message` in the error
    /// line and ignore the keyboard. The footer (reboot / shut down) keeps working.
    pub fn set_fatal(&mut self, message: impl Into<String>) {
        self.fatal = true;
        self.conv = Conv::Closed;
        self.queued = None;
        self.lock.phase = Phase::Idle;
        self.lock.checking_since = None;
        self.lock.message = Some(message.into());
    }

    // ── keyboard ─────────────────────────────────────────────────────────────

    pub fn input(&mut self, input: GreeterInput) -> GreeterAction {
        self.input_at(input, Instant::now())
    }

    pub fn input_at(&mut self, input: GreeterInput, now: Instant) -> GreeterAction {
        if self.fatal || matches!(self.lock.phase, Phase::Checking | Phase::Unlocked) {
            return GreeterAction::None;
        }
        self.lock.last_input = now;
        self.lock.armed = None;
        if !matches!(input, GreeterInput::Submit) && self.lock.phase == Phase::Failed {
            self.lock.phase = Phase::Idle;
        }
        match input {
            GreeterInput::Char(c) => {
                if c.is_control() {
                    return GreeterAction::None;
                }
                self.lock.message = None;
                self.lock.info = None;
                let max = if self.focus == Focus::Name { MAX_NAME_CHARS } else { MAX_ANSWER_CHARS };
                self.edit(|f| f.push(c, max))
            }
            GreeterInput::Backspace => self.edit(Field::backspace),
            GreeterInput::ClearLine => self.edit(Field::clear),
            GreeterInput::SelectAll => {
                let f = self.focused_mut();
                f.selected = !f.is_empty();
                GreeterAction::Redraw
            }
            GreeterInput::Tab | GreeterInput::BackTab => {
                self.focus = match self.focus {
                    Focus::Name => Focus::Prompt,
                    Focus::Prompt => Focus::Name,
                };
                self.name.selected = false;
                self.answer.selected = false;
                GreeterAction::Redraw
            }
            GreeterInput::NextSession => self.cycle_session(1),
            GreeterInput::PrevSession => self.cycle_session(-1),
            GreeterInput::Escape => self.escape(),
            GreeterInput::Submit => self.submit(now),
        }
    }

    fn focused_mut(&mut self) -> &mut Field {
        match self.focus {
            Focus::Name => &mut self.name,
            Focus::Prompt => &mut self.answer,
        }
    }

    /// Edit the focused field. Editing the *name* while greetd is asking about the old
    /// one cancels that conversation.
    fn edit(&mut self, f: impl FnOnce(&mut Field)) -> GreeterAction {
        match self.focus {
            Focus::Name => {
                f(&mut self.name);
                if matches!(self.conv, Conv::Prompting { .. }) {
                    self.conv = Conv::Closed;
                    self.answer.clear();
                    return GreeterAction::Cancel;
                }
                GreeterAction::Redraw
            }
            Focus::Prompt => {
                f(&mut self.answer);
                GreeterAction::Redraw
            }
        }
    }

    fn cycle_session(&mut self, by: isize) -> GreeterAction {
        if self.sessions.len() < 2 {
            return GreeterAction::None;
        }
        let n = self.sessions.len() as isize;
        self.session = (self.session as isize + by).rem_euclid(n) as usize;
        GreeterAction::Redraw
    }

    fn escape(&mut self) -> GreeterAction {
        self.lock.message = None;
        self.lock.info = None;
        if matches!(self.conv, Conv::Prompting { .. }) {
            self.conv = Conv::Closed;
            self.answer.clear();
            return GreeterAction::Cancel;
        }
        match self.focus {
            Focus::Prompt if !self.answer.is_empty() => self.answer.clear(),
            Focus::Prompt => {
                // Nothing to clear here: step back to the name.
                self.focus = Focus::Name;
            }
            Focus::Name => self.name.clear(),
        }
        GreeterAction::Redraw
    }

    fn penalty_active(&self, now: Instant) -> bool {
        self.lock.delay_until.is_some_and(|t| now < t)
    }

    fn begin_check(&mut self, now: Instant) {
        self.lock.phase = Phase::Checking;
        self.lock.checking_since = Some(now);
        self.lock.message = None;
        self.lock.info = None;
        self.name.selected = false;
        self.answer.selected = false;
    }

    fn submit(&mut self, now: Instant) -> GreeterAction {
        match self.conv.clone() {
            Conv::Prompting { .. } => {
                if self.answer.is_empty() {
                    self.focus = Focus::Prompt;
                    return GreeterAction::Redraw;
                }
                if self.penalty_active(now) {
                    return GreeterAction::Redraw;
                }
                self.pending_len = self.answer.len();
                self.begin_check(now);
                self.conv = Conv::Answering;
                GreeterAction::Answer(self.answer.take())
            }
            Conv::Closed => {
                let user = self.name.text.trim().to_string();
                if user.is_empty() {
                    self.focus = Focus::Name;
                    return GreeterAction::Redraw;
                }
                if !self.answer.is_empty() {
                    if self.penalty_active(now) {
                        return GreeterAction::Redraw;
                    }
                    self.pending_len = self.answer.len();
                    self.queued = Some(self.answer.take());
                }
                self.begin_check(now);
                self.conv = Conv::Opening;
                GreeterAction::CreateSession(user)
            }
            // Checking / Unlocked are filtered before.
            Conv::Opening | Conv::Answering | Conv::Done => GreeterAction::None,
        }
    }

    // ── greetd ───────────────────────────────────────────────────────────────

    pub fn greetd_event(&mut self, event: GreetdEvent) -> GreeterAction {
        self.greetd_event_at(event, Instant::now())
    }

    pub fn greetd_event_at(&mut self, event: GreetdEvent, now: Instant) -> GreeterAction {
        if self.fatal {
            return GreeterAction::None;
        }
        match event {
            GreetdEvent::Prompt { kind, text } => {
                match self.queued.take() {
                    Some(pw) if kind == PromptKind::Secret => {
                        // The password typed ahead answers the first secret prompt.
                        self.conv = Conv::Answering;
                        return GreeterAction::Answer(pw);
                    }
                    Some(_) => {} // a visible question cannot take a password: dropped
                    None => {}
                }
                self.conv = Conv::Prompting { kind, text };
                self.answer.clear();
                self.focus = Focus::Prompt;
                self.pending_len = 0;
                self.lock.phase = Phase::Idle;
                self.lock.checking_since = None;
                self.lock.last_input = now;
                GreeterAction::Redraw
            }
            GreetdEvent::Message { error, text } => {
                if error {
                    self.lock.message = Some(text);
                } else {
                    self.lock.info = Some(text);
                }
                GreeterAction::Redraw
            }
            GreetdEvent::Authenticated => {
                self.conv = Conv::Done;
                self.queued = None;
                self.lock.auth_result_at(true, None, now);
                GreeterAction::Redraw
            }
            GreetdEvent::Failed { auth, text } => {
                self.conv = Conv::Closed;
                self.queued = None;
                self.started = false;
                self.answer.clear();
                self.lock.unlocked_at = None;
                self.focus = if self.name.is_empty() { Focus::Name } else { Focus::Prompt };
                self.lock.last_input = now;
                let message = if auth {
                    None
                } else if text.trim().is_empty() {
                    Some("Login failed".to_string())
                } else {
                    Some(text)
                };
                self.lock.auth_result_at(false, message, now);
                GreeterAction::Redraw
            }
        }
    }

    /// `Some(session index)` exactly once, when the fade-out after a successful
    /// authentication has finished: send `start_session` for that session now.
    pub fn take_start(&mut self, now: Instant) -> Option<usize> {
        if self.conv == Conv::Done && !self.started && self.lock.unlock_ready(now) {
            self.started = true;
            Some(self.session)
        } else {
            None
        }
    }

    // ── pointer ──────────────────────────────────────────────────────────────

    pub fn pointer(&mut self, ev: LockPointer, size: (f64, f64)) -> GreeterAction {
        self.pointer_at(ev, size, Instant::now())
    }

    pub fn pointer_at(&mut self, ev: LockPointer, size: (f64, f64), now: Instant) -> GreeterAction {
        let footer = self.lock.pointer_at(ev, size, now);
        if let LockPointer::Press { x, y } = ev
            && !self.fatal
            && matches!(self.lock.phase, Phase::Idle | Phase::Failed)
            && let Some(h) = self.hits_at(size)
        {
            let inside = |r: [f64; 4]| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3];
            if inside(h.name) {
                self.focus = Focus::Name;
                self.lock.last_input = now;
                return GreeterAction::Redraw;
            }
            if inside(h.field) {
                self.focus = Focus::Prompt;
                self.lock.last_input = now;
                return GreeterAction::Redraw;
            }
            if let Some(s) = h.session
                && inside(s)
            {
                return self.cycle_session(1);
            }
        }
        match footer {
            LockAction::None => GreeterAction::None,
            LockAction::Redraw => GreeterAction::Redraw,
            LockAction::Power(a) => GreeterAction::Power(a),
            // The lock-screen password path is never used here.
            LockAction::Authenticate(_) => GreeterAction::None,
        }
    }

    fn hits_at(&self, size: (f64, f64)) -> Option<UiHits> {
        let key = size_key(size);
        self.ui_hits.iter().find(|(k, _)| *k == key).map(|(_, h)| *h)
    }

    pub(crate) fn set_ui_hits(&mut self, size: (f64, f64), hits: UiHits) {
        let key = size_key(size);
        self.ui_hits.retain(|(k, _)| *k != key);
        self.ui_hits.push((key, hits));
        if self.ui_hits.len() > 8 {
            self.ui_hits.remove(0);
        }
    }

    // ── time ─────────────────────────────────────────────────────────────────

    pub fn tick(&mut self, now: Instant) {
        self.lock.tick(now);
    }

    pub fn animating_at(&self, now: Instant) -> bool {
        self.lock.animating_at(now)
    }

    /// Next instant a static frame changes (see [`LockUi::next_wakeup`]).
    pub fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        self.lock.next_wakeup(now)
    }

    pub fn set_clock(&mut self, at: Option<chrono::NaiveDateTime>) {
        self.lock.set_clock(at);
    }

    #[cfg(test)]
    pub(crate) fn set_pending_len_for_tests(&mut self, n: usize) {
        self.pending_len = n;
    }
}

impl Draw for GreeterUi {
    fn draw(&mut self, scene: &mut vello::Scene, text: &mut Text, size: (f64, f64), scale: f64, now: Instant) {
        self.lock.tick(now);
        greeter_draw::draw(self, scene, text, size, scale, now);
    }

    fn animating(&self) -> bool {
        self.animating_at(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{FADE_OUT, MAX_ATTEMPTS};

    fn ui() -> GreeterUi {
        GreeterUi::new(ThemeName::Ash, vec!["niri".into()], None)
    }

    fn typed(ui: &mut GreeterUi, s: &str) {
        for c in s.chars() {
            ui.input(GreeterInput::Char(c));
        }
    }

    fn prompt(ui: &mut GreeterUi, kind: PromptKind, text: &str, now: Instant) -> GreeterAction {
        ui.greetd_event_at(GreetdEvent::Prompt { kind, text: text.into() }, now)
    }

    #[test]
    fn starts_on_the_name_field_or_on_the_remembered_user() {
        let ui = ui();
        assert_eq!(ui.focus(), Focus::Name);
        assert_eq!(ui.user(), "");
        let ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("alice".into()));
        assert_eq!(ui.focus(), Focus::Prompt);
        assert_eq!(ui.user(), "alice");
        let ui = GreeterUi::new(ThemeName::Ash, vec![], Some("  ".into()));
        assert_eq!(ui.focus(), Focus::Name, "blank remembered user is ignored");
        assert_eq!(ui.sessions(), ["session"]);
    }

    #[test]
    fn tab_switches_fields_and_typing_goes_to_the_focused_one() {
        let mut ui = ui();
        typed(&mut ui, "alice");
        assert_eq!(ui.user(), "alice");
        assert_eq!(ui.input(GreeterInput::Tab), GreeterAction::Redraw);
        assert_eq!(ui.focus(), Focus::Prompt);
        typed(&mut ui, "pw");
        assert_eq!(ui.answer_len(), 2);
        assert_eq!(ui.user(), "alice");
        ui.input(GreeterInput::BackTab);
        assert_eq!(ui.focus(), Focus::Name);
        ui.input(GreeterInput::Backspace);
        assert_eq!(ui.user(), "alic");
        ui.input(GreeterInput::ClearLine);
        assert_eq!(ui.user(), "");
        assert_eq!(ui.answer_len(), 2, "the other field is untouched");
    }

    #[test]
    fn enter_in_the_name_field_opens_a_session_and_the_prompt_takes_focus() {
        let t0 = Instant::now();
        let mut ui = ui();
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::Redraw, "no name: nothing to open");
        typed(&mut ui, " alice ");
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::CreateSession("alice".into()));
        assert_eq!(ui.lock.phase, Phase::Checking);
        assert_eq!(ui.input_at(GreeterInput::Char('x'), t0), GreeterAction::None, "input ignored while waiting");
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::None);
        assert_eq!(prompt(&mut ui, PromptKind::Secret, "Password: ", t0), GreeterAction::Redraw);
        assert_eq!(ui.lock.phase, Phase::Idle);
        assert_eq!(ui.focus(), Focus::Prompt);
        assert_eq!(ui.prompt_label(), "Password");
        assert_eq!(ui.prompt_kind(), PromptKind::Secret);
        typed(&mut ui, "hunter2");
        let GreeterAction::Answer(pw) = ui.input_at(GreeterInput::Submit, t0) else { panic!() };
        assert_eq!(pw.as_str(), "hunter2");
        assert_eq!(ui.answer_len(), 0, "password handed over");
        assert_eq!(ui.shown_len(), 7, "the dots stay while greetd checks");
        assert_eq!(ui.lock.phase, Phase::Checking);
    }

    #[test]
    fn a_password_typed_ahead_answers_the_first_secret_prompt() {
        let t0 = Instant::now();
        let mut ui = ui();
        typed(&mut ui, "bob");
        ui.input(GreeterInput::Tab);
        typed(&mut ui, "hunter2");
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::CreateSession("bob".into()));
        assert_eq!(ui.shown_len(), 7, "dots stay while the session opens");
        let GreeterAction::Answer(pw) = prompt(&mut ui, PromptKind::Secret, "Password:", t0) else { panic!() };
        assert_eq!(pw.as_str(), "hunter2");
        assert_eq!(ui.lock.phase, Phase::Checking, "still checking: no flash of an empty prompt");
        ui.greetd_event_at(GreetdEvent::Authenticated, t0);
        assert_eq!(ui.lock.phase, Phase::Unlocked);
    }

    #[test]
    fn a_password_typed_ahead_is_dropped_for_a_visible_question() {
        let t0 = Instant::now();
        let mut ui = ui();
        typed(&mut ui, "bob");
        ui.input(GreeterInput::Tab);
        typed(&mut ui, "hunter2");
        let _ = ui.input_at(GreeterInput::Submit, t0);
        assert_eq!(prompt(&mut ui, PromptKind::Visible, "Verification code:", t0), GreeterAction::Redraw);
        assert_eq!(ui.prompt_kind(), PromptKind::Visible);
        assert_eq!(ui.prompt_label(), "Verification code");
        assert_eq!(ui.answer_len(), 0);
        typed(&mut ui, "123456");
        assert_eq!(ui.answer_text(), "123456", "visible answers are readable");
    }

    #[test]
    fn multi_step_conversation() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("carol".into()));
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::CreateSession("carol".into()), "empty Enter opens the session");
        prompt(&mut ui, PromptKind::Secret, "Password:", t0);
        typed(&mut ui, "pw1");
        assert!(matches!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::Answer(_)));
        // second factor
        prompt(&mut ui, PromptKind::Visible, "Code:", t0);
        assert_eq!(ui.lock.phase, Phase::Idle);
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::Redraw, "empty answers are not sent");
        typed(&mut ui, "42");
        let GreeterAction::Answer(a) = ui.input_at(GreeterInput::Submit, t0) else { panic!() };
        assert_eq!(a.as_str(), "42");
        ui.greetd_event_at(GreetdEvent::Authenticated, t0);
        assert_eq!(ui.take_start(t0), None, "fade first");
        assert_eq!(ui.take_start(t0 + FADE_OUT), Some(0));
        assert_eq!(ui.take_start(t0 + FADE_OUT), None, "exactly once");
    }

    #[test]
    fn info_and_error_messages_fill_the_message_lines() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("carol".into()));
        ui.input_at(GreeterInput::Submit, t0);
        ui.greetd_event_at(GreetdEvent::Message { error: false, text: "Password expires in 3 days".into() }, t0);
        assert_eq!(ui.lock.info.as_deref(), Some("Password expires in 3 days"));
        ui.greetd_event_at(GreetdEvent::Message { error: true, text: "Fingerprint not recognised".into() }, t0);
        assert_eq!(ui.lock.message.as_deref(), Some("Fingerprint not recognised"));
        assert_eq!(ui.lock.phase, Phase::Checking, "messages do not end the wait");
        prompt(&mut ui, PromptKind::Secret, "Password:", t0);
        assert!(ui.lock.info.is_some(), "a prompt keeps the messages that came before it");
        typed(&mut ui, "x");
        assert!(ui.lock.info.is_none() && ui.lock.message.is_none(), "typing clears them");
    }

    #[test]
    fn wrong_password_shakes_counts_and_a_new_conversation_starts_on_enter() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("dave".into()));
        typed(&mut ui, "nope");
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::CreateSession("dave".into()));
        let GreeterAction::Answer(_) = prompt(&mut ui, PromptKind::Secret, "Password:", t0) else { panic!() };
        ui.greetd_event_at(GreetdEvent::Failed { auth: true, text: "pam_authenticate: AUTH_ERR".into() }, t0);
        assert_eq!(ui.lock.phase, Phase::Failed);
        assert_eq!(ui.lock.error_text(t0).as_deref(), Some("Wrong password · attempt 1 of 5"));
        assert_eq!(ui.lock.failures, 1);
        assert!(ui.animating_at(t0 + Duration::from_millis(50)), "shake");
        assert!(!ui.conversation_open());
        assert_eq!(ui.focus(), Focus::Prompt);
        assert_eq!(ui.answer_len(), 0);
        ui.tick(t0 + Duration::from_millis(1100));
        assert_eq!(ui.lock.phase, Phase::Idle);
        // try again: password first, then Enter: a fresh session
        typed(&mut ui, "right");
        assert_eq!(ui.input_at(GreeterInput::Submit, t0 + Duration::from_secs(2)), GreeterAction::CreateSession("dave".into()));
        let GreeterAction::Answer(pw) = prompt(&mut ui, PromptKind::Secret, "Password:", t0) else { panic!() };
        assert_eq!(pw.as_str(), "right");
    }

    #[test]
    fn other_errors_do_not_count_as_attempts() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("dave".into()));
        ui.input_at(GreeterInput::Submit, t0);
        ui.greetd_event_at(GreetdEvent::Failed { auth: false, text: "Account expired".into() }, t0);
        assert_eq!(ui.lock.failures, 0);
        assert_eq!(ui.lock.message.as_deref(), Some("Account expired"));
        assert_eq!(ui.lock.phase, Phase::Failed);
        ui.input_at(GreeterInput::Submit, t0);
        ui.greetd_event_at(GreetdEvent::Failed { auth: false, text: "  ".into() }, t0);
        assert_eq!(ui.lock.message.as_deref(), Some("Login failed"));
    }

    #[test]
    fn failure_without_a_name_goes_back_to_the_name_field() {
        let t0 = Instant::now();
        let mut ui = ui();
        typed(&mut ui, "x");
        ui.input_at(GreeterInput::Submit, t0);
        ui.input(GreeterInput::Tab); // ignored: checking
        ui.greetd_event_at(GreetdEvent::Failed { auth: false, text: "No such user".into() }, t0);
        assert_eq!(ui.focus(), Focus::Prompt, "name is not empty");
        ui.input(GreeterInput::Tab);
        ui.input(GreeterInput::ClearLine);
        assert_eq!(ui.user(), "");
        assert_eq!(ui.input(GreeterInput::Submit), GreeterAction::Redraw);
        assert_eq!(ui.focus(), Focus::Name);
    }

    #[test]
    fn growing_delay_blocks_submitting_a_password() {
        let mut now = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("erin".into()));
        for _ in 0..MAX_ATTEMPTS {
            typed(&mut ui, "x");
            assert_eq!(ui.input_at(GreeterInput::Submit, now), GreeterAction::CreateSession("erin".into()));
            let GreeterAction::Answer(_) = prompt(&mut ui, PromptKind::Secret, "Password:", now) else { panic!() };
            ui.greetd_event_at(GreetdEvent::Failed { auth: true, text: String::new() }, now);
            now += Duration::from_secs(2);
            ui.tick(now);
        }
        typed(&mut ui, "y");
        assert_eq!(ui.input_at(GreeterInput::Submit, now), GreeterAction::Redraw, "blocked");
        assert_eq!(ui.answer_len(), 1, "the password is kept");
        assert_eq!(ui.lock.error_text(now).as_deref(), Some("Too many attempts · wait 3 s"));
        now += Duration::from_secs(4);
        ui.tick(now);
        assert!(matches!(ui.input_at(GreeterInput::Submit, now), GreeterAction::CreateSession(_)));
    }

    #[test]
    fn escape_cancels_the_conversation_then_clears() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("frank".into()));
        ui.input_at(GreeterInput::Submit, t0);
        prompt(&mut ui, PromptKind::Secret, "Password:", t0);
        typed(&mut ui, "abc");
        assert_eq!(ui.input(GreeterInput::Escape), GreeterAction::Cancel);
        assert!(!ui.conversation_open());
        assert_eq!(ui.answer_len(), 0);
        // no conversation: Escape clears the focused field, then steps to the name, then clears it
        typed(&mut ui, "abc");
        assert_eq!(ui.input(GreeterInput::Escape), GreeterAction::Redraw);
        assert_eq!(ui.answer_len(), 0);
        assert_eq!(ui.input(GreeterInput::Escape), GreeterAction::Redraw);
        assert_eq!(ui.focus(), Focus::Name);
        assert_eq!(ui.input(GreeterInput::Escape), GreeterAction::Redraw);
        assert_eq!(ui.user(), "");
    }

    #[test]
    fn editing_the_name_cancels_an_open_conversation() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("gina".into()));
        ui.input_at(GreeterInput::Submit, t0);
        prompt(&mut ui, PromptKind::Secret, "Password:", t0);
        ui.input(GreeterInput::Tab);
        assert_eq!(ui.focus(), Focus::Name);
        assert_eq!(ui.input(GreeterInput::Char('x')), GreeterAction::Cancel);
        assert_eq!(ui.user(), "ginax");
        assert!(!ui.conversation_open());
    }

    #[test]
    fn sessions_cycle_only_when_there_are_several() {
        let mut one = ui();
        assert_eq!(one.input(GreeterInput::NextSession), GreeterAction::None);
        assert!(!one.multiple_sessions());
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into(), "sway".into(), "tty".into()], None);
        assert_eq!(ui.session_name(), "niri");
        assert_eq!(ui.input(GreeterInput::NextSession), GreeterAction::Redraw);
        assert_eq!(ui.session_name(), "sway");
        ui.input(GreeterInput::PrevSession);
        ui.input(GreeterInput::PrevSession);
        assert_eq!(ui.session_name(), "tty", "wraps");
        ui.set_session(1);
        assert_eq!(ui.session_index(), 1);
        ui.set_session(9);
        assert_eq!(ui.session_index(), 1, "out of range ignored");
        // the chosen session is the one started
        let t0 = Instant::now();
        ui.greetd_event_at(GreetdEvent::Authenticated, t0);
        assert_eq!(ui.take_start(t0 + FADE_OUT), Some(1));
    }

    #[test]
    fn failed_start_session_returns_to_the_login() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("hal".into()));
        ui.greetd_event_at(GreetdEvent::Authenticated, t0);
        assert_eq!(ui.take_start(t0 + FADE_OUT), Some(0));
        ui.greetd_event_at(GreetdEvent::Failed { auth: false, text: "cannot execute niri-session".into() }, t0 + FADE_OUT);
        assert_eq!(ui.lock.message.as_deref(), Some("cannot execute niri-session"));
        assert_eq!(ui.lock.content_alpha(t0 + FADE_OUT), 1.0, "content is back");
        assert_eq!(ui.take_start(t0 + FADE_OUT), None);
        typed(&mut ui, "x");
        assert!(matches!(ui.input_at(GreeterInput::Submit, t0 + FADE_OUT), GreeterAction::CreateSession(_)));
    }

    #[test]
    fn fatal_blocks_the_keyboard_but_not_the_footer() {
        let t0 = Instant::now();
        let mut ui = ui();
        let size = (1920.0, 1080.0);
        ui.set_fatal("GREETD_SOCK is not set");
        assert!(ui.is_fatal());
        assert_eq!(ui.input_at(GreeterInput::Char('a'), t0), GreeterAction::None);
        assert_eq!(ui.input_at(GreeterInput::Submit, t0), GreeterAction::None);
        assert_eq!(ui.lock.error_text(t0).as_deref(), Some("GREETD_SOCK is not set"));
        assert_eq!(ui.greetd_event_at(GreetdEvent::Authenticated, t0), GreeterAction::None);
        // the footer still reboots (two presses)
        let mut scene = vello::Scene::new();
        let mut text = Text::new();
        ui.draw(&mut scene, &mut text, size, 1.0, t0);
        let (_, rects) = ui.lock.hits.iter().find(|(k, _)| *k == (1920, 1080)).unwrap().clone();
        let (_, [x, y, w, h]) = rects[0];
        let p = LockPointer::Press { x: x + w / 2.0, y: y + h / 2.0 };
        assert_eq!(ui.pointer_at(p, size, t0), GreeterAction::Redraw);
        assert_eq!(ui.pointer_at(p, size, t0), GreeterAction::Power(PowerAction::Reboot));
    }

    #[test]
    fn pointer_focuses_fields_and_cycles_sessions() {
        let t0 = Instant::now();
        let size = (1920.0, 1080.0);
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into(), "sway".into()], Some("ivy".into()));
        let mut scene = vello::Scene::new();
        let mut text = Text::new();
        ui.draw(&mut scene, &mut text, size, 1.0, t0);
        let h = ui.hits_at(size).unwrap();
        let mid = |r: [f64; 4]| (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        assert_eq!(ui.focus(), Focus::Prompt);
        let (x, y) = mid(h.name);
        assert_eq!(ui.pointer_at(LockPointer::Press { x, y }, size, t0), GreeterAction::Redraw);
        assert_eq!(ui.focus(), Focus::Name);
        let (x, y) = mid(h.field);
        ui.pointer_at(LockPointer::Press { x, y }, size, t0);
        assert_eq!(ui.focus(), Focus::Prompt);
        let (x, y) = mid(h.session.expect("two sessions: selectable"));
        ui.pointer_at(LockPointer::Press { x, y }, size, t0);
        assert_eq!(ui.session_name(), "sway");
        // a single session has no hit rect
        let mut one = ui_with(Some("ivy"));
        one.draw(&mut scene, &mut text, size, 1.0, t0);
        assert!(one.hits_at(size).unwrap().session.is_none());
    }

    fn ui_with(user: Option<&str>) -> GreeterUi {
        GreeterUi::new(ThemeName::Ash, vec!["niri".into()], user.map(String::from))
    }

    #[test]
    fn select_all_replaces_the_field() {
        let mut ui = ui();
        typed(&mut ui, "abc");
        ui.input(GreeterInput::SelectAll);
        ui.input(GreeterInput::Char('z'));
        assert_eq!(ui.user(), "z");
        ui.input(GreeterInput::SelectAll);
        ui.input(GreeterInput::Backspace);
        assert_eq!(ui.user(), "");
    }

    #[test]
    fn name_length_is_capped() {
        let mut ui = ui();
        for _ in 0..200 {
            ui.input(GreeterInput::Char('a'));
        }
        assert_eq!(ui.user().chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn spinner_runs_while_waiting_and_wakeups_follow_the_caret() {
        let t0 = Instant::now();
        let mut ui = GreeterUi::new(ThemeName::Ash, vec!["niri".into()], Some("jo".into()));
        ui.set_clock(Some(chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap().and_hms_opt(14, 51, 0).unwrap()));
        ui.input_at(GreeterInput::Char('a'), t0);
        assert_eq!(ui.next_wakeup(t0), Some(t0 + crate::CARET_HALF_PERIOD));
        ui.input_at(GreeterInput::Submit, t0);
        assert!(ui.animating_at(t0), "spinner");
        ui.set_pending_len_for_tests(3);
        assert_eq!(ui.shown_len(), 3);
    }
}
