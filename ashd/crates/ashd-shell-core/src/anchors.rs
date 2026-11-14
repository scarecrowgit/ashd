//! Named popup anchors, per display.
//!
//! Frame panels publish where their popup-opening controls are while they paint (the bar:
//! `"clock"`, `"network"`, `"volume"`, `"media"`, `"bell"`, `"session"`); a modal panel
//! declares which names it grows from ([`crate::Panel::ANCHORS`]). `open_modal(id, None,
//! …)` — a keybind, `ashd quick-settings` — then resolves the first published name on the
//! target display, so a popup opened from the CLI sits where a click would have put it.
//! An explicit anchor (the clicked control's own bounds) always wins.
//!
//! Bounds are stored in the publishing surface's coordinates and converted to output-local
//! px when resolved (the theme's floating inset may have changed in between).

use std::collections::HashMap;

use gpui::{App, Bounds, DisplayId, Global, IntoElement, Pixels, Styled as _, canvas};

use crate::{DisplayInfo, ModalAnchor, SurfaceKind};

/// The standard names (the bar publishes these; panels refer to them in `ANCHORS`).
pub mod names {
    pub const CLOCK: &str = "clock";
    pub const NETWORK: &str = "network";
    pub const VOLUME: &str = "volume";
    pub const MEDIA: &str = "media";
    pub const BELL: &str = "bell";
    pub const SESSION: &str = "session";
}

#[derive(Default)]
struct NamedAnchors(HashMap<(DisplayId, &'static str), (SurfaceKind, Bounds<Pixels>)>);

impl Global for NamedAnchors {}

/// Record `bounds` (in `surface` coordinates) as anchor `name` on `display`.
pub fn publish(display: DisplayId, surface: SurfaceKind, name: &'static str, bounds: Bounds<Pixels>, cx: &mut App) {
    cx.default_global::<NamedAnchors>().0.insert((display, name), (surface, bounds));
}

/// Drop anchor `name` on `display` (its control is not shown any more).
pub fn forget(display: DisplayId, name: &'static str, cx: &mut App) {
    if let Some(a) = cx.try_global::<NamedAnchors>()
        && a.0.contains_key(&(display, name))
    {
        cx.global_mut::<NamedAnchors>().0.remove(&(display, name));
    }
}

/// Drop every anchor of a display that went away.
pub(crate) fn forget_display(display: DisplayId, cx: &mut App) {
    if cx.try_global::<NamedAnchors>().is_some() {
        cx.global_mut::<NamedAnchors>().0.retain(|(d, _), _| *d != display);
    }
}

/// An invisible, absolutely positioned child that publishes its parent's bounds as anchor
/// `name` every time it is laid out. Put it inside the control (which must be
/// `.relative()`), like `hit_area`.
pub fn track(display: DisplayId, surface: SurfaceKind, name: &'static str) -> impl IntoElement {
    canvas(move |bounds, _, cx| publish(display, surface, name, bounds, cx), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// Output-local anchor `name` on `info`'s display, if a frame panel published it.
pub fn resolve(info: &DisplayInfo, name: &str, cx: &App) -> Option<ModalAnchor> {
    let anchors = cx.try_global::<NamedAnchors>()?;
    let (surface, bounds) = anchors.0.iter().find(|((d, n), _)| *d == info.id && *n == name).map(|(_, v)| *v)?;
    let t = ashd_ui::theme(cx);
    Some(match surface {
        SurfaceKind::Bar => info.bar_to_output(bounds, t),
        SurfaceKind::Rail => info.rail_to_output(bounds, t),
        // Full-output surfaces are already output-local.
        _ => bounds,
    })
}

/// The first of `names` published on `info`'s display.
pub fn resolve_first(info: &DisplayInfo, names: &[&str], cx: &App) -> Option<ModalAnchor> {
    names.iter().find_map(|n| resolve(info, n, cx))
}
