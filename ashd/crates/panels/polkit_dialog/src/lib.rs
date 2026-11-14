//! Polkit authentication dialog (det-09) for the built-in agent (`ashd-svc-polkit`).
//!
//! Owner: polkit. Contract: ARCHITECTURE.md "Panels" — `PolkitDialogPanel` is registered by
//! `ashd-shell` with `reg.register::<PolkitDialogPanel>()`.
//!
//! A modal with the 60 % scrim, centred, 440 px. It opens by itself while the service has
//! a pending request (one at a time; "+N waiting" when more are queued) and cannot be
//! dismissed by clicking outside: Escape or Cancel cancel the request (so does anything
//! else that closes the modal — `ashd dismiss`, another modal opening). It shows exactly
//! what polkit sent: the message, the action id, and under "details" the requesting
//! processes (`polkit.subject-pid` / `polkit.caller-pid` resolved from `/proc` by the
//! service) and every detail key.
//!
//! The password field is the kit's `TextInput` in masked mode: the typed text lives only
//! in a `Zeroizing` buffer, is moved out on submit (the field is empty from then on) and
//! wiped on cancel / close. Enter = Authenticate. While the helper checks, a spinner
//! shows and the inputs are disabled (focus leaves the field); a wrong password brings
//! the error line and the focus back into the cleared field.

mod view;

use std::collections::HashSet;

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, IntoElement, KeyBinding, Render, Window, actions,
};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_svc_polkit::{AuthRequest, Phase};
use ashd_ui::{TextInput, TextInputEvent};

/// Card width (det-09).
pub const DIALOG_W: f32 = 440.0;

actions!(polkit_dialog, [EscapeRequest]);

/// Where keyboard focus belongs for a phase: the field while a prompt is open, the
/// dialog itself otherwise (checking, starting, helper unavailable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Input,
    Dialog,
}

pub fn focus_target(phase: Phase) -> FocusTarget {
    if phase == Phase::Prompting { FocusTarget::Input } else { FocusTarget::Dialog }
}

/// The field label for the helper's prompt: `Password: ` → `password`; other prompts
/// (one-time codes, …) as sent, without the trailing colon.
pub fn prompt_label(prompt: Option<&str>) -> String {
    let p = prompt.unwrap_or("").trim().trim_end_matches(':').trim();
    if p.is_empty() { "password".into() } else { p.to_lowercase() }
}

/// Label of the primary button.
pub fn primary_label(phase: Phase) -> &'static str {
    if phase == Phase::Unavailable { "Retry" } else { "Authenticate" }
}

/// "+N waiting" for queued requests.
pub fn queue_label(queue_len: usize) -> Option<String> {
    (queue_len > 0).then(|| format!("+{queue_len} waiting"))
}

/// Panel state (fm rule: everything this region needs lives here).
pub struct PolkitDialogPanelState {
    pub input: Entity<TextInput>,
    focus: FocusHandle,
    /// The request on screen.
    shown: Option<u64>,
    /// Requests this dialog cancelled (never re-opened while the service catches up).
    cancelled: HashSet<u64>,
    pub details_open: bool,
    pub menu_open: bool,
    /// Where focus was last put, and for which (request, phase); re-applied on change.
    focused: Option<(u64, Phase, FocusTarget)>,
    /// The field is echoing (`PAM_PROMPT_ECHO_ON`) instead of masked.
    echo: bool,
}

pub struct PolkitDialogPanel {
    pub ctx: PanelContext,
    pub state: PolkitDialogPanelState,
}

impl PolkitDialogPanel {
    fn request(&self, cx: &App) -> Option<AuthRequest> {
        self.ctx.services.polkit.state.read(cx).pending.clone().filter(|r| Some(r.id) == self.state.shown)
    }

