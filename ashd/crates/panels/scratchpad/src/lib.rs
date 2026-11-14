//! The right-hand scratchpad: hidden windows, recent images, clipboard (desk-05 "stack",
//! desk-06 / desk-08 "tabs").
//!
//! # Semantics
//!
//! Two different things share the name, and the commands keep them apart:
//!
//! * the **panel** is the 340 px surface on the right (`Slot::Scratchpad`, keyboard
//!   `OnDemand`). `ashd scratch toggle|open|close` shows and hides it. It is shell-only.
//! * **hiding a window** sends it to the compositor's hidden scratch workspace (ashd niri
//!   fork: `workspace "scratch" { hidden }`). `ashd scratch hide [--id N]` hides the
//!   focused window, `ashd scratch show [--id N]` brings back the most recently hidden one.
//!   Both work with the shell running (this panel answers the request after checking the
//!   compositor state) and without it (`ashd-cli` talks to niri directly). On stock niri,
//!   or when the config has no hidden workspace, they fail with a clear error and change
//!   nothing.
//!
//! The panel lists every window of a hidden workspace as a card (click = unhide + focus,
//! the cross = close it), the newest images (clipboard history and `~/Pictures/Screenshots`,
//! scanned off the main thread whenever the panel opens; click = copy, right click = open
//! the folder) and the clipboard history (click = copy). The search field filters all three
//! instantly. While the panel has the keyboard, typing goes to the search field, Up / Down
//! walk the items, Enter activates, Escape clears the search and then hands the keyboard
//! back to the window that had it. The Tabs variant adds a card for the selected clipboard
//! entry (Paste / Pin / Run / Delete; Run only for command lines and only on a second
//! click, because it executes clipboard text).
//!
//! State lives in the services (niri, apps, clipboard); the panel keeps the selection, the
//! search, the tab, the image scan and a few timers. Layout: `view.rs`; pure logic:
//! `model.rs`; screenshots and thumbnails: `images.rs`; key hints: `binds.rs`.

mod binds;
mod images;
mod model;
mod view;

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{AppContext as _, Context, Entity, Focusable as _, ScrollHandle, Window, point, px};
use ashd_ipc::{PanelOp, Request, Response, ScratchOp};
use ashd_niri::niri_ipc::Action;
use ashd_shell_core::{Panel, PanelContext, PanelId, Placement, Slot, config};
use ashd_ui::{TextInput, TextInputEvent};

use binds::Hints;
use images::ImageFile;
use model::{ImageKey, ItemKey, Tab, hidden_window_ids, image_mime, looks_like_command, plan_scratch_window};

/// How long "copied" stays on a row.
const FLASH: Duration = Duration::from_millis(1200);
/// How long an armed destructive button (Run, Clear) waits for its second click.
const ARM: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlashKind {
    Copied,
    Opened,
}

pub struct ScratchpadPanel {
    pub(crate) ctx: PanelContext,
    pub(crate) input: Entity<TextInput>,
    /// Tabs variant: the selected tab.
    pub(crate) tab: Tab,
    /// The item the user chose (click, arrows); see [`model::selection`] for the default.
    pub(crate) selected: Option<ItemKey>,
    pub(crate) scroll: ScrollHandle,
    /// The next render scrolls the selection into view.
    pub(crate) scroll_pending: bool,
    /// The render has seen the panel open (reset when the surface goes away).
    shown: bool,
    /// Restarts the fade-in on every opening.
    pub(crate) open_seq: u64,
    focus_pending: bool,
    /// Since when each hidden window has been hidden (as far as this panel saw).
    pub(crate) hidden_since: HashMap<u64, Instant>,
    niri_seen: bool,
    /// The last window that had the keyboard and is not hidden (Escape gives it back).
    last_window: Option<u64>,
    last_screenshot: Option<String>,
    /// The newest screenshots, from the last scan.
    pub(crate) files: Vec<ImageFile>,
    scan_seq: u64,
    pub(crate) hints: Hints,
    /// Terminal prefix for "Run" when `shell.json` names none.
    terminal: Vec<String>,
    wl_copy: bool,
    pub(crate) flash: Option<(ItemKey, FlashKind)>,
    flash_seq: u64,
    /// The clipboard entry whose "Run" waits for a second click.
    pub(crate) run_armed: Option<u64>,
    /// "Clear history" waits for a second click.
    pub(crate) clear_armed: bool,
    arm_seq: u64,
}

