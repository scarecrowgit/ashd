//! Lock / greeter UI (det-07): clock, avatar + password field, wrong-password shake
//! (`motion::SPRING_SHAKE`), caps-lock / layout badges, session buttons. Drawn with
//! ashd-gfx (vello) because ext-session-lock surfaces cannot be GPUI windows. Shared by
//! `ashd-lock` and `ashd-greeter`.
//!
//! Pure state machine + drawing: no Wayland, no PAM, no D-Bus. The binary translates
//! keys/pointer into [`LockInput`] / [`LockPointer`], executes the returned
//! [`LockAction`]s, feeds results back ([`LockUi::auth_result`]) and redraws when
//! [`LockUi::next_wakeup`] passes or while [`Draw::animating`].
//!
//! Phases: `Idle` → (Enter) `Checking` (spinner, input ignored) → `Unlocked` (300 ms fade,
//! then [`LockUi::unlock_ready`]) or `Failed` (shake + err border ≈1 s, error text,
//! then back to `Idle`). From the 5th failure on, a growing delay blocks submitting.
//!
//! Owner: lock.

use std::time::{Duration, Instant};

use ashd_gfx::{
    Draw, Text,
    anim::{Shake, Tween},
    icon::Icon,
    vello,
};
use ashd_tokens::{
    ThemeName,
    motion::{DUR_SLOW, EASE_OUT_QUAD, SPRING_SHAKE},
};
use zeroize::{Zeroize, Zeroizing};

mod draw;
mod greeter;
mod greeter_draw;
pub mod layout;

pub use greeter::{
    Focus, GreeterAction, GreeterInput, GreeterUi, GreetdEvent, MAX_ANSWER_CHARS, MAX_NAME_CHARS, PromptKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// PAM conversation in progress.
    Checking,
    /// Wrong password: shake, then back to Idle.
    Failed,
    Unlocked,
}

/// Keyboard input already translated by the xkb state of the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockInput {
    Char(char),
    Backspace,
    /// Ctrl+U / Ctrl+W / Ctrl+Backspace: clear the field.
    ClearLine,
    /// Ctrl+A: select everything (the next character replaces it, Backspace clears it).
    SelectAll,
    Submit,
    Escape,
}

/// Pointer input in surface-local logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LockPointer {
    Motion { x: f64, y: f64 },
    Press { x: f64, y: f64 },
    Leave,
}

/// Footer buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Suspend,
    Reboot,
    PowerOff,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Button::Suspend => "Suspend",
            Button::Reboot => "Reboot",
            Button::PowerOff => "Shut down",
        }
    }
    pub fn confirm_label(self) -> &'static str {
        match self {
            Button::Suspend => "Press again to suspend",
            Button::Reboot => "Press again to reboot",
            Button::PowerOff => "Press again to shut down",
        }
    }
}

/// logind action requested by a confirmed footer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Suspend,
    Reboot,
    PowerOff,
}

/// What the binary must do after an input.
#[derive(Debug, PartialEq, Eq)]
pub enum LockAction {
    None,
    Redraw,
    /// Run PAM with this password (zeroized on drop).
    Authenticate(Zeroizing<String>),
    /// Call logind.
    Power(PowerAction),
}

/// Footer status item (only cheap, time-independent data: battery if present).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusItem {
    pub icon: StatusIcon,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusIcon {
    Battery,
    Wifi,
    Volume,
}

/// Failures shown as "attempt N of 5"; from then on each failure adds a delay.
pub const MAX_ATTEMPTS: u32 = 5;
/// Second press of a footer button must come within this.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(3);
/// Field border stays `err` this long after a failure (det-07 `errb`: ~1 s).
pub const ERROR_BORDER: Duration = Duration::from_millis(1000);
/// Caret: CSS `blink 1s steps(1)` = 500 ms on, 500 ms off.
pub const CARET_HALF_PERIOD: Duration = Duration::from_millis(500);
/// Unlock fade-out (TOKENS.md "slow · lock screen").
pub const FADE_OUT: Duration = Duration::from_millis(DUR_SLOW);

/// Delay before the next attempt is accepted after `failures` wrong passwords.
pub fn failure_delay(failures: u32) -> Duration {
    if failures < MAX_ATTEMPTS {
        return Duration::ZERO;
    }
    let steps = (failures - MAX_ATTEMPTS).min(6);
    Duration::from_secs((5u64 << steps).min(300))
}

