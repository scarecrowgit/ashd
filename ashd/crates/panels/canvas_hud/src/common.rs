//! Pieces shared by the canvas/strip passive panels (canvas HUD, minimap, strip header):
//! the main-area rectangle, the shared live-view entity, real key hints from
//! `keybinds.json`, label/button styling, sticky-hover-free hover tracking, and running a
//! niri action on a specific workspace.

use std::{path::PathBuf, sync::Arc};

use gpui::{
    App, AppContext as _, Bounds, Context, Div, ElementId, Entity, Global, Hsla, InteractiveElement as _, IntoElement,
    MouseExitEvent, ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _, Styled as _, Window, canvas,
    div, point, px, size,
};
use ashd_niri::{
    LiveViews, NiriHandle, NiriState,
    niri_ipc::{Action, Window as NiriWindow, Workspace, WorkspaceReferenceArg},
};
use ashd_shell_core::{PanelContext, frame};
use ashd_tokens::metrics;
use ashd_ui::{
    Icon, Theme,
    anim::{EASE_OUT_CUBIC, motion::timing},
    hover_wash, icon,
    presence::{Presence, Timing},
};
use serde::Deserialize;

// ── geometry ──────────────────────────────────────────────────────────────────

/// The main area (where windows are) of an output of `size`, output-local: between the
/// rail, the bottom row and (while shown) the scratchpad; floating themes keep `gap` from
/// each and from the output edges (desk-05: `64,0 1516×1034`; desk-06: `112,10 1448×998`).
pub fn main_area(output: gpui::Size<Pixels>, t: &Theme, scratch_visible: bool) -> Bounds<Pixels> {
    let inset = frame::inset(t);
    let left = inset * 2. + t.rail_w;
    let top = inset;
    let right = if scratch_visible { px(metrics::SCRATCH_W) + inset * 2. } else { inset };
    let bottom = t.bar_h + inset * 2.;
    Bounds {
        origin: point(left, top),
        size: size((output.width - left - right).max(px(0.)), (output.height - top - bottom).max(px(0.))),
    }
}

/// What a per-display passive panel needs from the shell each render.
pub struct DisplayCtx {
    pub output: Option<String>,
    pub area: Bounds<Pixels>,
}

/// Output name and main area of the panel's display.
pub fn display_ctx(ctx: &PanelContext, t: &Theme, cx: &App) -> Option<DisplayCtx> {
    let info = ctx.shell.display(ctx.display.as_ref()?.id, cx)?;
    let scratch = ctx.shell.entity().is_some_and(|s| s.read(cx).scratch_visible);
    Some(DisplayCtx { area: main_area(info.size(), t, scratch), output: info.output })
}

/// The active workspace of `output` (the focused one when the output is unknown).
pub fn active_workspace<'a>(niri: &'a NiriState, output: Option<&str>) -> Option<&'a Workspace> {
    match output {
        Some(o) => niri.active_workspace(o),
        None => niri.focused_workspace(),
    }
}

/// Whether passive chrome should stay off `output` right now: overview open or a
/// fullscreen window (the overlay would cover it and block direct scanout).
pub fn suppressed(niri: &NiriState, output: Option<&str>) -> bool {
    niri.overview_open || !niri.connected || output.is_some_and(|o| frame::is_fullscreen(niri, o))
}

/// "02 code" — number and name as in the mockups. The number counts the *visible*
/// workspaces of the output (niri's `idx` also counts the hidden scratch workspace).
pub fn workspace_label(niri: &NiriState, ws: &Workspace) -> String {
    // a fullscreen workspace is labelled as its desk
    let ws = niri.desk(ws.id).unwrap_or(ws);
    let n = niri.visible_number(ws.id).unwrap_or((ws.idx as usize).max(1));
    match ws.name.as_deref().filter(|n| !n.is_empty()) {
        Some(name) => format!("{n:02} {name}"),
        None => format!("{n:02}"),
    }
}

/// Short name of a window for "focus: …": its title, else its app id.
pub fn window_label(w: &NiriWindow) -> String {
    let s = w.title.as_deref().filter(|t| !t.trim().is_empty()).or(w.app_id.as_deref()).unwrap_or("window");
    // Terminal titles are often the whole prompt: keep the chip short.
    const MAX: usize = 28;
    let mut out: String = s.chars().take(MAX).collect();
    if s.chars().count() > MAX {
        out.push('…');
    }
    out
}

