//! Theme switcher (det-13): five theme cards, a live mini-desktop preview and "apply to"
//! checkboxes, as a centred modal over a scrim. `ashd theme pick [--open|--close]`.
//!
//! Left / Right / 1–5 select a card, Enter applies it, Escape closes; hovering a card
//! previews it (the preview crossfades over 300 ms). Applying = `ashd_ui::set_theme` in this
//! process + `ashd_theme::apply` (persist, write the files of the checked consumers, reload
//! niri / kitty unless `ASHD_NO_RELOAD` / `ASHD_FAKE_POWER`) — the sequence `ashd theme set`
//! and the quick-settings theme row share. The "fm" checkbox decides whether `ashd fm` passes `--theme`.
//!
//! Owner: theme switcher. Consumers / persistence: `consumers.rs`; the mini desktop:
//! `preview.rs`; cards and layout: `view.rs`.

pub mod consumers;
pub mod preview;
mod view;

use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, FocusHandle, KeyDownEvent, Window};
use ashd_ipc::{PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_tokens::{ThemeName, motion};
use ashd_ui::Theme;

use consumers::{ApplyTo, Consumer};

/// A running preview crossfade between two themes.
struct Fade {
    from: Theme,
    to: Theme,
    started: Instant,
}

/// Outcome of the last apply, shown next to the title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Applied { theme: ThemeName, files: usize },
    Failed(String),
}

pub struct ThemeSwitcherPanel {
    pub(crate) ctx: PanelContext,
    pub(crate) focus: FocusHandle,
    /// The five themes with their installed fonts, in `ThemeName::ALL` order.
    pub(crate) themes: [Theme; 5],
    /// Keyboard / click selection (index into `ThemeName::ALL`): the card with the check.
    pub(crate) selected: usize,
    pub(crate) hover: Option<usize>,
    /// The theme the shell is on.
    pub(crate) applied: ThemeName,
    pub(crate) apply_to: ApplyTo,
    pub(crate) status: Option<Status>,
    fade: Fade,
    /// Which card the preview is heading to.
    preview_ix: usize,
    /// Bumped per open: a late checkbox load of a previous open is ignored.
    seq: u64,
}