pub(crate) struct Icons {
    pub field: Icon,
    pub battery: Icon,
    pub wifi: Icon,
    pub volume: Icon,
    pub suspend: Icon,
    pub reboot: Icon,
    pub power_off: Icon,
}

impl Icons {
    fn load() -> Self {
        macro_rules! icon {
            ($n:literal) => {
                Icon::embedded($n, include_str!(concat!("../../../assets/icons/", $n, ".svg")))
            };
        }
        // The icon set's real glyphs (det-07 drew placeholders: pin, pause, activity, minus):
        // field = lock, footer = wifi/bat/vol, buttons = moon (suspend) / reboot / power.
        Self {
            field: icon!("lock"),
            battery: icon!("bat"),
            wifi: icon!("wifi"),
            volume: icon!("vol"),
            suspend: icon!("moon"),
            reboot: icon!("reboot"),
            power_off: icon!("power"),
        }
    }
}

/// Button hit rects (logical px) of the last frame drawn at a given surface size.
pub(crate) type Hits = Vec<((u32, u32), Vec<(Button, [f64; 4])>)>;

pub struct LockUi {
    pub theme: ThemeName,
    /// Login name (PAM user).
    pub user: String,
    /// Shown name (GECOS full name or the login name).
    pub display_name: String,
    /// Host name (caps line under the name).
    pub host: String,
    /// Compositor name for the caps line (`host · niri`).
    pub compositor: String,
    pub phase: Phase,
    pub caps_lock: bool,
    /// Short name of the active keyboard layout ("EN"), badge.
    pub layout: Option<String>,
    /// Error text (wrong password / PAM error).
    pub message: Option<String>,
    /// Informational PAM text (e.g. password expiry warning), shown in the hint line.
    pub info: Option<String>,
    pub failures: u32,
    pub failed_at: Option<Instant>,
    /// Show "Suspend" (logind `CanSuspend` = yes/challenge).
    pub can_suspend: bool,
    pub status: Vec<StatusItem>,
    password: Zeroizing<String>,
    pending_len: usize,
    selected_all: bool,
    shake: Option<Shake>,
    delay_until: Option<Instant>,
    unlocked_at: Option<Instant>,
    checking_since: Option<Instant>,
    last_input: Instant,
    armed: Option<(Button, Instant)>,
    hover: Option<Button>,
    hits: Hits,
    clock: Option<chrono::NaiveDateTime>,
    icons: Icons,
}

impl LockUi {
    pub fn new(theme: ThemeName, user: String) -> Self {
        Self {
            theme,
            display_name: user.clone(),
            user,
            host: String::new(),
            compositor: "niri".into(),
            phase: Phase::Idle,
            caps_lock: false,
            layout: None,
            message: None,
            info: None,
            failures: 0,
            failed_at: None,
            can_suspend: false,
            status: Vec::new(),
            password: Zeroizing::default(),
            pending_len: 0,
            selected_all: false,
            shake: None,
            delay_until: None,
            unlocked_at: None,
            checking_since: None,
            last_input: Instant::now(),
            armed: None,
            hover: None,
            hits: Vec::new(),
            clock: None,
            icons: Icons::load(),
        }
    }

    /// Characters typed (the field shows dots, never the text).
    pub fn password_len(&self) -> usize {
        self.password.chars().count()
    }

    /// Fixed wall-clock time for screenshots/tests (`None` = live local time).
    pub fn set_clock(&mut self, at: Option<chrono::NaiveDateTime>) {
        self.clock = at;
    }

    pub(crate) fn wall_clock(&self) -> chrono::NaiveDateTime {
        self.clock.unwrap_or_else(|| chrono::Local::now().naive_local())
    }

    fn clear_password(&mut self) {
        self.password.zeroize();
        self.selected_all = false;
    }

    pub fn input(&mut self, input: LockInput) -> LockAction {
        self.input_at(input, Instant::now())
    }