/// The window "focus: …" names on workspace `ws`: the focused window if it is there,
/// else the workspace's active window.
pub fn focus_window<'a>(niri: &'a NiriState, ws: &Workspace) -> Option<&'a NiriWindow> {
    niri.focused_window()
        .filter(|w| w.workspace_id == Some(ws.id))
        .or_else(|| ws.active_window_id.and_then(|id| niri.windows.get(&id)))
}

// ── niri actions on a workspace ───────────────────────────────────────────────

/// Run `actions` in order (each awaited) on workspace `ws`, focusing it first when it is
/// not the focused one: niri's canvas/column actions act on the focused workspace.
pub fn run_on_workspace(handle: &NiriHandle, niri: &NiriState, ws: u64, actions: Vec<Action>, cx: &mut App) {
    let focus = !niri.workspaces.get(&ws).is_some_and(|w| w.is_focused);
    run_actions(handle, focus.then_some(ws), actions, cx);
}

/// [`run_on_workspace`] with the decision made: focus `focus_first` (a workspace id)
/// before running `actions`.
pub fn run_actions(handle: &NiriHandle, focus_first: Option<u64>, actions: Vec<Action>, cx: &mut App) {
    let handle = handle.clone();
    cx.spawn(async move |_| {
        if let Some(ws) = focus_first
            && let Err(e) = handle.action(Action::FocusWorkspace { reference: WorkspaceReferenceArg::Id(ws) }).await
        {
            tracing::warn!("niri focus workspace {ws}: {e}");
            return;
        }
        for a in actions {
            let name = format!("{a:?}");
            if let Err(e) = handle.action(a).await {
                tracing::warn!("niri {name}: {e}");
                return;
            }
        }
    })
    .detach();
}

// ── live views (one subscription for the whole shell) ─────────────────────────

struct LiveViewsGlobal(Entity<Arc<LiveViews>>);

impl Global for LiveViewsGlobal {}

/// The shared live canvas/strip view entity. Created (and subscribed to ashd-niri) by
/// the first caller; notified at most once per `ashd_niri::VIEWS_MIN_INTERVAL` while a
/// view moves. Observe it and compare the part you draw before `cx.notify()`.
pub fn live_views(handle: &NiriHandle, cx: &mut App) -> Entity<Arc<LiveViews>> {
    if let Some(g) = cx.try_global::<LiveViewsGlobal>() {
        return g.0.clone();
    }
    let entity = cx.new(|_| Arc::<LiveViews>::default());
    ashd_shell_core::bridge(handle.subscribe_views(), entity.clone(), cx);
    cx.set_global(LiveViewsGlobal(entity.clone()));
    entity
}

// ── key hints from keybinds.json ──────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug, PartialEq)]
struct BindEntry {
    keys: String,
    #[serde(default)]
    action: String,
}

/// The project's real binds (`/etc/ashd/keybinds.json`, `$ASHD_KEYBINDS`), loaded once in
/// the background. Empty when the file is missing: then no hint is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyBinds {
    binds: Vec<BindEntry>,
}

/// `$ASHD_KEYBINDS`, else `<etc>/keybinds.json` (same lookup as the cheatsheet).
pub fn keybinds_path() -> PathBuf {
    match std::env::var_os("ASHD_KEYBINDS").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => ashd_tokens::state::etc_dir().join("keybinds.json"),
    }
}

/// The niri action a bind runs in `canvas` or strip mode: the matching child of
/// `if-canvas { a; b; }`, else the action itself (`None` = nothing in this mode).
fn action_for_mode(action: &str, canvas: bool) -> Option<&str> {
    let a = action.trim();
    let Some(rest) = a.strip_prefix("if-canvas") else { return Some(a) };
    let body = rest.trim().strip_prefix('{')?.trim_end().strip_suffix('}')?;
    let mut children = body.split(';').map(str::trim).filter(|c| !c.is_empty());
    let first = children.next();
    if canvas { first } else { children.next() }
}