impl Panel for ThemeSwitcherPanel {
    const ID: PanelId = PanelId("theme-switcher");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: true });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let themes = ThemeName::ALL.map(|name| Theme::resolved(name, cx));
        let applied = ashd_ui::current_theme_name(cx);
        let selected = index_of(applied);
        Self {
            ctx,
            focus: cx.focus_handle(),
            fade: Fade { from: themes[selected].clone(), to: themes[selected].clone(), started: Instant::now() },
            themes,
            selected,
            hover: None,
            applied,
            apply_to: ApplyTo::default(),
            status: None,
            preview_ix: selected,
            seq: 0,
        }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::ThemeSwitcher { op } = request else { return None };
        let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
        match (op, open) {
            (PanelOp::Open, _) | (PanelOp::Toggle, false) => self.ctx.shell.open_modal(Self::ID, None, cx),
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        // Start on the theme the shell is on, without a fade.
        self.applied = ashd_ui::current_theme_name(cx);
        self.selected = index_of(self.applied);
        self.hover = None;
        self.status = None;
        self.themes = ThemeName::ALL.map(|name| Theme::resolved(name, cx));
        self.preview_ix = self.selected;
        let shown = self.themes[self.selected].clone();
        self.fade = Fade { from: shown.clone(), to: shown, started: Instant::now() };
        // The stored checkboxes are read off the main thread.
        self.seq += 1;
        let seq = self.seq;
        let task = cx.background_spawn(async { ApplyTo::load(&consumers::apply_to_path()) });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update(cx, |this, cx| {
                if this.seq == seq {
                    this.apply_to = loaded;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_close(&mut self, _cx: &mut Context<Self>) {
        self.seq += 1;
        self.hover = None;
    }
}

fn index_of(name: ThemeName) -> usize {
    ThemeName::ALL.iter().position(|n| *n == name).unwrap_or(0)
}

impl ThemeSwitcherPanel {
    /// The theme the preview should show: the hovered card, else the selected one.
    fn desired_preview(&self) -> usize {
        self.hover.unwrap_or(self.selected)
    }

    /// Start a crossfade towards the desired card if the target changed.
    fn retarget(&mut self) {
        let want = self.desired_preview();
        if want == self.preview_ix {
            return;
        }
        let from = self.preview_now().0;
        self.fade = Fade { from, to: self.themes[want].clone(), started: Instant::now() };
        self.preview_ix = want;
    }

    /// The crossfaded preview theme right now and whether the fade is still running.
    pub(crate) fn preview_now(&self) -> (Theme, bool) {
        let total = Duration::from_millis(ashd_ui::anim::duration_ms(motion::timing::THEME_CROSSFADE)).max(Duration::from_millis(1));
        let t = (self.fade.started.elapsed().as_secs_f32() / total.as_secs_f32()).min(1.0);
        if t >= 1.0 {
            return (self.fade.to.clone(), false);
        }
        (self.fade.from.lerp(&self.fade.to, motion::EASE_STANDARD.eval(t)), true)
    }

    pub(crate) fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix.min(ThemeName::ALL.len() - 1);
        self.retarget();
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = ThemeName::ALL.len() as isize;
        self.select((self.selected as isize + delta).rem_euclid(n) as usize, cx);
    }

    pub(crate) fn set_hover(&mut self, ix: usize, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hover = Some(ix);
        } else if self.hover == Some(ix) {
            self.hover = None;
        } else {
            return;
        }
        self.retarget();
        cx.notify();
    }

    pub(crate) fn toggle_consumer(&mut self, consumer: Consumer, cx: &mut Context<Self>) {
        if consumer.always_on() {
            return;
        }
        self.apply_to.set(consumer, !self.apply_to.get(consumer));
        let sel = self.apply_to;
        cx.background_spawn(async move {
            if let Err(e) = sel.save(&consumers::apply_to_path()) {
                tracing::warn!("theme switcher: cannot persist the apply-to selection: {e}");
            }
        })
        .detach();
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let m = ev.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        match ev.keystroke.key.as_str() {
            "left" => self.step(-1, cx),
            "right" => self.step(1, cx),
            "enter" => self.apply(cx),
            "escape" => self.ctx.shell.close_modal(cx),
            key => {
                if let Some(n) = key.parse::<usize>().ok().filter(|n| (1..=ThemeName::ALL.len()).contains(n)) {
                    self.select(n - 1, cx);
                }
            }
        }
    }

    /// Apply the selected card: crossfade the shell now; persist, write the files of the
    /// checked consumers and reload them in the background.
    pub(crate) fn apply(&mut self, cx: &mut Context<Self>) {
        let name = ThemeName::ALL[self.selected];
        ashd_ui::set_theme(name, true, cx);
        self.applied = name;
        self.status = None;
        let sel = self.apply_to;
        // The shared sequence (`ashd_theme::apply`): persist, write the checked consumers'
        // files, reload unless ASHD_NO_RELOAD / ASHD_FAKE_POWER.
        let task = cx.background_spawn(async move { ashd_theme::apply(name, &sel, true) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            match &result {
                Ok(a) => {
                    for w in a.reload.iter().flat_map(|r| r.warnings()) {
                        tracing::warn!("theme switcher: {w}");
                    }
                }
                Err(e) => tracing::warn!("theme switcher: cannot write the theme files for {name}: {e}"),
            }
            this.update(cx, |this, cx| {
                this.status = Some(match &result {
                    Ok(a) => Status::Applied { theme: name, files: a.written.len() },
                    Err(e) => Status::Failed(e.to_string()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}