    pub fn input_at(&mut self, input: LockInput, now: Instant) -> LockAction {
        if matches!(self.phase, Phase::Checking | Phase::Unlocked) {
            return LockAction::None;
        }
        self.last_input = now;
        self.armed = None;
        let editing = !matches!(input, LockInput::Submit);
        if editing && self.phase == Phase::Failed {
            self.phase = Phase::Idle;
        }
        match input {
            LockInput::Char(c) => {
                if c.is_control() {
                    return LockAction::None;
                }
                if self.selected_all {
                    self.clear_password();
                }
                self.password.push(c);
                self.message = None;
                self.info = None;
                LockAction::Redraw
            }
            LockInput::Backspace => {
                if self.selected_all {
                    self.clear_password();
                } else {
                    self.password.pop();
                }
                LockAction::Redraw
            }
            LockInput::ClearLine | LockInput::Escape => {
                self.clear_password();
                LockAction::Redraw
            }
            LockInput::SelectAll => {
                self.selected_all = !self.password.is_empty();
                LockAction::Redraw
            }
            LockInput::Submit => {
                if self.password.is_empty() {
                    return LockAction::Redraw;
                }
                if self.delay_until.is_some_and(|t| now < t) {
                    // Still in the penalty delay: keep the password, refuse to submit.
                    return LockAction::Redraw;
                }
                self.phase = Phase::Checking;
                self.checking_since = Some(now);
                self.selected_all = false;
                self.pending_len = self.password_len();
                LockAction::Authenticate(std::mem::take(&mut self.password))
            }
        }
    }

    /// Result of the PAM conversation. `message: None` on a plain wrong password (counts
    /// as a failure); `Some(text)` for every other refusal (account expired, PAM error).
    pub fn auth_result(&mut self, ok: bool, message: Option<String>) {
        self.auth_result_at(ok, message, Instant::now());
    }

    pub fn auth_result_at(&mut self, ok: bool, message: Option<String>, now: Instant) {
        self.checking_since = None;
        self.pending_len = 0;
        if ok {
            self.phase = Phase::Unlocked;
            self.unlocked_at = Some(now);
            self.failed_at = None;
            self.message = None;
            return;
        }
        self.phase = Phase::Failed;
        self.failed_at = Some(now);
        self.shake = Some(Shake::new(now, SPRING_SHAKE, layout::SHAKE_PX as f32));
        match message {
            Some(m) => self.message = Some(m),
            None => {
                self.failures += 1;
                let delay = failure_delay(self.failures);
                self.delay_until = (!delay.is_zero()).then(|| now + delay);
                self.message = Some(self.failure_text());
            }
        }
    }

    fn failure_text(&self) -> String {
        if self.failures < MAX_ATTEMPTS {
            format!("Wrong password · attempt {} of {MAX_ATTEMPTS}", self.failures)
        } else {
            format!("Wrong password · attempt {}", self.failures)
        }
    }

    /// The error line as drawn at `now` (includes the delay countdown).
    pub fn error_text(&self, now: Instant) -> Option<String> {
        if let Some(until) = self.delay_until
            && now < until
        {
            let secs = (until - now).as_secs_f32().ceil() as u64;
            return Some(format!("Too many attempts · wait {secs} s"));
        }
        self.message.clone()
    }

    /// Informational text from PAM (`text_info`), shown in the hint line.
    pub fn set_info(&mut self, info: Option<String>) {
        self.info = info;
    }

    /// Footer pointer handling; `size` is the logical size of the surface it happened on.
    pub fn pointer(&mut self, ev: LockPointer, size: (f64, f64)) -> LockAction {
        self.pointer_at(ev, size, Instant::now())
    }

    pub fn pointer_at(&mut self, ev: LockPointer, size: (f64, f64), now: Instant) -> LockAction {
        if self.phase == Phase::Unlocked {
            return LockAction::None;
        }
        match ev {
            LockPointer::Leave => {
                if self.hover.take().is_some() { LockAction::Redraw } else { LockAction::None }
            }
            LockPointer::Motion { x, y } => {
                let hover = self.hit(size, x, y);
                if hover != self.hover {
                    self.hover = hover;
                    LockAction::Redraw
                } else {
                    LockAction::None
                }
            }
            LockPointer::Press { x, y } => match self.hit(size, x, y) {
                Some(b) => {
                    if let Some((armed, at)) = self.armed
                        && armed == b
                        && now.saturating_duration_since(at) <= CONFIRM_WINDOW
                    {
                        self.armed = None;
                        LockAction::Power(match b {
                            Button::Suspend => PowerAction::Suspend,
                            Button::Reboot => PowerAction::Reboot,
                            Button::PowerOff => PowerAction::PowerOff,
                        })
                    } else {
                        self.armed = Some((b, now));
                        LockAction::Redraw
                    }
                }
                None => {
                    if self.armed.take().is_some() { LockAction::Redraw } else { LockAction::None }
                }
            },
        }
    }