impl KeyBinds {
    pub fn parse(json: &str) -> Self {
        Self { binds: serde_json::from_str(json).unwrap_or_default() }
    }

    /// Keys of the first bind that runs `action` (exact, or with arguments) in the given
    /// mode, e.g. `keys_for("focus-column-left", false)` → `Super+Left`.
    pub fn keys_for(&self, action: &str, canvas: bool) -> Option<&str> {
        let matches = |exact: bool| {
            self.binds.iter().find(|b| {
                action_for_mode(&b.action, canvas).is_some_and(|a| {
                    if exact { a == action } else { a.split_whitespace().next() == Some(action) }
                })
            })
        };
        matches(true).or_else(|| matches(false)).map(|b| b.keys.as_str())
    }

    /// "Super+Left / Super+Right" when both exist, the one that exists, else `None`.
    pub fn pair(&self, a: &str, b: &str, canvas: bool) -> Option<String> {
        match (self.keys_for(a, canvas), self.keys_for(b, canvas)) {
            (Some(x), Some(y)) => Some(format!("{x} / {y}")),
            (Some(x), None) | (None, Some(x)) => Some(x.to_string()),
            (None, None) => None,
        }
    }
}

struct KeyBindsGlobal(Entity<KeyBinds>);

impl Global for KeyBindsGlobal {}

/// The shared [`KeyBinds`] entity (read in the background on first use).
pub fn keybinds(cx: &mut App) -> Entity<KeyBinds> {
    if let Some(g) = cx.try_global::<KeyBindsGlobal>() {
        return g.0.clone();
    }
    let entity = cx.new(|_| KeyBinds::default());
    cx.set_global(KeyBindsGlobal(entity.clone()));
    let path = keybinds_path();
    let task = cx.background_spawn(async move { std::fs::read_to_string(&path).ok() });
    let weak = entity.downgrade();
    cx.spawn(async move |cx| {
        if let Some(text) = task.await {
            let binds = KeyBinds::parse(&text);
            let _ = weak.update(cx, |b, cx| {
                *b = binds;
                cx.notify();
            });
        }
    })
    .detach();
    entity
}

// ── styling ───────────────────────────────────────────────────────────────────

/// Label role text (mono 10.5, caps per theme) in `color`.
pub fn label(text: impl AsRef<str>, color: Hsla, t: &Theme) -> Div {
    label_sized(text, metrics::type_scale::LABEL.0, color, t)
}

/// [`label`] at `size_px`.
pub fn label_sized(text: impl AsRef<str>, size_px: f32, color: Hsla, t: &Theme) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .font_family(t.font_mono.clone())
        .text_size(px(size_px))
        .line_height(px(metrics::type_scale::LABEL.1))
        .text_color(color)
        .child(t.label_text(text.as_ref()))
}

/// A square bordered ghost button (desk-05: header prev/next 24, minimap zoom 28) with a
/// 13px icon. `hovered` comes from [`Hover`].
pub fn square_button(id: impl Into<ElementId>, ic: Icon, side: f32, _hovered: bool, t: &Theme) -> Stateful<Div> {
    // (the hover fill is eased by the wash itself; `_hovered` is kept for the callers)
    div()
        .id(id)
        .flex_none()
        .size(px(side))
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(t.line)
        .rounded(frame::nested_radius(t))
        .cursor_pointer()
        .child(hover_wash(t.surface_2, Some(t.line), frame::nested_radius(t)))
        .child(icon(ic, 13.).text_color(t.fg))
}

// ── the Scroll <-> Canvas swap ────────────────────────────────────────────────

/// The chips of a mode come in and go out when the workspace switches between Scroll and
/// Canvas: 280 ms out-cubic (TOKENS.md §9 "Scroll <-> Canvas"), cross-fading with the other
/// mode's chips. (GPUI cannot scale a div, so the spec's .96 / 1.04 scale is a 4 px travel.)
///
/// `ModeFade` keeps the last content while it leaves; the panel is live (its passive surface
/// exists) until the exit is over.
pub struct ModeFade<T> {
    shown: Option<T>,
    presence: Presence,
    /// The exit finished since the last [`Self::take_gone`].
    gone: bool,
}