impl Panel for ScratchpadPanel {
    const ID: PanelId = PanelId("scratchpad");
    const PLACEMENT: Placement = Placement::Frame(Slot::Scratchpad);

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("search clipboard, windows, images", cx));
        cx.subscribe(&input, |this: &mut Self, _, ev: &TextInputEvent, cx| this.on_input_event(ev, cx)).detach();
        cx.observe(&ctx.services.niri.state, |this, _, cx| this.on_niri(cx)).detach();
        cx.observe(&ctx.services.apps.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&ctx.services.clipboard.state, |_, _, cx| cx.notify()).detach();
        if let Some(shell) = ctx.shell.entity() {
            // The surface (and with it the render) comes and goes with `scratch_visible`.
            cx.observe(&shell, |this, shell, cx| {
                if !shell.read(cx).scratch_visible {
                    this.shown = false;
                }
            })
            .detach();
        }
        Self {
            ctx,
            input,
            tab: Tab::default(),
            selected: None,
            scroll: ScrollHandle::new(),
            scroll_pending: false,
            shown: false,
            open_seq: 0,
            focus_pending: false,
            hidden_since: HashMap::new(),
            niri_seen: false,
            last_window: None,
            last_screenshot: None,
            files: Vec::new(),
            scan_seq: 0,
            hints: Hints::default(),
            terminal: Vec::new(),
            wl_copy: true,
            flash: None,
            flash_seq: 0,
            run_armed: None,
            clear_armed: false,
            arm_seq: 0,
        }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Scratch(op) = request else { return None };
        match op {
            ScratchOp::Toggle | ScratchOp::Open | ScratchOp::Close => {
                let visible = self.ctx.shell.entity().is_some_and(|s| s.read(cx).scratch_visible);
                let want = match op {
                    ScratchOp::Open => true,
                    ScratchOp::Close => false,
                    _ => !visible,
                };
                self.ctx.shell.set_scratch_visible(want, cx);
                Some(Response::Ok)
            }
            ScratchOp::Hide | ScratchOp::Show | ScratchOp::HideWindow(_) | ScratchOp::ShowWindow(_) => {
                let niri = self.ctx.services.niri.state.read(cx);
                Some(match plan_scratch_window(*op, niri, self.last_window) {
                    Ok(action) => {
                        self.ctx.niri_handle.send_action(action);
                        Response::Ok
                    }
                    Err(why) => Response::error(why),
                })
            }
        }
    }
}

impl ScratchpadPanel {
    // ── opening ───────────────────────────────────────────────────────────────