    fn hit(&self, size: (f64, f64), x: f64, y: f64) -> Option<Button> {
        let key = size_key(size);
        let (_, rects) = self.hits.iter().find(|(k, _)| *k == key)?;
        rects.iter().find(|(_, [rx, ry, rw, rh])| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(b, _)| *b)
    }

    /// The buttons shown in the footer, left to right.
    pub fn buttons(&self) -> Vec<Button> {
        let mut v = Vec::with_capacity(3);
        if self.can_suspend {
            v.push(Button::Suspend);
        }
        v.extend([Button::Reboot, Button::PowerOff]);
        v
    }

    /// The button waiting for its confirming second press.
    pub fn armed(&self, now: Instant) -> Option<Button> {
        self.armed.filter(|(_, at)| now.saturating_duration_since(*at) <= CONFIRM_WINDOW).map(|(b, _)| b)
    }

    pub fn hovered(&self) -> Option<Button> {
        self.hover
    }

    /// Advance time-based state (call before drawing / after a wakeup).
    pub fn tick(&mut self, now: Instant) {
        if self.armed.is_some() && self.armed(now).is_none() {
            self.armed = None;
        }
        if self.delay_until.is_some_and(|t| now >= t) {
            self.delay_until = None;
        }
        if self.shake.is_some_and(|s| s.settled(now)) {
            self.shake = None;
        }
        if self.phase == Phase::Failed
            && self.shake.is_none()
            && self.failed_at.is_none_or(|t| now.saturating_duration_since(t) >= ERROR_BORDER)
        {
            self.phase = Phase::Idle;
        }
    }

    pub(crate) fn shake_offset(&self, now: Instant) -> f64 {
        self.shake.map_or(0.0, |s| f64::from(s.offset(now)))
    }

    pub(crate) fn error_border(&self, now: Instant) -> bool {
        self.failed_at.is_some_and(|t| now.saturating_duration_since(t) < ERROR_BORDER)
    }

    pub(crate) fn caret_visible(&self, now: Instant) -> bool {
        let half = now.saturating_duration_since(self.last_input).as_millis() / CARET_HALF_PERIOD.as_millis();
        half.is_multiple_of(2)
    }

    /// Dots to draw: the typed length, or while checking the length that was submitted.
    pub(crate) fn shown_len(&self) -> usize {
        if self.phase == Phase::Checking { self.pending_len } else { self.password_len() }
    }

    pub(crate) fn selected_all(&self) -> bool {
        self.selected_all
    }

    pub(crate) fn checking_since(&self) -> Option<Instant> {
        self.checking_since
    }

    /// Content opacity (1 → 0 during the unlock fade).
    pub fn content_alpha(&self, now: Instant) -> f32 {
        match self.unlocked_at {
            Some(at) => 1.0 - Tween::new(at, DUR_SLOW, EASE_OUT_QUAD).value(now),
            None => 1.0,
        }
    }

    /// The fade-out finished: unlock the session and exit.
    pub fn unlock_ready(&self, now: Instant) -> bool {
        self.phase == Phase::Unlocked && self.unlocked_at.is_some_and(|t| now >= t + FADE_OUT)
    }

    /// Animations at `now` (frame-callback driven redraws).
    pub fn animating_at(&self, now: Instant) -> bool {
        self.phase == Phase::Checking
            || self.shake.is_some_and(|s| !s.settled(now))
            || (self.phase == Phase::Unlocked && !self.unlock_ready(now))
    }

    /// Next instant a static frame changes (caret blink, clock minute, countdown, timeouts).
    /// The binary sleeps until then, calls [`LockUi::tick`] and redraws.
    pub fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        let mut next: Vec<Instant> = Vec::new();
        if matches!(self.phase, Phase::Idle | Phase::Failed) {
            let since = now.saturating_duration_since(self.last_input);
            let half = CARET_HALF_PERIOD.as_millis();
            let n = since.as_millis() / half + 1;
            next.push(self.last_input + Duration::from_millis((n * half) as u64));
        }
        if self.clock.is_none() {
            use chrono::Timelike;
            let t = chrono::Local::now();
            let into_minute = Duration::from_secs(u64::from(t.second())) + Duration::from_nanos(u64::from(t.nanosecond() % 1_000_000_000));
            next.push(now + Duration::from_secs(60).saturating_sub(into_minute) + Duration::from_millis(5));
        }
        if let Some((_, at)) = self.armed {
            next.push(at + CONFIRM_WINDOW + Duration::from_millis(1));
        }
        if let Some(until) = self.delay_until {
            // countdown: once per second, and at the end
            let left = until.saturating_duration_since(now);
            let frac = Duration::from_nanos((left.as_nanos() % 1_000_000_000) as u64);
            next.push(now + if frac.is_zero() { Duration::from_secs(1) } else { frac });
            next.push(until);
        }
        if let Some(t) = self.unlocked_at {
            next.push(t + FADE_OUT);
        }
        if let Some(t) = self.failed_at {
            let end = t + ERROR_BORDER;
            if end > now {
                next.push(end);
            }
        }
        next.into_iter().filter(|t| *t > now).min()
    }
}