    fn is_open(&self, cx: &App) -> bool {
        self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID)
    }

    /// Follow the service: open for a new request, close when none is left.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let pending = self.ctx.services.polkit.state.read(cx).pending.clone();
        let open = self.is_open(cx);
        match pending.filter(|r| !self.state.cancelled.contains(&r.id)) {
            Some(req) => {
                if self.state.shown != Some(req.id) {
                    self.reset(cx);
                    self.state.shown = Some(req.id);
                }
                if req.echo != self.state.echo {
                    self.state.echo = req.echo;
                    self.state.input.update(cx, |i, cx| i.set_masked(!req.echo, cx));
                }
                if !open {
                    self.ctx.shell.open_modal(Self::ID, None, cx);
                }
            }
            None => {
                if self.state.shown.take().is_some() {
                    self.reset(cx);
                    if open {
                        self.ctx.shell.close_modal(cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Forget the request on screen: wipe the field, fold details and the menu.
    fn reset(&mut self, cx: &mut Context<Self>) {
        self.state.input.update(cx, |i, cx| i.clear(cx));
        self.state.details_open = false;
        self.state.menu_open = false;
        self.state.focused = None;
    }

    pub fn authenticate(&mut self, cx: &mut Context<Self>) {
        let Some(req) = self.request(cx) else { return };
        match req.phase {
            Phase::Prompting => {
                let response = self.state.input.update(cx, |i, cx| i.take_secret(cx));
                self.ctx.services.polkit.handle.respond_to(req.id, response);
            }
            Phase::Unavailable => self.ctx.services.polkit.handle.retry(req.id),
            Phase::Starting | Phase::Checking => {}
        }
        self.state.menu_open = false;
        cx.notify();
    }

    /// Escape (in the field, or anywhere on the card): cancel, like the Cancel button. A
    /// held Escape from the previous request's dialog never reaches this one: the modal
    /// host swallows key repeats a modal surface did not see start (ashd-shell-core
    /// `overlay.rs`, "key repeat across modal surfaces").
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    /// Cancel the request on screen and close the dialog.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.state.shown.take() {
            self.state.cancelled.insert(id);
            self.ctx.services.polkit.handle.cancel_request(id);
        }
        self.reset(cx);
        if self.is_open(cx) {
            self.ctx.shell.close_modal(cx);
        }
        cx.notify();
    }

    pub fn select_identity(&mut self, uid: u32, cx: &mut Context<Self>) {
        if let Some(id) = self.state.shown {
            self.state.input.update(cx, |i, cx| i.clear(cx));
            self.ctx.services.polkit.handle.select_identity_for(id, uid);
        }
        self.state.menu_open = false;
        self.state.focused = None;
        cx.notify();
    }

    /// Put the focus where the phase wants it (once per request/phase change).
    fn apply_focus(&mut self, req: &AuthRequest, window: &mut Window, cx: &mut Context<Self>) {
        let target = focus_target(req.phase);
        let key = (req.id, req.phase, target);
        if self.state.focused == Some(key) {
            return;
        }
        self.state.focused = Some(key);
        let handle = match target {
            FocusTarget::Input => self.state.input.focus_handle(cx),
            FocusTarget::Dialog => self.state.focus.clone(),
        };
        window.defer(cx, move |window, cx| handle.focus(window, cx));
    }
}

impl Panel for PolkitDialogPanel {
    const ID: PanelId = PanelId("polkit-dialog");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: true });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut i = TextInput::new("", cx);
            i.set_masked(true, cx);
            i
        });
        cx.subscribe(&input, |this: &mut Self, _, ev: &TextInputEvent, cx| match ev {
            TextInputEvent::Submit => this.authenticate(cx),
            // Escape in the field (it eats `ModalDismiss`).
            TextInputEvent::Cancel => this.escape(cx),
            _ => {}
        })
        .detach();
        cx.observe(&ctx.services.polkit.state, |this, _, cx| this.sync(cx)).detach();
        // Escape on the card itself (focus outside the field) cancels the request, the same
        // path as Escape in the field (instead of the modal host's plain `ModalDismiss`).
        cx.bind_keys([KeyBinding::new("escape", EscapeRequest, Some("PolkitDialog"))]);
        let state = PolkitDialogPanelState {
            input,
            focus: cx.focus_handle(),
            shown: None,
            cancelled: HashSet::new(),
            details_open: false,
            menu_open: false,
            focused: None,
            echo: false,
        };
        // A request may already be pending (service started before the panels).
        cx.defer_in_this(|this: &mut Self, cx| this.sync(cx));
        Self { ctx, state }
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.focused = None;
        if let Some(req) = self.request(cx) {
            self.apply_focus(&req, window, cx);
        } else {
            self.state.focus.focus(window, cx);
        }
    }

    /// Closed by anything (Escape outside the field, `ashd dismiss`, another modal):
    /// the request on screen is cancelled — never left waiting behind a closed dialog.
    fn on_close(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.state.shown.take() {
            let still_pending = self.ctx.services.polkit.state.read(cx).pending.as_ref().is_some_and(|r| r.id == id);
            if still_pending {
                self.state.cancelled.insert(id);
                self.ctx.services.polkit.handle.cancel_request(id);
            }
        }
        self.reset(cx);
        // Anything still queued opens the dialog again.
        cx.defer_in_this(|this: &mut Self, cx| this.sync(cx));
    }
}

/// `cx.defer` for the entity itself.
trait DeferInThis<T> {
    fn defer_in_this(&mut self, f: impl FnOnce(&mut T, &mut Context<T>) + 'static);
}

impl<T: 'static> DeferInThis<T> for Context<'_, T> {
    fn defer_in_this(&mut self, f: impl FnOnce(&mut T, &mut Context<T>) + 'static) {
        let this = self.weak_entity();
        self.defer(move |cx| {
            this.update(cx, f).ok();
        });
    }
}

impl Render for PolkitDialogPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let req = self.request(cx);
        if let Some(req) = &req {
            self.apply_focus(req, window, cx);
        }
        view::render(self, req, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_become_field_labels() {
        assert_eq!(prompt_label(Some("Password: ")), "password");
        assert_eq!(prompt_label(Some("Passwort:")), "passwort");
        assert_eq!(prompt_label(Some("Verification code:")), "verification code");
        assert_eq!(prompt_label(None), "password");
        assert_eq!(prompt_label(Some("  :")), "password");
    }

    #[test]
    fn focus_follows_the_phase() {
        assert_eq!(focus_target(Phase::Prompting), FocusTarget::Input);
        assert_eq!(focus_target(Phase::Checking), FocusTarget::Dialog);
        assert_eq!(focus_target(Phase::Starting), FocusTarget::Dialog);
        assert_eq!(focus_target(Phase::Unavailable), FocusTarget::Dialog);
    }

    #[test]
    fn labels() {
        assert_eq!(primary_label(Phase::Prompting), "Authenticate");
        assert_eq!(primary_label(Phase::Unavailable), "Retry");
        assert_eq!(queue_label(0), None);
        assert_eq!(queue_label(2).as_deref(), Some("+2 waiting"));
    }
}
