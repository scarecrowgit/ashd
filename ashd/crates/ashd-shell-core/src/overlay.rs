//! OverlayHost: the two full-output transparent layer surfaces of a display.
//!
//! - **passive** (`overlay-passive`, layer Overlay, keyboard None): toasts, OSD, flyouts.
//!   Exists while any passive panel of the display `is_live`. Pointer input reaches it
//!   only inside the union of the rects its panels mark with [`hit_area`]; everywhere
//!   else clicks fall through to the windows / rail / bar underneath.
//! - **modal** (`overlay-modal`, layer Overlay, keyboard Exclusive): launcher, quick
//!   settings, session menu, popups. Exists only while a modal is open (keyboard
//!   interactivity cannot change on a live layer surface, so it is created on open and
//!   destroyed on close; ~50 ms). A click outside the panel's `.occlude()`d content or
//!   Escape (`ModalDismiss` in key context `AshdModal`) dismisses it.

use std::{collections::HashMap, time::Instant};

use gpui::{
    App, Bounds, Context, Div, Entity, Global, IntoElement, KeyBinding, MouseButton, ParentElement as _, Pixels,
    Styled as _, Window, WindowId, actions, canvas, div, prelude::*,
};
use ashd_tokens::motion::{EASE_OUT_EXPO, EASE_OUT_QUAD, timing};
use ashd_ui::{
    presence::{Presence, Timing},
    theme,
};

use crate::{
    ModalAnchor, ModalState, PanelId, PanelInstance, PanelMotion, Placement, Shell,
    surfaces::{self, SurfaceKind, SurfaceRoot},
};

actions!(ashd_shell, [ModalDismiss]);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", ModalDismiss, Some("AshdModal"))]);
    cx.set_global(ModalKeys::default());
    // Interceptors run before key bindings (and before `TextInput`'s): the only place a
    // repeat inherited from a previous modal can be stopped before it acts.
    cx.intercept_keystrokes(|ev, window, cx| {
        let id = window.window_handle().window_id();
        if let Some(keys) = cx.try_global::<ModalKeys>()
            && keys.is_modal(id)
        {
            let swallow = cx.global_mut::<ModalKeys>().record_down(id, &ev.keystroke);
            if swallow {
                cx.stop_propagation();
            }
        }
    })
    .detach();
}

// ── key repeat across modal surfaces ──────────────────────────────────────────
//
// GPUI's Wayland client starts its key-repeat timer on a press and stops it on the
// release — but it drops a release that arrives while no GPUI window has keyboard focus,
// and destroying the focused window does not stop the timer either. A modal surface
// destroyed between an Escape press and its release (Escape *closes* it) therefore left the
// repeat armed, and the next modal surface to get the keyboard (a reopened launcher, the
// next queued polkit request) received an Escape every ~40 ms until another key was pressed.
//
// The fix, for every modal:
// 1. **Linger.** A modal surface closed while keys pressed in it are still down is not
//    destroyed at once: it stays, empty, click-through and focused, until those keys are
//    released (the release reaches a GPUI window, so the platform repeat stops) or
//    `LINGER_MAX` passes. Opening another modal meanwhile does not wait for it.
// 2. **Inherit.** A modal surface opened while keys pressed in a previous one are still
//    down swallows those keys (their repeats) until they are released or another key is
//    pressed; it never acts on a press it did not see start.

/// Longest a closed modal surface waits for the release of keys pressed in it.
pub const LINGER_MAX: std::time::Duration = std::time::Duration::from_millis(1000);

/// Key state of the modal surfaces (pure bookkeeping, unit-tested).
#[derive(Default, Debug)]
pub(crate) struct ModalKeys {
    /// Live and lingering modal windows.
    windows: std::collections::HashSet<WindowId>,
    /// Keys pressed in a modal window and not released in any modal window yet.
    down: std::collections::HashSet<String>,
    /// Per modal window: keys that were already down when it opened.
    inherited: HashMap<WindowId, std::collections::HashSet<String>>,
}

impl Global for ModalKeys {}

impl ModalKeys {
    fn is_modal(&self, id: WindowId) -> bool {
        self.windows.contains(&id)
    }

