//! Bottom row (desk-05 `top 1034, 1920 × 46`), two panels of this crate (the dock in the
//! middle is `ashd-panel-dock`):
//!
//! - [`BarPanel`] (`Slot::BarStart`, `STATUS_W` 560 at most, hugging its content): clock + date,
//!   cpu/mem meters, the network + volume control, battery (if any), keyboard layout chip, bell. Controls
//!   open popups owned by other panels through `ShellHandle::request` with their own
//!   bounds as anchor (`DisplayInfo::bar_to_output`), and show the "open" look
//!   (surface-2 + 2px accent underline) while their popup is the open modal.
//! - [`BarEndPanel`] (`Slot::BarEnd`): workspace label ("02 code scroll"), Scroll | Canvas,
//!   session button.
//!
//! Owner: shell frame.

mod end;
mod fixture;
mod nowplaying;
pub mod geom;
mod status;
mod tray;
mod tray_menu;
mod tray_overflow;

use std::{cell::RefCell, collections::HashMap, rc::Rc};

pub use end::BarEndPanel;
use gpui::{
    App, Bounds, Div, Hsla, IntoElement, ParentElement as _, Pixels, SharedString, Styled as _, canvas, div, px,
};
use ashd_shell_core::{ModalAnchor, PanelContext, PanelId};
use ashd_ui::{Theme, theme};
pub use status::{BarPanel, layout_code};
pub use tray::{TrayTooltipPanel, plan as tray_plan};
pub use tray_menu::TrayMenuPanel;
pub use tray_overflow::TrayOverflowPanel;

/// Panel ids of the popups the bar opens (no crate dependency between panels).
pub mod ids {
    use ashd_shell_core::PanelId;
    pub const QUICK_SETTINGS: PanelId = PanelId("quick-settings");
    pub const NOTIFICATION_CENTER: PanelId = PanelId("notification-center");
    pub const CALENDAR: PanelId = PanelId("calendar");
    pub const SESSION: PanelId = PanelId("session");
    pub const MEDIA: PanelId = PanelId("media");
    pub const TRAY_MENU: PanelId = PanelId("tray-menu");
    pub const TRAY_OVERFLOW: PanelId = PanelId("tray-overflow");
}

/// Last painted bounds of the bar's controls (bar-surface px), for popup anchors. Also
/// publishes them as shell-core named anchors (`ashd_shell_core::anchors`), so popups
/// opened from a keybind / the CLI sit where a click would have put them.
#[derive(Clone, Default)]
pub(crate) struct BoundsStore(Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>, Option<gpui::DisplayId>);

/// Named anchors (`ashd_shell_core::anchors::names`) each tracked control publishes.
fn anchor_names(key: &'static str) -> &'static [&'static str] {
    use ashd_shell_core::anchors::names::*;
    match key {
        "clock" => &[CLOCK],
        "netvol" => &[NETWORK, VOLUME],
        "bell" => &[BELL],
        "session" => &[SESSION],
        "media" => &[MEDIA],
        _ => &[],
    }
}

impl BoundsStore {
    pub fn new(ctx: &PanelContext) -> Self {
        Self(Rc::default(), ctx.display.as_ref().map(|d| d.id))
    }

    /// An invisible absolute child that records its parent's bounds under `key`.
    pub fn track(&self, key: &'static str) -> impl IntoElement {
        let store = self.0.clone();
        let display = self.1;
        canvas(
            move |bounds, _, cx| {
                store.borrow_mut().insert(key, bounds);
                if let Some(d) = display {
                    for name in anchor_names(key) {
                        ashd_shell_core::anchors::publish(d, ashd_shell_core::SurfaceKind::Bar, name, bounds, cx);
                    }
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// Output-local anchor of control `key` on the panel's display.
    pub fn anchor(&self, key: &'static str, ctx: &PanelContext, cx: &App) -> Option<ModalAnchor> {
        let bounds = *self.0.borrow().get(key)?;
        let info = ctx.shell.display(ctx.display.as_ref()?.id, cx)?;
        Some(info.bar_to_output(bounds, theme(cx)))
    }

    /// Control `key` is not shown (this frame): forget its bounds and named anchors.
    pub fn forget(&self, key: &'static str, cx: &mut App) {
        self.0.borrow_mut().remove(key);
        if let Some(d) = self.1 {
            for name in anchor_names(key) {
                ashd_shell_core::anchors::forget(d, name, cx);
            }
        }
    }
}

/// Whether `panel` is the open modal on this panel's display.
pub(crate) fn is_open(panel: PanelId, ctx: &PanelContext, cx: &App) -> bool {
    let Some(m) = ctx.shell.modal(cx) else { return false };
    m.panel == panel && ctx.display.as_ref().is_none_or(|d| d.id == m.display)
}

/// The "my popup is open" look: surface-2 background + 2px accent underline, fading in and
/// out (120 ms) with the popup. A layer under the control's content: add it as the *first
/// child* of a control that has an element id.
pub(crate) fn open_layer(open: bool, t: &Theme) -> ashd_ui::Follow<Div> {
    let (s2, accent, radius) = (t.surface_2, t.accent, t.radius_control);
    ashd_ui::follow(
        "open-look",
        if open { 1.0 } else { 0.0 },
        ashd_ui::presence::Timing::new(ashd_ui::anim::motion::timing::SWAP, ashd_ui::anim::EASE_STANDARD),
        move |p| {
            let clear = gpui::transparent_black();
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .rounded(radius)
                .bg(ashd_ui::mix(clear, s2, p))
                .child(
                    div().absolute().left_0().right_0().bottom_0().h(px(geom::UNDERLINE_H)).bg(ashd_ui::mix(clear, accent, p)),
                )
        },
    )
    .fade()
}

/// Hover fill of a bar control (not while its own popup is open: the open layer has it).
pub(crate) fn hover_layer(t: &Theme) -> ashd_ui::HoverWash {
    ashd_ui::hover_wash(t.surface_2, None, t.radius_control)
}

/// Mono label text (labels, values) at `size`: caps, or as written in themes without
/// label caps (Midnight, Dusk).
pub(crate) fn caps(text: impl Into<SharedString>, size: f32, color: Hsla, t: &Theme) -> Div {
    let text: SharedString = text.into();
    div()
        .flex_none()
        .font_family(t.font_mono.clone())
        .text_size(px(size))
        .line_height(px((size * 1.3).round()))
        .text_color(color)
        .child(t.label_text(&text))
}

/// A numeric readout: mono text right-aligned in a slot as wide as its widest value
/// (`chars` characters), so digits appearing or disappearing move nothing.
pub(crate) fn slot(text: impl Into<SharedString>, chars: usize, size: f32, color: Hsla, t: &Theme) -> Div {
    caps(text, size, color, t).min_w(px(geom::slot_w(chars, size))).flex().justify_end()
}

/// 1×22 vertical separator.
pub(crate) fn sep(t: &Theme) -> Div {
    div().flex_none().w(px(1.)).h(px(geom::SEP_H)).bg(t.line)
}