    /// Called from `render` (it has the `Window`): the first frame after the surface
    /// appeared restarts the fade-in, resets the search and refreshes the data.
    pub(crate) fn ensure_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shown {
            self.shown = true;
            self.open_seq += 1;
            self.selected = None;
            self.run_armed = None;
            self.clear_armed = false;
            self.scroll.set_offset(point(px(0.), px(0.)));
            if !self.input.read(cx).text().is_empty() {
                self.input.update(cx, |i, cx| i.set_text("", cx));
            }
            self.focus_pending = true;
            self.refresh(cx);
        }
        if self.focus_pending {
            self.focus_pending = false;
            self.input.focus_handle(cx).focus(window, cx);
        }
    }

    /// Re-scan the screenshots folder, the key hints and the terminal, off the main thread.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.scan_seq += 1;
        let seq = self.scan_seq;
        let task = cx.background_spawn(async move {
            let files = images::scan(&images::screenshots_dir(), model::MAX_IMAGES, &images::cache_dir());
            let hints = binds::load(&binds::keybinds_path());
            let terminal = ashd_svc_apps::terminal_command();
            let wl_copy = which::which("wl-copy").is_ok();
            (files, hints, terminal, wl_copy)
        });
        cx.spawn(async move |this, cx| {
            let (files, hints, terminal, wl_copy) = task.await;
            this.update(cx, |this, cx| {
                if this.scan_seq != seq {
                    return;
                }
                this.files = files;
                this.hints = hints;
                this.terminal = terminal;
                this.wl_copy = wl_copy;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The niri state changed: remember who has been hidden since when, who had the
    /// keyboard last, and re-scan after a new screenshot.
    fn on_niri(&mut self, cx: &mut Context<Self>) {
        let niri = self.ctx.services.niri.state.read(cx);
        let hidden = hidden_window_ids(niri);
        let first = !self.niri_seen && niri.connected;
        if niri.connected {
            self.niri_seen = true;
        }
        model::track_hidden(&mut self.hidden_since, &hidden, first || !niri.connected, Instant::now());
        if let Some(w) = niri.focused_window().filter(|w| !hidden.contains(&w.id)) {
            self.last_window = Some(w.id);
        }
        if self.last_window.is_some_and(|id| !niri.windows.contains_key(&id)) {
            self.last_window = None;
        }
        let shot = niri.last_screenshot.clone();
        let new_shot = shot != self.last_screenshot;
        self.last_screenshot = shot;
        if new_shot && self.shown {
            self.refresh(cx);
        }
        cx.notify();
    }

    // ── input ─────────────────────────────────────────────────────────────────

    fn on_input_event(&mut self, ev: &TextInputEvent, cx: &mut Context<Self>) {
        match ev {
            TextInputEvent::Changed => {
                self.selected = None;
                self.run_armed = None;
                self.scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }
            TextInputEvent::Up => self.move_selection(-1, cx),
            TextInputEvent::Down => self.move_selection(1, cx),
            TextInputEvent::Submit => self.activate_selected(cx),
            TextInputEvent::Tab => {
                if self.is_tabs(cx) {
                    self.set_tab(self.tab.next(), cx);
                }
            }
            TextInputEvent::Cancel => self.escape(cx),
            TextInputEvent::Blur => {}
        }
    }

    /// Escape: disarm a pending Run / Clear, else clear the search, else give the keyboard
    /// back to the window that had it.
    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.run_armed.take().is_some() || std::mem::take(&mut self.clear_armed) {
            cx.notify();
        } else if !self.input.read(cx).text().is_empty() {
            self.input.update(cx, |i, cx| i.set_text("", cx));
        } else if let Some(id) = self.last_window {
            self.ctx.niri_handle.send_action(Action::FocusWindow { id });
        }
    }

    pub(crate) fn is_tabs(&self, cx: &gpui::App) -> bool {
        ashd_ui::theme(cx).scratchpad == ashd_tokens::ScratchpadVariant::Tabs
    }

    pub(crate) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.selected = None;
            self.run_armed = None;
            self.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }

    // ── selection ─────────────────────────────────────────────────────────────

    pub(crate) fn select(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&key) {
            self.run_armed = None;
        }
        self.selected = Some(key);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let d = self.data(cx);
        if let Some(next) = model::step(d.items.len(), d.selected, delta) {
            self.select(d.items[next].clone(), cx);
            self.scroll_pending = true;
        }
    }

    fn activate_selected(&mut self, cx: &mut Context<Self>) {
        let d = self.data(cx);
        if let Some(key) = d.selected.and_then(|i| d.items.get(i)).cloned() {
            self.activate(key, cx);
        }
    }

    // ── actions ───────────────────────────────────────────────────────────────

    /// The item's primary action: unhide a window, copy an image or a clipboard entry.
    pub(crate) fn activate(&mut self, key: ItemKey, cx: &mut Context<Self>) {
        match &key {
            ItemKey::Window(id) => self.unhide(*id, cx),
            ItemKey::Image(ImageKey::Clip(id)) | ItemKey::Clip(id) => {
                self.ctx.services.clipboard.handle.activate(*id);
                self.flash(key.clone(), FlashKind::Copied, cx);
            }
            ItemKey::Image(ImageKey::File(path)) => {
                let kind = self.copy_file(path.clone(), cx);
                self.flash(key.clone(), kind, cx);
            }
        }
        self.select(key, cx);
    }

    /// Back to its workspace and focused (niri does both).
    pub(crate) fn unhide(&mut self, id: u64, _cx: &mut Context<Self>) {
        self.ctx.niri_handle.send_action(Action::UnhideWindow { id: Some(id) });
    }

    pub(crate) fn close_window(&mut self, id: u64, _cx: &mut Context<Self>) {
        self.ctx.niri_handle.send_action(Action::CloseWindow { id: Some(id) });
    }

    /// The "+" in the Windows header: hide the window that had the keyboard.
    pub(crate) fn hide_last_window(&mut self, cx: &mut Context<Self>) {
        let niri = self.ctx.services.niri.state.read(cx);
        match plan_scratch_window(ScratchOp::Hide, niri, self.last_window) {
            Ok(action) => self.ctx.niri_handle.send_action(action),
            Err(why) => tracing::warn!("scratchpad: {why}"),
        }
    }

    /// Put an image file on the clipboard with `wl-copy`; without it, open the file.
    /// (`Action::Spawn` has no stdin, so the shell does the redirect.)
    fn copy_file(&mut self, path: PathBuf, cx: &mut Context<Self>) -> FlashKind {
        if self.wl_copy {
            let mime = image_mime(&path).to_string();
            let argv = ["sh".to_string(), "-c".into(), "exec wl-copy --type \"$1\" < \"$2\"".into(), "sh".into(), mime, path.display().to_string()];
            self.ctx.shell.spawn(argv, cx);
            FlashKind::Copied
        } else {
            tracing::warn!("scratchpad: wl-copy not found, opening {} instead", path.display());
            self.open_path(&path, cx);
            FlashKind::Opened
        }
    }

    pub(crate) fn open_path(&self, path: &std::path::Path, cx: &gpui::App) {
        self.ctx.shell.spawn(["xdg-open".to_string(), path.display().to_string()], cx);
    }

    /// Secondary action on an image: a screenshot's folder, a clipboard image itself.
    pub(crate) fn open_image_location(&mut self, key: &ImageKey, cx: &mut Context<Self>) {
        match key {
            ImageKey::File(path) => {
                if let Some(dir) = path.parent() {
                    self.open_path(dir, cx);
                }
            }
            ImageKey::Clip(id) => {
                let path = self.ctx.services.clipboard.state.read(cx).entry(*id).and_then(|e| e.image.as_ref().map(|i| i.path.clone()));
                if let Some(path) = path {
                    self.open_path(&path, cx);
                }
            }
        }
    }

    pub(crate) fn open_screenshots_folder(&mut self, cx: &mut Context<Self>) {
        self.open_path(&images::screenshots_dir(), cx);
    }

    pub(crate) fn open_clipboard_manager(&mut self, cx: &mut Context<Self>) {
        self.ctx.shell.request(Request::Clipboard { op: PanelOp::Open }, None, cx);
    }

    pub(crate) fn toggle_pin(&mut self, id: u64, pinned: bool, _cx: &mut Context<Self>) {
        self.ctx.services.clipboard.handle.set_pinned(id, !pinned);
    }

    pub(crate) fn delete_clip(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.selected == Some(ItemKey::Clip(id)) {
            self.selected = None;
        }
        self.ctx.services.clipboard.handle.remove(id);
        cx.notify();
    }

    /// Clear everything unpinned — after a second click.
    pub(crate) fn clear_history(&mut self, cx: &mut Context<Self>) {
        if self.clear_armed {
            self.clear_armed = false;
            self.selected = None;
            self.ctx.services.clipboard.handle.clear_unpinned();
        } else {
            self.clear_armed = true;
            self.disarm_later(cx);
        }
        cx.notify();
    }

    /// Run a command line from the clipboard in the terminal — after a second click.
    pub(crate) fn run_clip(&mut self, id: u64, cx: &mut Context<Self>) {
        let state = self.ctx.services.clipboard.state.read(cx);
        let Some(entry) = state.entry(id).filter(|e| looks_like_command(e)) else { return };
        if self.run_armed != Some(id) {
            self.run_armed = Some(id);
            self.disarm_later(cx);
            cx.notify();
            return;
        }
        let command = entry.preview.trim().to_string();
        self.run_armed = None;
        let mut argv = self.terminal_argv(cx);
        argv.extend([
            "sh".to_string(),
            "-c".into(),
            "eval \"$1\"; printf '\\n[done] press enter to close '; read -r _".into(),
            "sh".into(),
            command,
        ]);
        self.ctx.shell.spawn(argv, cx);
        cx.notify();
    }

    /// `shell.json`'s `apps.terminal`, else the first terminal found at start.
    pub(crate) fn terminal_argv(&self, cx: &gpui::App) -> Vec<String> {
        config(cx)
            .apps
            .terminal
            .as_deref()
            .and_then(ashd_svc_apps::terminal_for)
            .unwrap_or_else(|| self.terminal.clone())
    }

    // ── timers ────────────────────────────────────────────────────────────────

    fn flash(&mut self, key: ItemKey, kind: FlashKind, cx: &mut Context<Self>) {
        self.flash = Some((key, kind));
        self.flash_seq += 1;
        let seq = self.flash_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            this.update(cx, |this, cx| {
                if this.flash_seq == seq {
                    this.flash = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn disarm_later(&mut self, cx: &mut Context<Self>) {
        self.arm_seq += 1;
        let seq = self.arm_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ARM).await;
            this.update(cx, |this, cx| {
                if this.arm_seq == seq && (this.run_armed.is_some() || this.clear_armed) {
                    this.run_armed = None;
                    this.clear_armed = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