pub(crate) fn size_key(size: (f64, f64)) -> (u32, u32) {
    (size.0.round() as u32, size.1.round() as u32)
}

impl Draw for LockUi {
    fn draw(&mut self, scene: &mut vello::Scene, text: &mut Text, size: (f64, f64), scale: f64, now: Instant) {
        self.tick(now);
        draw::draw(self, scene, text, size, scale, now);
    }

    fn animating(&self) -> bool {
        self.animating_at(Instant::now())
    }
}

impl LockUi {
    pub(crate) fn icons(&self) -> &Icons {
        &self.icons
    }

    pub(crate) fn set_hits(&mut self, size: (f64, f64), rects: Vec<(Button, [f64; 4])>) {
        let key = size_key(size);
        self.hits.retain(|(k, _)| *k != key);
        self.hits.push((key, rects));
        // keep a handful (outputs); drop the oldest sizes
        if self.hits.len() > 8 {
            self.hits.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(ui: &mut LockUi, s: &str) {
        for c in s.chars() {
            ui.input(LockInput::Char(c));
        }
    }

    #[test]
    fn submit_hands_over_and_clears_the_password() {
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        typed(&mut ui, "hunter2");
        assert_eq!(ui.password_len(), 7);
        let LockAction::Authenticate(pw) = ui.input(LockInput::Submit) else { panic!() };
        assert_eq!(pw.as_str(), "hunter2");
        assert_eq!(ui.password_len(), 0);
        assert_eq!(ui.phase, Phase::Checking);
        assert_eq!(ui.input(LockInput::Submit), LockAction::None, "no double submit while checking");
        assert_eq!(ui.input(LockInput::Char('x')), LockAction::None, "input ignored while checking");
        assert_eq!(ui.password_len(), 0);
    }

    #[test]
    fn editing_keys() {
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        typed(&mut ui, "abc");
        ui.input(LockInput::Backspace);
        assert_eq!(ui.password_len(), 2);
        ui.input(LockInput::ClearLine);
        assert_eq!(ui.password_len(), 0);
        typed(&mut ui, "abc");
        ui.input(LockInput::Escape);
        assert_eq!(ui.password_len(), 0);
        typed(&mut ui, "abc");
        ui.input(LockInput::SelectAll);
        ui.input(LockInput::Char('z'));
        assert_eq!(ui.password_len(), 1, "typing replaces a select-all");
        ui.input(LockInput::SelectAll);
        ui.input(LockInput::Backspace);
        assert_eq!(ui.password_len(), 0);
        assert_eq!(ui.input(LockInput::Char('\u{7f}')), LockAction::None);
        assert_eq!(ui.input(LockInput::Submit), LockAction::Redraw, "empty submit does nothing");
        assert_eq!(ui.phase, Phase::Idle);
    }

    #[test]
    fn wrong_password_shakes_and_counts() {
        let t0 = Instant::now();
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        typed(&mut ui, "nope");
        let _ = ui.input_at(LockInput::Submit, t0);
        ui.auth_result_at(false, None, t0);
        assert_eq!(ui.phase, Phase::Failed);
        assert_eq!(ui.error_text(t0).as_deref(), Some("Wrong password · attempt 1 of 5"));
        assert!(ui.animating_at(t0 + Duration::from_millis(50)));
        assert!(ui.error_border(t0 + Duration::from_millis(900)));
        ui.tick(t0 + Duration::from_millis(1100));
        assert_eq!(ui.phase, Phase::Idle, "back to idle after shake + border");
        assert!(!ui.animating_at(t0 + Duration::from_millis(1100)));
        assert!(ui.message.is_some(), "error text stays until typing");
        ui.input(LockInput::Char('a'));
        assert!(ui.message.is_none());
    }

    #[test]
    fn growing_delay_after_five_failures() {
        assert_eq!(failure_delay(4), Duration::ZERO);
        assert_eq!(failure_delay(5), Duration::from_secs(5));
        assert_eq!(failure_delay(6), Duration::from_secs(10));
        assert_eq!(failure_delay(50), Duration::from_secs(300));
        let mut now = Instant::now();
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        for _ in 0..5 {
            typed(&mut ui, "x");
            assert!(matches!(ui.input_at(LockInput::Submit, now), LockAction::Authenticate(_)));
            ui.auth_result_at(false, None, now);
            now += Duration::from_secs(2);
            ui.tick(now);
        }
        // 5th failure happened 2 s ago → 3 s left
        typed(&mut ui, "y");
        assert_eq!(ui.input_at(LockInput::Submit, now), LockAction::Redraw, "blocked during delay");
        assert_eq!(ui.password_len(), 1, "password kept while blocked");
        assert_eq!(ui.error_text(now).as_deref(), Some("Too many attempts · wait 3 s"));
        now += Duration::from_secs(4);
        ui.tick(now);
        assert!(matches!(ui.input_at(LockInput::Submit, now), LockAction::Authenticate(_)));
    }

    #[test]
    fn pam_errors_do_not_count_as_attempts() {
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        typed(&mut ui, "x");
        let _ = ui.input(LockInput::Submit);
        ui.auth_result(false, Some("Account expired".into()));
        assert_eq!(ui.failures, 0);
        assert_eq!(ui.message.as_deref(), Some("Account expired"));
    }

    #[test]
    fn unlock_fades_then_is_ready() {
        let t0 = Instant::now();
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        typed(&mut ui, "x");
        let _ = ui.input_at(LockInput::Submit, t0);
        ui.auth_result_at(true, None, t0);
        assert_eq!(ui.phase, Phase::Unlocked);
        assert!(!ui.unlock_ready(t0 + Duration::from_millis(100)));
        assert!(ui.animating_at(t0 + Duration::from_millis(100)));
        assert!(ui.content_alpha(t0 + Duration::from_millis(150)) < 1.0);
        assert!(ui.unlock_ready(t0 + FADE_OUT));
        assert_eq!(ui.input(LockInput::Char('a')), LockAction::None);
    }

    #[test]
    fn wakeups_follow_the_caret() {
        let t0 = Instant::now();
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        ui.set_clock(Some(chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap().and_hms_opt(14, 51, 0).unwrap()));
        let _ = ui.input_at(LockInput::Char('a'), t0);
        assert_eq!(ui.next_wakeup(t0), Some(t0 + CARET_HALF_PERIOD));
        assert!(ui.caret_visible(t0 + Duration::from_millis(100)));
        assert!(!ui.caret_visible(t0 + Duration::from_millis(600)));
    }

    #[test]
    fn buttons_need_a_second_press_within_three_seconds() {
        let t0 = Instant::now();
        let mut ui = LockUi::new(ThemeName::Ash, "me".into());
        let size = (1920.0, 1080.0);
        let mut scene = vello::Scene::new();
        let mut text = Text::new();
        ui.draw(&mut scene, &mut text, size, 1.0, t0);
        let (_, rects) = ui.hits.iter().find(|(k, _)| *k == (1920, 1080)).unwrap().clone();
        assert_eq!(rects.len(), 2, "no Suspend unless logind can");
        let (b, [x, y, w, h]) = rects[0];
        assert_eq!(b, Button::Reboot);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        assert!(cy > 1080.0 - 48.0 - 30.0 && cy < 1080.0 - 48.0, "footer row 48 px from the bottom");
        assert_eq!(ui.pointer_at(LockPointer::Press { x: cx, y: cy }, size, t0), LockAction::Redraw);
        assert_eq!(ui.armed(t0), Some(Button::Reboot));
        // too late: re-arms instead of firing
        let late = t0 + Duration::from_secs(4);
        assert_eq!(ui.pointer_at(LockPointer::Press { x: cx, y: cy }, size, late), LockAction::Redraw);
        let soon = late + Duration::from_secs(1);
        assert_eq!(
            ui.pointer_at(LockPointer::Press { x: cx, y: cy }, size, soon),
            LockAction::Power(PowerAction::Reboot)
        );
        // pressing elsewhere disarms
        let _ = ui.pointer_at(LockPointer::Press { x: cx, y: cy }, size, soon);
        assert_eq!(ui.pointer_at(LockPointer::Press { x: 5.0, y: 5.0 }, size, soon), LockAction::Redraw);
        assert_eq!(ui.armed(soon), None);
    }
}
