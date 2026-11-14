//! Stroke icons (24 grid, 1.6 stroke, round caps) from `ashd/assets/icons/*.svg`
//! (fm's set + the glyphs of docs/design/pages/sys-07-icons.html).
//! Tinted with the element's text colour: `icon(Icon::Wifi, 16.).text_color(t.fg)`.
//!
//! Adding an icon (additive edit, list it in your report): drop `name.svg` into
//! `ashd/assets/icons/` (stroke `#000`, it is recoloured) and add a line below.

use std::path::Path;

use gpui::{AnyElement, App, Animation, AnimationExt as _, ObjectFit, Svg, Transformation, percentage, prelude::*, px, svg, img};

use crate::theme::theme;

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum Icon { $($variant),* }
        impl Icon {
            pub const ALL: &'static [Icon] = &[$(Icon::$variant),*];
            /// Asset path (`icons/<file>.svg`).
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file, ".svg")),* }
            }
        }
    };
}

icons! {
    // fm set
    File => "file", Folder => "folder", Sliders => "sliders", Image => "image", Code => "code",
    Pin => "pin", Grid => "grid", Clipboard => "clipboard", Trash => "trash",
    ChevronRight => "chevron_right", ChevronLeft => "chevron_left", ChevronDown => "chevron_down",
    ChevronUp => "chevron_up", Search => "search", Minus => "minus", Plus => "plus", List => "list",
    Pause => "pause", Play => "play", Music => "music", Eject => "eject", Eye => "eye",
    Fullscreen => "fullscreen", Close => "close", Globe => "globe", Terminal => "terminal",
    Home => "home", Disk => "disk", Archive => "archive", Link => "link", Download => "download",
    Tag => "tag", Copy => "copy", Move => "move", Pencil => "pencil", Info => "info",
    Check => "check", Video => "video", Refresh => "refresh", GitBranch => "git_branch",
    Lock => "lock",
    // design set (sys-07)
    Activity => "act", Battery => "bat", Calc => "calc", Canvas => "canvas", Chat => "chat",
    Clip => "clip", Columns => "cols", Fit => "fit", Left => "left", Note => "note",
    Right => "right", SkipBack => "skipb", SkipForward => "skipf", Swatch => "swatch",
    Term => "term", Text => "text", Volume => "vol", Wifi => "wifi",
    // kit set: session
    Unlock => "unlock", Power => "power", Reboot => "reboot", Logout => "logout", Moon => "moon",
    // connectivity
    Bluetooth => "bluetooth", BluetoothOff => "bluetooth_off", Ethernet => "ethernet",
    EthernetOff => "ethernet_off", Shield => "shield", ShieldCheck => "shield_check",
    GlobeOff => "globe_off", Wifi0 => "wifi_0", Wifi1 => "wifi_1", WifiOff => "wifi_off",
    Airplane => "airplane",
    // notifications / audio
    Bell => "bell", BellOff => "bell_off", Mic => "mic", MicOff => "mic_off", Volume0 => "vol_0",
    Volume1 => "vol_1", VolumeMute => "vol_mute", Headphones => "headphones",
    // hardware / system
    Sun => "sun", Keyboard => "keyboard", User => "user", Cpu => "cpu", Memory => "memory",
    Monitor => "monitor", Camera => "camera", Record => "record", Crop => "crop",
    Calendar => "calendar", Clock => "clock", Settings => "settings", App => "app",
    Window => "window",
    // battery
    Battery0 => "battery_0", Battery25 => "battery_25", Battery50 => "battery_50",
    Battery75 => "battery_75", Battery100 => "battery_100", BatteryCharging => "battery_charging",
    // generic
    EyeOff => "eye_off", Upload => "upload", AlertTriangle => "alert_triangle",
    AlertCircle => "alert_circle", CheckCircle => "check_circle", Help => "help", Dots => "dots",
    ArrowUp => "arrow_up", ArrowDown => "arrow_down", ArrowLeft => "arrow_left",
    ArrowRight => "arrow_right", External => "external", Spinner => "spinner", Star => "star", CheckBold => "check_bold",
    // quick settings (theme row)
    Contrast => "contrast",
}

impl Icon {
    /// `x` — the close glyph (same as [`Icon::Close`]).
    pub const X: Icon = Icon::Close;
    /// Speaker with two waves (full volume; same as [`Icon::Volume`]).
    pub const VOLUME_2: Icon = Icon::Volume;
    /// Wifi with all arcs (same as [`Icon::Wifi`]).
    pub const WIFI_FULL: Icon = Icon::Wifi;

    /// Battery glyph for a charge `fraction` (0..=1), or the charging bolt.
    pub fn battery(fraction: f32, charging: bool) -> Icon {
        if charging {
            return Icon::BatteryCharging;
        }
        match fraction {
            f if f < 0.125 => Icon::Battery0,
            f if f < 0.375 => Icon::Battery25,
            f if f < 0.625 => Icon::Battery50,
            f if f < 0.875 => Icon::Battery75,
            _ => Icon::Battery100,
        }
    }