/// What to draw: the content, its opacity, and the movement progress (0..=1).
pub struct Shown<T> {
    pub content: T,
    pub fade: f32,
    pub travel: f32,
    /// Leaving: not interactive.
    pub leaving: bool,
}

/// Travel (px) of the swap.
pub const SWAP_TRAVEL: f32 = 4.0;

fn mode_timing() -> Timing {
    Timing::new(timing::MODE_SWITCH, EASE_OUT_CUBIC)
}

impl<T: Clone> ModeFade<T> {
    /// What is there at start does not animate in.
    pub fn new(initial: Option<T>) -> Self {
        let now = std::time::Instant::now();
        let presence = if initial.is_some() {
            Presence::present(mode_timing(), mode_timing(), now)
        } else {
            Presence::absent(mode_timing(), mode_timing(), now)
        };
        Self { shown: initial, presence, gone: false }
    }

    /// The current content (`None`: the mode is not active).
    pub fn sync(&mut self, current: Option<&T>) {
        let now = std::time::Instant::now();
        match current {
            Some(c) => {
                if self.shown.is_none() || !self.presence.is_present() {
                    self.presence.set_present(true, now);
                }
                self.shown = Some(c.clone());
            }
            None => {
                if self.shown.is_some() && self.presence.is_present() {
                    self.presence.set_present(false, now);
                }
            }
        }
    }

    /// The panel's surface must exist: showing, or still leaving.
    pub fn is_live(&self) -> bool {
        self.shown.is_some()
    }

    /// What to draw now; `None` once nothing is left (then [`Self::take_gone`] says so once).
    pub fn view(&mut self) -> Option<Shown<T>> {
        let now = std::time::Instant::now();
        if self.shown.is_some() && self.presence.is_gone(now) {
            self.shown = None;
            self.gone = true;
        }
        let (fade, travel, leaving) = (self.presence.value(now), self.presence.movement(now), !self.presence.is_present());
        self.shown.clone().map(|content| Shown { content, fade, travel, leaving })
    }

    /// The exit just finished: the caller tells the overlay host to drop the surface.
    pub fn take_gone(&mut self) -> bool {
        std::mem::take(&mut self.gone)
    }

    /// Frames are needed (entering or leaving).
    pub fn animating(&self) -> bool {
        self.presence.is_animating(std::time::Instant::now())
    }
}

/// The floating chip/card look: `surface` fill, 1px `frame` border.
pub fn card(t: &Theme, radius: Pixels) -> Div {
    div().bg(t.surface).border_1().border_color(t.frame).rounded(radius)
}

// ── hover without stickiness ──────────────────────────────────────────────────

/// Which control of a passive panel is hovered. GPUI's `.hover()` style sticks in a
/// passive overlay once the pointer leaves the input region (no leave reaches the
/// element), so panels track hover themselves: `on_hover` transitions plus the window's
/// `MouseExitEvent` ([`hover_reset`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hover(pub Option<&'static str>);

impl Hover {
    pub fn is(&self, key: &'static str) -> bool {
        self.0 == Some(key)
    }

    /// Apply an `on_hover` transition of control `key`. `true` if it changed.
    pub fn set(&mut self, key: &'static str, inside: bool) -> bool {
        let next = if inside {
            Some(key)
        } else if self.0 == Some(key) {
            None
        } else {
            self.0
        };
        let changed = next != self.0;
        self.0 = next;
        changed
    }
}

/// Attach hover tracking of control `key` (stored where `get` points) to `el`.
pub fn track_hover<T: 'static>(
    el: Stateful<Div>,
    key: &'static str,
    get: fn(&mut T) -> &mut Hover,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    el.on_hover(cx.listener(move |this, inside: &bool, _, cx| {
        if get(this).set(key, *inside) {
            cx.notify();
        }
    }))
}

