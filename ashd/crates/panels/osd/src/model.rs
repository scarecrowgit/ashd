//! What the OSD shows, as plain data (no GPUI): kinds and the volume bar maths.

/// One OSD variant.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Default sink: icon, bar, value (muted / boost variants).
    Volume,
    /// Default source: "Microphone muted" / "Microphone on".
    Mic,
    /// First display's brightness: sliders icon, bar, value (det-06 "brightness").
    Brightness,
}

/// The 150 % ceiling of the audio service (`ashd_svc_audio::MAX_VOLUME`).
const SCALE_MAX: f32 = ashd_svc_audio::MAX_VOLUME;

/// The volume bar of det-06.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    /// Fill, 0..=1 of the track.
    pub fill: f32,
    /// The 100 % tick (only above 100 %).
    pub tick: Option<f32>,
    pub muted: bool,
    /// Above 100 %: value in `warn`.
    pub boost: bool,
    /// "62", "128", "mute", or "--" when there is no sink.
    pub text: String,
}

/// Bar for `volume` (1.0 = 100 %). Up to 100 % the track is the 0..100 range; boosted, it is
/// 0..150 so the 100 % mark becomes a tick two thirds along.
pub fn volume_bar(volume: f32, muted: bool, has_device: bool) -> Bar {
    let volume = if volume.is_finite() { volume.clamp(0.0, SCALE_MAX) } else { 0.0 };
    let boost = !muted && volume > 1.0 + 1e-3;
    let pct = (volume * 100.0).round() as i32;
    Bar {
        fill: if boost || (volume > 1.0 && muted) { volume / SCALE_MAX } else { volume },
        tick: (boost || (volume > 1.0 && muted)).then_some(1.0 / SCALE_MAX),
        muted,
        boost,
        text: match (has_device, muted) {
            (false, _) => "--".into(),
            (_, true) => "mute".into(),
            _ => pct.to_string(),
        },
    }
}

/// The brightness bar: `level` 0..=1 fills the track, the value is the whole percent.
pub fn brightness_bar(level: f32, has_display: bool) -> Bar {
    let level = if level.is_finite() { level.clamp(0.0, 1.0) } else { 0.0 };
    Bar {
        fill: level,
        tick: None,
        muted: false,
        boost: false,
        text: if has_display { ((level * 100.0).round() as i32).to_string() } else { "--".into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_volume() {
        let b = volume_bar(0.62, false, true);
        assert_eq!((b.fill, b.tick, b.muted, b.boost, b.text.as_str()), (0.62, None, false, false, "62"));
    }

    #[test]
    fn muted_keeps_the_level_and_says_mute() {
        let b = volume_bar(0.62, true, true);
        assert_eq!((b.fill, b.muted, b.boost, b.text.as_str()), (0.62, true, false, "mute"));
    }

    #[test]
    fn boost_scales_to_150_with_a_tick_at_100() {
        let b = volume_bar(1.28, false, true);
        assert!(b.boost);
        assert_eq!(b.text, "128");
        assert!((b.fill - 1.28 / 1.5).abs() < 1e-6);
        assert!((b.tick.unwrap() - 2.0 / 3.0).abs() < 1e-6);
        let full = volume_bar(1.0, false, true);
        assert!(!full.boost && full.tick.is_none() && full.fill == 1.0);
        let top = volume_bar(9.0, false, true);
        assert_eq!((top.fill, top.text.as_str()), (1.0, "150"));
    }

    #[test]
    fn missing_device_and_garbage() {
        assert_eq!(volume_bar(0.0, false, false).text, "--");
        assert_eq!(volume_bar(f32::NAN, false, true).fill, 0.0);
        assert_eq!(volume_bar(-1.0, false, true).fill, 0.0);
    }

    #[test]
    fn brightness_bar_is_a_plain_percentage() {
        let b = brightness_bar(0.4, true);
        assert_eq!((b.fill, b.tick, b.muted, b.boost, b.text.as_str()), (0.4, None, false, false, "40"));
        assert_eq!(brightness_bar(1.7, true).text, "100");
        assert_eq!(brightness_bar(-1.0, true).fill, 0.0);
        assert_eq!(brightness_bar(f32::NAN, true).text, "0");
        assert_eq!(brightness_bar(0.5, false).text, "--");
    }
}