    /// Speaker glyph for a volume `fraction` (0..=1, above 1 counts as full) or muted.
    pub fn volume(fraction: f32, muted: bool) -> Icon {
        match fraction {
            _ if muted => Icon::VolumeMute,
            f if f <= 0.0 => Icon::Volume0,
            f if f < 0.5 => Icon::Volume1,
            _ => Icon::Volume,
        }
    }

    /// Wifi glyph for a signal `strength` (0..=1); `None` = radio off / disconnected.
    pub fn wifi(strength: Option<f32>) -> Icon {
        match strength {
            None => Icon::WifiOff,
            Some(s) if s < 0.25 => Icon::Wifi0,
            Some(s) if s < 0.6 => Icon::Wifi1,
            Some(_) => Icon::Wifi,
        }
    }
}

/// An icon element of `size` px. Colour it with `.text_color(..)`; it does not inherit.
pub fn icon(icon: Icon, size: f32) -> Svg {
    svg().path(icon.path()).size(px(size)).flex_none()
}

/// A spinning [`Icon::Spinner`] (one turn per second, linear). Under reduced motion it
/// stays still. `id` must be unique among siblings.
pub fn spinner(id: impl Into<gpui::ElementId>, size: f32, color: gpui::Hsla) -> impl IntoElement {
    icon(Icon::Spinner, size).text_color(color).with_animation(
        id,
        Animation::new(std::time::Duration::from_millis(900)).repeat(),
        |svg, t| svg.with_transformation(Transformation::rotate(percentage(t))),
    )
}

/// An application icon from a file (PNG, SVG, … — e.g. a path out of the freedesktop icon
/// theme) at `size` px, aspect preserved. `None`, an unreadable file or a decode error
/// falls back to the generic [`Icon::App`] glyph in `fg-muted`.
pub fn app_icon(path: Option<&Path>, size: f32, cx: &App) -> AnyElement {
    let color = theme(cx).fg_muted;
    let fallback = move || icon(Icon::App, size).text_color(color).into_any_element();
    match path {
        Some(path) => img(path.to_path_buf())
            .size(px(size))
            .flex_none()
            .object_fit(ObjectFit::Contain)
            .with_fallback(fallback)
            .into_any_element(),
        None => fallback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_an_asset() {
        for ic in Icon::ALL {
            assert!(crate::Assets::get(ic.path()).is_some(), "missing asset for {ic:?}: {}", ic.path());
        }
    }

    #[test]
    fn no_orphan_or_malformed_assets() {
        let used: std::collections::HashSet<_> = Icon::ALL.iter().map(|i| i.path()).collect();
        let mut seen = 0;
        for path in crate::Assets::iter().filter(|p| p.starts_with("icons/")) {
            assert!(used.contains(path.as_ref()), "{path} has no Icon variant");
            let data = crate::Assets::get(&path).unwrap().data;
            let src = std::str::from_utf8(&data).unwrap();
            assert!(src.starts_with("<svg ") && src.trim_end().ends_with("</svg>"), "{path}");
            for needle in ["viewBox=\"0 0 24 24\"", "stroke=\"#000\"", "stroke-linecap=\"round\"", "stroke-linejoin=\"round\""] {
                assert!(src.contains(needle), "{path} lacks {needle}");
            }
            let stroke = if path.ends_with("check_bold.svg") { "stroke-width=\"3.5\"" } else { "stroke-width=\"1.6\"" };
            assert!(src.contains(stroke), "{path} lacks {stroke}");
            assert!(!src.contains("<span") && !src.contains("<div"), "{path} contains HTML");
            assert_eq!(src.matches("<svg").count(), 1, "{path}");
            seen += 1;
        }
        assert_eq!(seen, used.len());
        let unique: std::collections::HashSet<_> = Icon::ALL.iter().collect();
        assert_eq!(unique.len(), Icon::ALL.len());
    }

    #[test]
    fn level_helpers() {
        assert_eq!(Icon::battery(0.05, false), Icon::Battery0);
        assert_eq!(Icon::battery(0.5, false), Icon::Battery50);
        assert_eq!(Icon::battery(1.0, false), Icon::Battery100);
        assert_eq!(Icon::battery(0.3, true), Icon::BatteryCharging);
        assert_eq!(Icon::volume(0.0, false), Icon::Volume0);
        assert_eq!(Icon::volume(0.3, false), Icon::Volume1);
        assert_eq!(Icon::volume(1.2, false), Icon::Volume);
        assert_eq!(Icon::volume(0.9, true), Icon::VolumeMute);
        assert_eq!(Icon::wifi(None), Icon::WifiOff);
        assert_eq!(Icon::wifi(Some(0.1)), Icon::Wifi0);
        assert_eq!(Icon::wifi(Some(0.9)), Icon::Wifi);
    }
}
