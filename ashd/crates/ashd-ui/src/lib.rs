//! ashd GPUI kit: the [`Theme`] global, shared components, [`TextInput`], icons and
//! animation helpers. Every surface of the shell is drawn with these.
//!
//! ```ignore
//! gpui_platform::application().with_assets(ashd_ui::Assets).run(|cx| {
//!     ashd_ui::init(ashd_tokens::ThemeName::Ash, cx);
//!     // … open windows; in views:
//!     let t = ashd_ui::theme(cx).clone();
//!     div().bg(t.surface).child(ashd_ui::label("cpu", cx))
//! });
//! ```

pub mod anim;
pub mod fx;
pub mod presence;
mod assets;
pub mod controls;
pub mod icons;
pub mod overlays;
pub mod text_input;
pub mod theme;
pub mod ui;

pub use assets::Assets;
pub use controls::*;
pub use fx::{Flip, Follow, HoverWash, Look, Reveal, Swap, fill_bar, flip, follow, hover_ring, hover_wash, mix, reveal, select_plate, state_layer, swap};
pub use icons::{Icon, app_icon, icon, spinner};
pub use overlays::*;
pub use text_input::{TextInput, TextInputEvent};
pub use theme::{Theme, color, current_theme_name, metrics, set_theme, theme};
pub use ui::*;

/// Theme global + TextInput key bindings. Call once at startup, before opening windows.
pub fn init(theme_name: ashd_tokens::ThemeName, cx: &mut gpui::App) {
    anim::init_from_env();
    theme::init(theme_name, cx);
    text_input::bind_keys(cx);
}
