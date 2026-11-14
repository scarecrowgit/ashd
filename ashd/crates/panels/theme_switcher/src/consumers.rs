//! The "apply to" side of the theme switcher. The consumers, their generated files, the
//! persisted checkbox state and the reload rules live in `ashd-theme` (shared with
//! `ashd theme set` and the quick-settings theme row); see the table there.

pub use ashd_theme::{ApplyTo, Consumer, apply_to_path};