/// An invisible element that clears the hover state when the pointer leaves the surface.
pub fn hover_reset<T: 'static>(get: fn(&mut T) -> &mut Hover, cx: &mut Context<T>) -> impl IntoElement {
    let this = cx.entity().downgrade();
    canvas(
        |_, _, _| {},
        move |_, _, window: &mut Window, _| {
            let this = this.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, _, _, cx| {
                let _ = this.update(cx, |t, cx| {
                    if get(t).0.take().is_some() {
                        cx.notify();
                    }
                });
            });
        },
    )
    .absolute()
    .size_0()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_area_matches_the_mockups() {
        use ashd_tokens::ThemeName;
        let out = size(px(1920.), px(1080.));
        let ash = Theme::get(ThemeName::Ash);
        assert_eq!(main_area(out, &ash, true), Bounds { origin: point(px(64.), px(0.)), size: size(px(1516.), px(1034.)) });
        let ink = Theme::get(ThemeName::Ink);
        assert_eq!(main_area(out, &ink, true), Bounds { origin: point(px(112.), px(10.)), size: size(px(1448.), px(998.)) });
        let a = main_area(out, &ash, false);
        assert_eq!(a.size.width, px(1856.));
    }

    #[test]
    fn key_hints_come_from_the_real_binds() {
        let json = r#"[
          {"keys":"Super+Left","niri":"Mod+Left","title":"Focus column left","group":"Columns","action":"focus-column-left"},
          {"keys":"Super+Right","niri":"Mod+Right","title":"Focus column right","group":"Columns","action":"focus-column-right"},
          {"keys":"Super+Shift+WheelScrollDown","niri":"x","title":"pan","group":"Columns","action":"if-canvas { canvas-pan-right; focus-column-right; }"},
          {"keys":"Super+Equal","niri":"x","title":"Zoom in","group":"Canvas","action":"if-canvas { canvas-zoom-in; set-column-width \"+10%\"; }"},
          {"keys":"Super+WheelScrollUp","niri":"x","title":"Zoom at cursor","group":"Canvas","action":"if-canvas { canvas-zoom-in at-cursor=true; focus-workspace-up; }"},
          {"keys":"Super+0","niri":"x","title":"Fit","group":"Canvas","action":"if-canvas { canvas-fit; center-column; }"},
          {"keys":"Super+Ctrl+0","niri":"x","title":"100%","group":"Canvas","action":"canvas-set-zoom 1.0"},
          {"keys":"Super+P","niri":"x","title":"planned","group":"Canvas","action":"planned"}
        ]"#;
        let b = KeyBinds::parse(json);
        assert_eq!(b.keys_for("focus-column-left", false), Some("Super+Left"));
        assert_eq!(b.keys_for("focus-column-right", false), Some("Super+Right"), "the plain bind is listed first");
        assert_eq!(b.keys_for("canvas-zoom-in", true), Some("Super+Equal"), "exact match wins over at-cursor");
        assert_eq!(b.keys_for("canvas-zoom-in", false), None, "the strip branch is set-column-width");
        assert_eq!(b.keys_for("center-column", false), Some("Super+0"));
        assert_eq!(b.keys_for("canvas-set-zoom", true), Some("Super+Ctrl+0"));
        assert_eq!(b.keys_for("canvas-pan-right", true), Some("Super+Shift+WheelScrollDown"));
        assert_eq!(b.pair("focus-column-left", "focus-column-right", false).as_deref(), Some("Super+Left / Super+Right"));
        assert_eq!(b.pair("canvas-zoom-out", "canvas-zoom-in", true).as_deref(), Some("Super+Equal"));
        assert_eq!(b.pair("nope", "nada", true), None);
        assert_eq!(KeyBinds::parse("not json"), KeyBinds::default());
    }

    #[test]
    fn if_canvas_children() {
        assert_eq!(action_for_mode("if-canvas { a b; c; }", true), Some("a b"));
        assert_eq!(action_for_mode("if-canvas { a b; c; }", false), Some("c"));
        assert_eq!(action_for_mode("if-canvas { a; }", false), None);
        assert_eq!(action_for_mode("focus-column-left", true), Some("focus-column-left"));
    }

    #[test]
    fn hover_transitions() {
        let mut h = Hover::default();
        assert!(h.set("a", true) && h.is("a"));
        assert!(h.set("b", true) && h.is("b"));
        assert!(!h.set("a", false), "a late leave of another control changes nothing");
        assert!(h.set("b", false) && h.0.is_none());
    }
}
