//! Keybinding cheatsheet (det-11): a centred modal over a scrim. Header "Keys", a filter
//! field and the Scroll / Canvas segmented control; below, the binds of
//! `/etc/ashd/keybinds.json` (override: `$ASHD_KEYBINDS`) as sections in three columns.
//!
//! Typing filters the rows instantly (never animated), Tab / the control switches the
//! mode (canvas groups only under Canvas), Up / Down scroll when the content is taller than
//! the output, Escape closes. `ashd cheatsheet [--open|--close]` toggles it.
//!
//! Owner: cheatsheet. Data (loading, chips, filter, masonry): `keys.rs`; layout: `view.rs`.

pub mod keys;
mod view;

use std::path::PathBuf;

use gpui::{AppContext as _, Context, Entity, Focusable as _, ScrollHandle, Window, point, px};
use ashd_ipc::{PanelOp, Request, Response};
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_tokens::WindowMode;
use ashd_ui::{TextInput, TextInputEvent};

use keys::{LoadError, Section};

/// What the file read gave.
pub(crate) enum Load {
    /// Reading in the background (a few ms).
    Loading,
    Ready(Vec<Section>),
    Missing,
    Invalid(String),
}

pub struct CheatsheetPanel {
    pub(crate) ctx: PanelContext,
    pub(crate) input: Entity<TextInput>,
    pub(crate) load: Load,
    pub(crate) path: PathBuf,
    pub(crate) mode: WindowMode,
    pub(crate) scroll: ScrollHandle,
    /// Bumped per open: a late read of a previous open must not overwrite a newer one.
    seq: u64,
}

impl Panel for CheatsheetPanel {
    const ID: PanelId = PanelId("cheatsheet");
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: true });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("filter", cx));
        cx.subscribe(&input, |this: &mut Self, _, ev: &TextInputEvent, cx| this.on_input_event(ev, cx)).detach();
        Self {
            ctx,
            input,
            load: Load::Loading,
            path: keys::keybinds_path(),
            mode: WindowMode::Scroll,
            scroll: ScrollHandle::new(),
            seq: 0,
        }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Cheatsheet { op } = request else { return None };
        let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
        match (op, open) {
            (PanelOp::Open, _) | (PanelOp::Toggle, false) => self.ctx.shell.open_modal(Self::ID, None, cx),
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.focus_handle(cx).focus(window, cx);
        // Open in the focused workspace's mode (ashd canvas).
        let niri = self.ctx.niri.read(cx);
        let canvas = niri.focused_workspace().is_some_and(|w| niri.is_canvas(w.id));
        self.mode = if canvas { WindowMode::Canvas } else { WindowMode::Scroll };
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.seq += 1;
        let seq = self.seq;
        // Re-read on every open: the NixOS switch may have replaced the file.
        self.path = keys::keybinds_path();
        let path = self.path.clone();
        self.load = Load::Loading;
        let task = cx.background_spawn(async move { keys::load(&path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.load = match result {
                    Ok(binds) => Load::Ready(keys::sections(&binds)),
                    Err(LoadError::Missing) => Load::Missing,
                    Err(LoadError::Invalid(why)) => {
                        tracing::warn!("cheatsheet: {}: {why}", this.path.display());
                        Load::Invalid(why)
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.seq += 1;
        self.input.update(cx, |i, cx| i.set_text("", cx));
    }
}

impl CheatsheetPanel {
    fn on_input_event(&mut self, ev: &TextInputEvent, cx: &mut Context<Self>) {
        match ev {
            // Filtering is instant: the next render reads the text.
            TextInputEvent::Changed => {
                self.scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }
            TextInputEvent::Cancel => self.ctx.shell.close_modal(cx),
            TextInputEvent::Tab => self.set_mode(
                if self.mode == WindowMode::Scroll { WindowMode::Canvas } else { WindowMode::Scroll },
                cx,
            ),
            TextInputEvent::Up => self.scroll_by(-2.0 * keys::ROW_H, cx),
            TextInputEvent::Down => self.scroll_by(2.0 * keys::ROW_H, cx),
            TextInputEvent::Submit | TextInputEvent::Blur => {}
        }
    }

    pub(crate) fn set_mode(&mut self, mode: WindowMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// Positive = further down. GPUI clamps the offset to the content on the next paint.
    fn scroll_by(&mut self, dy: f32, cx: &mut Context<Self>) {
        let off = self.scroll.offset();
        let max = self.scroll.max_offset().y;
        let y = (off.y - px(dy)).clamp(-max, px(0.));
        if y != off.y {
            self.scroll.set_offset(point(off.x, y));
            cx.notify();
        }
    }

    pub(crate) fn query(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_string()
    }
}