    /// A key-down in modal window `id`; true = swallow it (an inherited repeat).
    fn record_down(&mut self, id: WindowId, k: &gpui::Keystroke) -> bool {
        if k.is_modifier_key() {
            return false;
        }
        if self.inherited.get(&id).is_some_and(|s| s.contains(&k.key)) {
            return true;
        }
        // Another key went down: the platform repeat moved to it.
        self.inherited.remove(&id);
        self.down.insert(k.key.clone());
        false
    }

    /// A key-up in any modal window. True when no key pressed in a modal is down anymore.
    fn record_up(&mut self, key: &str) -> bool {
        self.down.remove(key);
        for s in self.inherited.values_mut() {
            s.remove(key);
        }
        self.down.is_empty()
    }

    /// A modal window opened: it inherits the keys still down.
    fn opened(&mut self, id: WindowId) {
        self.windows.insert(id);
        if !self.down.is_empty() {
            self.inherited.insert(id, self.down.clone());
        }
    }

    fn closed(&mut self, id: WindowId) {
        self.windows.remove(&id);
        self.inherited.remove(&id);
    }

    /// No modal window is left: nobody can receive the releases (and GPUI's repeat timer
    /// stops without a focused window), so forget what was down.
    fn forget_if_alone(&mut self) {
        if self.windows.is_empty() {
            self.down.clear();
        }
    }
}

fn modal_key_up(ev: &gpui::KeyUpEvent, cx: &mut App) {
    if cx.try_global::<ModalKeys>().is_none() {
        return;
    }
    if cx.global_mut::<ModalKeys>().record_up(&ev.keystroke.key) {
        // Inside this window's own dispatch: end the lingering ones afterwards.
        cx.defer(|cx| {
            let lingering = std::mem::take(&mut cx.default_global::<Lingering>().0);
            for h in lingering {
                end_linger(h, cx);
            }
        });
    }
}

/// Closed modal surfaces waiting for key releases.
#[derive(Default)]
struct Lingering(Vec<gpui::WindowHandle<SurfaceRoot>>);

impl Global for Lingering {}

fn end_linger(h: gpui::WindowHandle<SurfaceRoot>, cx: &mut App) {
    cx.default_global::<Lingering>().0.retain(|w| *w != h);
    let id = h.window_id();
    forget_window(id, cx);
    surfaces::remove_window(h, SurfaceKind::OverlayModal, 0, cx);
    if let Some(keys) = cx.try_global::<ModalKeys>()
        && keys.is_modal(id)
    {
        let keys = cx.global_mut::<ModalKeys>();
        keys.closed(id);
        keys.forget_if_alone();
    }
    tracing::debug!("modal surface {id:?} removed after linger");
}

/// Destroy every lingering modal surface now (shutdown).
pub(crate) fn end_all_lingering(cx: &mut App) {
    let lingering = std::mem::take(&mut cx.default_global::<Lingering>().0);
    for h in lingering {
        end_linger(h, cx);
    }
}

/// Close the modal surface `h`: at once if no key pressed in a modal is down, else after
/// the release (see the section comment).
fn retire_modal_surface(h: gpui::WindowHandle<SurfaceRoot>, cx: &mut App) {
    let id = h.window_id();
    let keys_down = cx.try_global::<ModalKeys>().is_some_and(|k| !k.down.is_empty());
    if !keys_down {
        end_linger(h, cx);
        return;
    }
    let ok = h
        .update(cx, |root, window, cx| {
            root.lingering = true;
            if let Some(f) = &root.focus {
                f.focus(window, cx);
            }
            cx.notify();
        })
        .is_ok();
    if !ok {
        // Checked out (closing from inside its own dispatch): decide again afterwards.
        cx.defer(move |cx| retire_modal_surface(h, cx));
        return;
    }
    tracing::debug!("modal surface {id:?} lingers until key release");
    cx.default_global::<Lingering>().0.push(h);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(LINGER_MAX).await;
        cx.update(|cx| {
            if cx.default_global::<Lingering>().0.contains(&h) {
                end_linger(h, cx);
            }
        });
    })
    .detach();
}

