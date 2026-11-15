//! Hover state shared by the rail (frame surface) and its flyout (passive overlay) of one
//! display. The two are separate panels on separate surfaces; they meet in this entity,
//! found through a per-display registry global (weak, so it dies with the panels).
//!
//! Timing (sys-06 "Rail submenu": 120 in · 100 out): entering a button shows the flyout at
//! once (the passive overlay is created on demand); leaving the rail *and* the flyout
//! starts a short grace period, then the 100 ms exit, then the overlay goes away.

use std::{collections::HashMap, time::Duration};

use gpui::{App, AppContext as _, Context, DisplayId, Entity, Global, WeakEntity};
use ashd_shell_core::ShellHandle;
use ashd_tokens::motion::timing;

/// Pointer may travel between rail and flyout (two surfaces) for this long.
const GRACE: Duration = Duration::from_millis(140);

pub struct RailHover {
    shell: ShellHandle,
    /// Workspace the flyout shows, and the rail-local top of its entry.
    pub shown: Option<(u64, f32)>,
    pub in_rail: bool,
    pub in_flyout: bool,
    /// Exit animation running.
    pub closing: bool,
    /// Bumped on every show/close transition: animation ids and stale timers.
    pub generation: u64,
}

impl RailHover {
    pub fn is_live(&self) -> bool {
        self.shown.is_some()
    }

    /// Pointer entered the entry of workspace `ws`, whose top is `top` (rail-local px).
    pub fn enter_button(&mut self, ws: u64, top: f32, cx: &mut Context<Self>) {
        self.in_rail = true;
        if self.shown != Some((ws, top)) || self.closing {
            let was_live = self.is_live();
            self.shown = Some((ws, top));
            self.closing = false;
            self.generation += 1;
            cx.notify();
            if !was_live {
                self.shell.refresh_overlays(cx);
            }
        }
    }

    pub fn leave_rail(&mut self, cx: &mut Context<Self>) {
        self.in_rail = false;
        self.schedule_check(cx);
    }

    pub fn set_in_flyout(&mut self, inside: bool, cx: &mut Context<Self>) {
        self.in_flyout = inside;
        if inside {
            if self.closing {
                self.closing = false;
                self.generation += 1;
                cx.notify();
            }
        } else {
            self.schedule_check(cx);
        }
    }

    /// Hide immediately (an action in the flyout was taken).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.shown.take().is_some() {
            self.closing = false;
            self.in_flyout = false;
            self.generation += 1;
            cx.notify();
            self.shell.refresh_overlays(cx);
        }
    }

    fn schedule_check(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GRACE).await;
            let closing = this
                .update(cx, |h, cx| {
                    if h.generation != generation || h.in_rail || h.in_flyout || h.shown.is_none() || h.closing {
                        return None;
                    }
                    h.closing = true;
                    h.generation += 1;
                    cx.notify();
                    Some(h.generation)
                })
                .ok()
                .flatten();
            let Some(generation) = closing else { return };
            cx.background_executor().timer(Duration::from_millis(timing::SUBMENU_OUT)).await;
            this.update(cx, |h, cx| {
                if h.generation == generation && h.closing {
                    h.closing = false;
                    h.shown = None;
                    h.generation += 1;
                    cx.notify();
                    h.shell.refresh_overlays(cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

#[derive(Default)]
struct Registry(HashMap<DisplayId, WeakEntity<RailHover>>);

impl Global for Registry {}

/// The hover entity of `display` (created by whichever panel asks first).
pub fn for_display(display: Option<DisplayId>, shell: &ShellHandle, cx: &mut App) -> Entity<RailHover> {
    let make = |cx: &mut App| {
        cx.new(|_| RailHover {
            shell: shell.clone(),
            shown: None,
            in_rail: false,
            in_flyout: false,
            closing: false,
            generation: 0,
        })
    };
    let Some(display) = display else { return make(cx) };
    if let Some(e) = cx.default_global::<Registry>().0.get(&display).and_then(|w| w.upgrade()) {
        return e;
    }
    let e = make(cx);
    let reg = cx.default_global::<Registry>();
    reg.0.retain(|_, w| w.upgrade().is_some());
    reg.0.insert(display, e.downgrade());
    e
}