// ── passive input regions ─────────────────────────────────────────────────────

#[derive(Default)]
struct HitRegions {
    by_window: HashMap<WindowId, Vec<Bounds<Pixels>>>,
}

impl Global for HitRegions {}

#[derive(Default)]
pub(crate) struct HitState {
    pub(crate) applied: Option<Vec<Bounds<Pixels>>>,
}

/// Drop the hit rects recorded for a window that is going away.
pub(crate) fn forget_window(id: WindowId, cx: &mut App) {
    if let Some(h) = cx.try_global::<HitRegions>()
        && h.by_window.contains_key(&id)
    {
        cx.global_mut::<HitRegions>().by_window.remove(&id);
    }
}

/// Wrap the interactive part of a passive-overlay panel (a toast, the OSD, a flyout):
/// its bounds are added to the passive surface's input region every frame. Outside all
/// hit areas the passive overlay is click-through.
///
/// ```ignore
/// ashd_shell_core::hit_area(toast_card.on_click(…))
/// ```
pub fn hit_area(element: impl IntoElement) -> Div {
    div().relative().child(element).child(
        canvas(
            |bounds, window, cx| {
                let id = window.window_handle().window_id();
                cx.default_global::<HitRegions>().by_window.entry(id).or_default().push(bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
}

/// Sticky hover fix, installed by shell-core in **every** surface (bar, rail, scratchpad,
/// both overlays) — panels use plain GPUI `.hover()`, `on_hover`, `group_hover`.
///
/// GPUI keeps a window's last pointer position after `wl_pointer.leave` and hit-tests
/// hover against it, so a control the pointer left through the surface edge (or through
/// the edge of a passive overlay's input region) stays "hovered": its `.hover()` style
/// sticks, `on_hover(false)` never fires, hover-pauses never resume. On the leave this
/// dispatches one synthetic mouse move far outside the surface: every hitbox is then
/// un-hovered exactly as if the pointer had moved away inside the window. Skipped while a
/// button is held (a drag leaving the surface keeps its state).
pub fn pointer_exit_reset() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|ev: &gpui::MouseExitEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble || ev.pressed_button.is_some() {
                    return;
                }
                let modifiers = ev.modifiers;
                window.defer(cx, move |window, cx| {
                    let away = gpui::point(gpui::px(-100_000.), gpui::px(-100_000.));
                    window.dispatch_event(
                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: away, pressed_button: None, modifiers }),
                        cx,
                    );
                });
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_0()
}

pub(crate) fn render_passive(root: &mut SurfaceRoot, _window: &mut Window, cx: &mut Context<SurfaceRoot>) -> impl IntoElement {
    let views = root
        .shell
        .upgrade()
        .and_then(|s| s.read(cx).displays.get(&root.display).map(|d| d.passive_views()))
        .unwrap_or_default();
    let hit = root.hit.clone();
    div()
        .size_full()
        .relative()
        .children(views.into_iter().map(|v| div().absolute().top_0().left_0().size_full().child(v)))
        // Last child: its prepaint runs after every hit_area's, so the region is complete.
        .child(
            canvas(
                move |_, window, cx| {
                    let id = window.window_handle().window_id();
                    let rects = cx.default_global::<HitRegions>().by_window.remove(&id).unwrap_or_default();
                    let mut hit = hit.borrow_mut();
                    if hit.applied.as_ref() != Some(&rects) {
                        window.set_input_region(Some(&rects));
                        hit.applied = Some(rects);
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_0(),
        )
}

/// Create / destroy passive overlays according to `Panel::is_live`.
pub(crate) fn refresh_passive(shell: &Entity<Shell>, cx: &mut App) {
    let wanted: Vec<(gpui::DisplayId, bool, bool)> = {
        let s = shell.read(cx);
        s.displays
            .iter()
            .map(|(id, d)| {
                let live = d.panels.iter().any(|p| p.placement == Placement::Passive && p.is_live(cx));
                (*id, live, d.windows.contains_key(&SurfaceKind::OverlayPassive))
            })
            .collect()
    };
    for (id, live, open) in wanted {
        if live && !open {
            if let Err(e) = surfaces::open_surface(shell, id, SurfaceKind::OverlayPassive, cx) {
                tracing::error!("cannot open passive overlay: {e:#}");
            }
        } else if !live && open {
            surfaces::close_surface(shell, id, SurfaceKind::OverlayPassive, cx);
        }
    }
}

// ── modal ─────────────────────────────────────────────────────────────────────

pub(crate) fn render_modal(root: &mut SurfaceRoot, window: &mut Window, cx: &mut Context<SurfaceRoot>) -> impl IntoElement {
    let focus = root.focus.get_or_insert_with(|| cx.focus_handle()).clone();
    if root.lingering {
        // Closed, waiting for key releases: nothing drawn, nothing clickable, no actions.
        let hit = root.hit.clone();
        return div()
            .size_full()
            .track_focus(&focus)
            .capture_key_up(|ev, _, cx| modal_key_up(ev, cx))
            .child(
                canvas(
                    move |_, window, _| {
                        let mut hit = hit.borrow_mut();
                        if hit.applied.as_ref().is_none_or(|r| !r.is_empty()) {
                            window.set_input_region(Some(&[]));
                            hit.applied = Some(Vec::new());
                        }
                    },
                    |_, _, _, _| {},
                )
                .size_0(),
            )
            .into_any_element();
    }
    let t = theme(cx).clone();
    // Motion: the panel and its scrim follow `presence` (clock-driven; frames are asked
    // for only while it moves).
    let now = Instant::now();
    let (fade, travel) = (root.presence.value(now), root.presence.movement(now));
    if root.presence.is_animating(now) {
        window.request_animation_frame();
    }
    if root.region_closed != root.closing {
        // A leaving panel is click-through: the next click belongs to what is below.
        window.set_input_region(if root.closing { Some(&[]) } else { None });
        root.region_closed = root.closing;
    }
    let closing = root.closing;
    let (view, scrim, slide) = root
        .shell
        .upgrade()
        .and_then(|s| {
            let s = s.read(cx);
            let id = root.modal_panel.or_else(|| s.modal.as_ref().map(|m| m.panel))?;
            let p = s.modal_panels.iter().find(|p| p.id == id)?;
            let scrim = matches!(p.placement, Placement::Modal(style) if style.scrim);
            let slide = p.motion == PanelMotion::Auto && !scrim;
            Some((p.view.clone(), scrim, slide))
        })
        .map(|(v, s, sl)| (Some(v), Some(s), sl))
        .unwrap_or((None, None, false));
    let shell = root.shell.clone();
    // Deferred: these handlers run inside this very window's event dispatch, where
    // `remove_window` on it cannot run (the window is checked out) — closing inline left a
    // zombie Exclusive-keyboard layer surface that swallowed all input.
    let dismiss = move |cx: &mut App| {
        let shell = shell.clone();
        cx.defer(move |cx| {
            if let Some(s) = shell.upgrade() {
                close_modal(&s, cx);
            }
        });
    };
    let dismiss2 = dismiss.clone();
    let dy = if slide { timing::POPUP_SLIDE } else { 0.0 };
    div()
        .size_full()
        .relative()
        .track_focus(&focus)
        // A leaving panel takes no keys: no `AshdModal` context, no actions. (Focus is on
        // this root, so none of the panel's own bindings are in the dispatch path either.)
        .when(!closing, |d| d.key_context("AshdModal").on_action(move |_: &ModalDismiss, _, cx| dismiss(cx)))
        .capture_key_up(|ev, _, cx| modal_key_up(ev, cx))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .when(scrim == Some(true), |d| d.bg(t.scrim_power()).opacity(fade))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| dismiss2(cx))
                .on_mouse_down(MouseButton::Right, |_, _, _| {}),
        )
        .children(view.map(|v| {
            ashd_ui::fx::fade_slide(div().absolute().left_0().size_full().child(v), fade, travel, dy)
        }))
        .into_any_element()
}

/// Timings of a modal surface (dialogs and the launcher class vs popups out of the bar).
fn timings(scrim: bool) -> (Timing, Timing) {
    if scrim {
        (Timing::new(timing::MODAL_IN, EASE_OUT_EXPO), Timing::new(timing::MODAL_OUT, EASE_OUT_QUAD))
    } else {
        (Timing::new(timing::POPUP_IN, EASE_OUT_EXPO), Timing::new(timing::POPUP_OUT, EASE_OUT_QUAD))
    }
}

/// What a fresh modal surface starts with, before it knows its panel: entering, popup timings.
pub(crate) fn initial_presence() -> Presence {
    let (enter, exit) = timings(false);
    Presence::entering(enter, exit, Instant::now())
}

/// Set up a freshly created modal surface for `instance`: which panel it shows, how it
/// moves. The first frame was already drawn (entering, at 0).
fn configure_modal(root: &mut SurfaceRoot, instance: &PanelInstance) {
    let scrim = matches!(instance.placement, Placement::Modal(style) if style.scrim);
    root.modal_panel = Some(instance.id);
    let (enter, exit) = timings(scrim);
    root.presence = if instance.motion == PanelMotion::Instant {
        Presence::present(enter, exit, Instant::now())
    } else {
        Presence::entering(enter, exit, Instant::now())
    };
}

// ── motion: leaving modals ────────────────────────────────────────────────────
//
// Dismissing a modal does not destroy its surface at once: the panel keeps drawing from the
// state it had (so `Panel::on_close`, which resets that state, runs when the exit is over)
// and fades out while the surface takes no input. `Shell::modal` is `None` from the moment
// of the dismissal (toggles, highlights and `ashd status` see it closed); a panel that reads
// `modal_anchor` / `modal_display` while it renders still gets its own. Opening the same
// panel again while it leaves reverses the exit on the same surface.

/// Leaving modal surfaces.
#[derive(Default)]
struct Closing {
    list: Vec<ClosingModal>,
    next: u64,
}

impl Global for Closing {}

struct ClosingModal {
    handle: gpui::WindowHandle<SurfaceRoot>,
    instance: PanelInstance,
    display: gpui::DisplayId,
    anchor: Option<ModalAnchor>,
    token: u64,
}

/// A leaving panel's own anchor (while it renders its exit).
pub(crate) fn closing_anchor(panel: PanelId, cx: &App) -> Option<ModalAnchor> {
    let c = cx.try_global::<Closing>()?;
    c.list.iter().rev().find(|c| c.instance.id == panel).and_then(|c| c.anchor)
}

/// The display of the most recently dismissed modal still leaving.
pub(crate) fn closing_display(cx: &App) -> Option<gpui::DisplayId> {
    cx.try_global::<Closing>()?.list.last().map(|c| c.display)
}

/// Start the exit of modal surface `h` showing `instance`.
fn begin_closing(
    h: gpui::WindowHandle<SurfaceRoot>,
    instance: PanelInstance,
    display: gpui::DisplayId,
    anchor: Option<ModalAnchor>,
    cx: &mut App,
) {
    let now = Instant::now();
    let started = h
        .update(cx, |root, window, cx| {
            root.closing = true;
            root.presence.set_present(false, now);
            // Key-ups (and the Escape that closed it) still have to reach this surface;
            // the panel's own bindings must not.
            if let Some(f) = &root.focus {
                f.focus(window, cx);
            }
            cx.notify();
            root.presence.remaining(now)
        })
        .ok();
    let Some(wait) = started else {
        // Checked out (dismissed from inside its own dispatch): decide again afterwards.
        cx.defer(move |cx| begin_closing(h, instance, display, anchor, cx));
        return;
    };
    let token = {
        let c = cx.default_global::<Closing>();
        c.next += 1;
        c.list.push(ClosingModal { handle: h, instance, display, anchor, token: c.next });
        c.next
    };
    tracing::debug!("modal surface {:?} leaves ({} ms)", h.window_id(), wait.as_millis());
    cx.spawn(async move |cx| {
        cx.background_executor().timer(wait + std::time::Duration::from_millis(16)).await;
        cx.update(|cx| finish_closing(token, cx));
    })
    .detach();
}

/// The exit is over (or is cut short): `on_close`, then the surface goes (waiting for
/// held keys, see "key repeat across modal surfaces").
fn finish_closing(token: u64, cx: &mut App) {
    let c = cx.default_global::<Closing>();
    let Some(ix) = c.list.iter().position(|c| c.token == token) else { return };
    let done = c.list.remove(ix);
    done.instance.on_close(cx);
    retire_modal_surface(done.handle, cx);
}

/// Cut every exit short (on `display`, or everywhere): `on_close` and the surfaces go now.
pub(crate) fn flush_closing(display: Option<gpui::DisplayId>, cx: &mut App) {
    let c = cx.default_global::<Closing>();
    let (done, keep): (Vec<_>, Vec<_>) =
        std::mem::take(&mut c.list).into_iter().partition(|c| display.is_none_or(|d| c.display == d));
    c.list = keep;
    for d in done {
        d.instance.on_close(cx);
        end_linger(d.handle, cx);
    }
}

/// Take the leaving surface of `panel` on `display` back: true when there was one.
fn revive_closing(shell: &Entity<Shell>, panel: PanelId, display: gpui::DisplayId, cx: &mut App) -> bool {
    let c = cx.default_global::<Closing>();
    let Some(ix) = c.list.iter().position(|c| c.instance.id == panel && c.display == display) else { return false };
    let back = c.list.remove(ix);
    let now = Instant::now();
    let instance = back.instance.clone();
    let ok = back
        .handle
        .update(cx, |root, window, cx| {
            root.closing = false;
            root.presence.set_present(true, now);
            window.activate_window();
            instance.on_open(window, cx);
            if window.focused(cx).is_none()
                && let Some(f) = &root.focus
            {
                f.focus(window, cx);
            }
            cx.notify();
        })
        .is_ok();
    if !ok {
        // Cannot be reached any more: let it go the usual way.
        back.instance.on_close(cx);
        retire_modal_surface(back.handle, cx);
        return false;
    }
    shell.update(cx, |s, _| {
        if let Some(ds) = s.displays.get_mut(&display) {
            ds.windows.insert(SurfaceKind::OverlayModal, back.handle);
        }
    });
    tracing::debug!("modal surface {:?}: exit reversed", back.handle.window_id());
    true
}

/// Open `panel` on `on` (a known display), else on the focused display.
pub(crate) fn open_modal(
    shell: &Entity<Shell>,
    panel: PanelId,
    on: Option<gpui::DisplayId>,
    anchor: Option<ModalAnchor>,
    cx: &mut App,
) {
    let (instance, display, prev) = {
        let s = shell.read(cx);
        let Some(instance) = s.modal_panels.iter().find(|p| p.id == panel).cloned() else {
            tracing::warn!("open_modal: `{panel}` is not a registered modal panel");
            return;
        };
        let Some(display) = on.filter(|d| s.displays.contains_key(d)).or_else(|| s.focused_display(cx)) else {
            return;
        };
        (instance, display, s.modal.clone())
    };
    // A bar button dispatching through `ShellHandle::request` supplies the anchor.
    let anchor = anchor.or(shell.read(cx).request_anchor).or_else(|| {
        // Keybind / CLI: the panel's named anchor on that display (`anchors.rs`).
        let info = shell.read(cx).displays.get(&display)?.info.clone();
        crate::anchors::resolve_first(&info, instance.anchors, cx)
    });
    let on = display;
    tracing::debug!("open modal {panel} on {on:?} anchor {anchor:?}");

    if let Some(prev) = prev {
        if prev.panel == panel && prev.display == display {
            // Already open: only the anchor may change.
            shell.update(cx, |s, cx| {
                s.modal = Some(ModalState { panel, display, anchor });
                cx.notify();
            });
            return;
        }
        close_modal(shell, cx);
    }

    // The same panel is still leaving on this display (closed a moment ago): turn that exit
    // around instead of starting a second surface.
    shell.update(cx, |s, _| s.modal = Some(ModalState { panel, display, anchor }));
    if revive_closing(shell, panel, display, cx) {
        shell.update(cx, |_, cx| cx.notify());
        return;
    }
    match surfaces::open_surface(shell, display, SurfaceKind::OverlayModal, cx) {
        Ok(handle) => {
            if let Some(keys) = cx.try_global::<ModalKeys>()
                && !keys.is_modal(handle.window_id())
            {
                cx.global_mut::<ModalKeys>().opened(handle.window_id());
            }
            let _ = handle.update(cx, |root, window, cx| {
                configure_modal(root, &instance);
                window.activate_window();
                instance.on_open(window, cx);
                // Key-ups must reach the surface root (they end a linger) even when the
                // panel focuses nothing.
                if window.focused(cx).is_none()
                    && let Some(f) = &root.focus
                {
                    f.focus(window, cx);
                }
            });
        }
        Err(e) => {
            tracing::error!("cannot open modal overlay: {e:#}");
            shell.update(cx, |s, _| s.modal = None);
        }
    }
    shell.update(cx, |_, cx| cx.notify());
}

pub(crate) fn close_modal(shell: &Entity<Shell>, cx: &mut App) {
    let Some(prev) = shell.update(cx, |s, _| s.modal.take()) else { return };
    let instance = shell.read(cx).modal_panels.iter().find(|p| p.id == prev.panel).cloned();
    // Out of the registry at once (a new modal gets a fresh surface, or takes this one
    // back while it leaves).
    let handle = shell.update(cx, |s, _| s.displays.get_mut(&prev.display)?.windows.remove(&SurfaceKind::OverlayModal));
    match (handle, instance) {
        (Some(h), Some(instance)) if instance.motion != PanelMotion::Instant && !shell.read(cx).shutting_down => {
            tracing::info!("closing {} on {:?}", SurfaceKind::OverlayModal.name(), prev.display);
            begin_closing(h, instance, prev.display, prev.anchor, cx);
        }
        (handle, instance) => {
            if let Some(p) = instance {
                p.on_close(cx);
            }
            // Destroyed now or after the release of the keys still down (see "key repeat
            // across modal surfaces").
            if let Some(h) = handle {
                tracing::info!("closed {} on {:?}", SurfaceKind::OverlayModal.name(), prev.display);
                retire_modal_surface(h, cx);
            }
        }
    }
    shell.update(cx, |_, cx| cx.notify());
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, WindowId};

    use super::ModalKeys;

    fn key(k: &'static str) -> Keystroke {
        Keystroke::parse(k).unwrap()
    }

    #[test]
    fn a_modal_never_acts_on_a_press_it_did_not_see_start() {
        let (a, b) = (WindowId::from(1u64), WindowId::from(2u64));
        let mut keys = ModalKeys::default();
        keys.opened(a);
        // Escape pressed in A (A closes on it), not released yet.
        assert!(!keys.record_down(a, &key("escape")));
        assert!(!keys.down.is_empty(), "A lingers");
        // B (the next queued request / the reopened launcher) opens meanwhile.
        keys.opened(b);
        assert!(keys.record_down(b, &key("escape")), "the repeat is swallowed");
        assert!(keys.record_down(b, &key("escape")));
        // The release ends the linger and B's inheritance.
        assert!(keys.record_up("escape"));
        assert!(!keys.record_down(b, &key("escape")), "a fresh press acts");
    }

    #[test]
    fn another_key_ends_the_inheritance_and_modifiers_are_ignored() {
        let (a, b) = (WindowId::from(1u64), WindowId::from(2u64));
        let mut keys = ModalKeys::default();
        keys.opened(a);
        keys.record_down(a, &key("enter"));
        keys.opened(b);
        assert!(!keys.record_down(b, &key("shift")), "modifiers never count");
        assert!(!keys.record_down(b, &key("a")), "a new key acts");
        assert!(!keys.record_down(b, &key("enter")), "the platform repeat moved to `a`");
    }

    #[test]
    fn nothing_down_without_modal_windows() {
        let a = WindowId::from(1u64);
        let mut keys = ModalKeys::default();
        keys.opened(a);
        keys.record_down(a, &key("escape"));
        keys.closed(a);
        keys.forget_if_alone();
        assert!(keys.down.is_empty());
        let b = WindowId::from(2u64);
        keys.opened(b);
        assert!(!keys.record_down(b, &key("escape")), "a stale press is not inherited");
    }
}
