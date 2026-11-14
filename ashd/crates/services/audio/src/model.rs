//! The pure core of the audio service: the device model, the command → action logic
//! and the volume arithmetic. No pulse, no threads, fully unit-tested; the pulse backend
//! (`pulse.rs`) only mirrors server state into it and executes the [`Action`]s it returns.

use crate::{AudioState, Command, Device, DeviceKind, MAX_VOLUME, Target};

/// `PA_VOLUME_NORM`: 100 % in pulse's integer scale.
pub(crate) const NORM: u32 = 0x10000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dir {
    Sink,
    Source,
}

/// A device plus what is needed to write a volume back (the raw per-channel values).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RawDevice {
    pub dev: Device,
    pub channels: Vec<u32>,
}

/// Something the backend must do to the server.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Volume { dir: Dir, name: String, channels: Vec<u32> },
    Mute { dir: Dir, name: String, muted: bool },
    MakeDefault { dir: Dir, name: String },
}

#[derive(Debug, Default)]
pub(crate) struct Model {
    pub available: bool,
    pub sinks: Vec<RawDevice>,
    pub sources: Vec<RawDevice>,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

/// Average of the channels as a fraction of 100 %.
pub(crate) fn volume_of(channels: &[u32]) -> f32 {
    if channels.is_empty() {
        return 0.0;
    }
    let sum: u64 = channels.iter().map(|&c| u64::from(c)).sum();
    (sum as f64 / channels.len() as f64 / f64::from(NORM)) as f32
}

/// `channels` rescaled so that their average becomes `target` (balance is preserved).
/// All-zero channels become uniform.
pub(crate) fn scale_channels(channels: &[u32], target: f32) -> Vec<u32> {
    let target = target.clamp(0.0, MAX_VOLUME);
    let want = (f64::from(target) * f64::from(NORM)).round();
    let avg = volume_of(channels) as f64 * f64::from(NORM);
    channels
        .iter()
        .map(|&c| {
            let v = if avg < 1.0 { want } else { f64::from(c) * want / avg };
            v.round().clamp(0.0, f64::from((MAX_VOLUME * NORM as f32) as u32)) as u32
        })
        .collect()
}

/// New volume after a relative step of `percent` points: clamped to 0..=100 %, unless the
/// device is already above 100 % — then up to [`MAX_VOLUME`].
pub(crate) fn step(current: f32, percent: i32) -> f32 {
    let top = if current > 1.0 + 1e-3 { MAX_VOLUME } else { 1.0 };
    (current + percent as f32 / 100.0).clamp(0.0, top)
}

pub(crate) fn kind_of(
    form_factor: Option<&str>,
    bus: Option<&str>,
    port: Option<&str>,
    name: &str,
    is_source: bool,
) -> DeviceKind {
    let port = port.unwrap_or("").to_ascii_lowercase();
    let ff = form_factor.unwrap_or("").to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    if is_source {
        return DeviceKind::Microphone;
    }
    if bus == Some("bluetooth") || name.starts_with("bluez") {
        return DeviceKind::Bluetooth;
    }
    if port.contains("hdmi") || port.contains("displayport") || name.contains("hdmi") || ff == "hdmi" {
        return DeviceKind::Hdmi;
    }
    if port.contains("headset") || ff == "headset" {
        return DeviceKind::Headset;
    }
    if port.contains("headphone") || ff == "headphone" {
        return DeviceKind::Headphones;
    }
    if port.contains("speaker") || ff == "speaker" || ff == "internal" || ff == "computer" {
        return DeviceKind::Speakers;
    }
    DeviceKind::Other
}

impl Model {
    pub fn state(&self) -> AudioState {
        let list = |v: &[RawDevice], default: &Option<String>| {
            v.iter()
                .map(|r| Device { is_default: default.as_deref() == Some(r.dev.name.as_str()), ..r.dev.clone() })
                .collect()
        };
        AudioState {
            available: self.available,
            sinks: list(&self.sinks, &self.default_sink),
            sources: list(&self.sources, &self.default_source),
        }
    }

    fn list(&self, dir: Dir) -> &Vec<RawDevice> {
        match dir {
            Dir::Sink => &self.sinks,
            Dir::Source => &self.sources,
        }
    }

    fn list_mut(&mut self, dir: Dir) -> &mut Vec<RawDevice> {
        match dir {
            Dir::Sink => &mut self.sinks,
            Dir::Source => &mut self.sources,
        }
    }

    fn resolve(&self, target: &Target) -> Option<(Dir, String)> {
        match target {
            Target::DefaultSink => self.default_sink.clone().map(|n| (Dir::Sink, n)),
            Target::DefaultSource => self.default_source.clone().map(|n| (Dir::Source, n)),
            Target::Sink(n) => Some((Dir::Sink, n.clone())),
            Target::Source(n) => Some((Dir::Source, n.clone())),
        }
    }

    fn find(&mut self, dir: Dir, name: &str) -> Option<&mut RawDevice> {
        self.list_mut(dir).iter_mut().find(|r| r.dev.name == name)
    }

    /// Apply a command: update the model optimistically (so repeated steps compose before
    /// the server's change events arrive) and return what the backend must execute.
    pub fn apply(&mut self, cmd: Command) -> Vec<Action> {
        if !self.available {
            return Vec::new();
        }
        match cmd {
            Command::SetVolume(target, volume) => self.set_volume(&target, volume),
            Command::StepVolume(target, percent) => {
                let Some((dir, name)) = self.resolve(&target) else { return Vec::new() };
                let Some(cur) = self.list(dir).iter().find(|r| r.dev.name == name).map(|r| r.dev.volume) else {
                    return Vec::new();
                };
                self.set_volume(&target, step(cur, percent))
            }
            Command::SetMute(target, muted) => self.set_mute(&target, Some(muted)),
            Command::ToggleMute(target) => self.set_mute(&target, None),
            Command::SetDefault(target) => {
                let Some((dir, name)) = self.resolve(&target) else { return Vec::new() };
                if self.find(dir, &name).is_none() {
                    return Vec::new();
                }
                match dir {
                    Dir::Sink => self.default_sink = Some(name.clone()),
                    Dir::Source => self.default_source = Some(name.clone()),
                }
                vec![Action::MakeDefault { dir, name }]
            }
        }
    }

    fn set_volume(&mut self, target: &Target, volume: f32) -> Vec<Action> {
        let Some((dir, name)) = self.resolve(target) else { return Vec::new() };
        let Some(dev) = self.find(dir, &name) else { return Vec::new() };
        if dev.channels.is_empty() {
            return Vec::new();
        }
        let channels = scale_channels(&dev.channels, volume);
        dev.dev.volume = volume_of(&channels);
        dev.channels = channels.clone();
        vec![Action::Volume { dir, name, channels }]
    }

    fn set_mute(&mut self, target: &Target, muted: Option<bool>) -> Vec<Action> {
        let Some((dir, name)) = self.resolve(target) else { return Vec::new() };
        let Some(dev) = self.find(dir, &name) else { return Vec::new() };
        let muted = muted.unwrap_or(!dev.dev.muted);
        dev.dev.muted = muted;
        vec![Action::Mute { dir, name, muted }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, vol: f32, muted: bool) -> RawDevice {
        let c = (vol * NORM as f32).round() as u32;
        RawDevice {
            dev: Device { index: 1, name: name.into(), description: name.into(), volume: vol, muted, ..Default::default() },
            channels: vec![c, c],
        }
    }

    fn model() -> Model {
        Model {
            available: true,
            sinks: vec![dev("a", 0.5, false), dev("b", 1.2, true)],
            sources: vec![dev("mic", 0.8, false)],
            default_sink: Some("a".into()),
            default_source: Some("mic".into()),
        }
    }

    #[test]
    fn volume_roundtrip_and_balance() {
        assert!((volume_of(&[NORM, NORM / 2]) - 0.75).abs() < 1e-6);
        let scaled = scale_channels(&[NORM, NORM / 2], 0.3);
        assert!((volume_of(&scaled) - 0.3).abs() < 1e-3);
        assert!(scaled[0] > scaled[1] && (scaled[0] as f32 / scaled[1] as f32 - 2.0).abs() < 0.01);
        assert_eq!(scale_channels(&[0, 0], 0.5), vec![NORM / 2, NORM / 2]);
        assert_eq!(scale_channels(&[NORM], 9.0), vec![(MAX_VOLUME * NORM as f32) as u32]);
        assert_eq!(scale_channels(&[NORM], -1.0), vec![0]);
    }

    #[test]
    fn step_clamps_to_100_unless_already_above() {
        assert!((step(0.98, 5) - 1.0).abs() < 1e-6);
        assert!((step(0.02, -5) - 0.0).abs() < 1e-6);
        assert!((step(1.2, 5) - 1.25).abs() < 1e-6);
        assert!((step(1.48, 5) - MAX_VOLUME).abs() < 1e-6);
        assert!((step(1.2, -5) - 1.15).abs() < 1e-6);
        assert!((step(1.0, 5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn set_and_step_target_the_right_device() {
        let mut m = model();
        let acts = m.apply(Command::SetVolume(Target::DefaultSink, 0.25));
        assert_eq!(acts.len(), 1);
        let Action::Volume { dir, name, channels } = &acts[0] else { panic!() };
        assert_eq!((*dir, name.as_str()), (Dir::Sink, "a"));
        assert!((volume_of(channels) - 0.25).abs() < 1e-3);
        // steps compose before the server answers
        m.apply(Command::StepVolume(Target::DefaultSink, 5));
        m.apply(Command::StepVolume(Target::DefaultSink, 5));
        assert!((m.state().default_sink().unwrap().volume - 0.35).abs() < 2e-3);
        // other devices are untouched
        assert!((m.state().sinks[1].volume - 1.2).abs() < 1e-6);
        // sources by name
        let acts = m.apply(Command::StepVolume(Target::Source("mic".into()), -10));
        assert!(matches!(&acts[0], Action::Volume { dir: Dir::Source, name, .. } if name == "mic"));
        assert!((m.state().default_source().unwrap().volume - 0.7).abs() < 2e-3);
    }

    #[test]
    fn mute_set_and_toggle() {
        let mut m = model();
        assert_eq!(m.apply(Command::ToggleMute(Target::DefaultSink)), vec![Action::Mute { dir: Dir::Sink, name: "a".into(), muted: true }]);
        assert!(m.state().default_sink().unwrap().muted);
        m.apply(Command::ToggleMute(Target::DefaultSink));
        assert!(!m.state().default_sink().unwrap().muted);
        assert_eq!(
            m.apply(Command::SetMute(Target::Sink("b".into()), false)),
            vec![Action::Mute { dir: Dir::Sink, name: "b".into(), muted: false }]
        );
        assert!(!m.state().sinks[1].muted);
    }

    #[test]
    fn set_default_moves_the_flag_and_ignores_unknown_names() {
        let mut m = model();
        assert!(m.apply(Command::SetDefault(Target::Sink("nope".into()))).is_empty());
        assert_eq!(
            m.apply(Command::SetDefault(Target::Sink("b".into()))),
            vec![Action::MakeDefault { dir: Dir::Sink, name: "b".into() }]
        );
        let s = m.state();
        assert_eq!(s.default_sink().unwrap().name, "b");
        assert_eq!(s.sinks.iter().filter(|d| d.is_default).count(), 1);
    }

    #[test]
    fn nothing_happens_without_a_server_or_a_default() {
        let mut m = Model::default();
        assert!(m.apply(Command::ToggleMute(Target::DefaultSink)).is_empty());
        let mut m = model();
        m.default_sink = None;
        assert!(m.apply(Command::StepVolume(Target::DefaultSink, 5)).is_empty());
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of(None, Some("bluetooth"), None, "bluez_output.x", false), DeviceKind::Bluetooth);
        assert_eq!(kind_of(None, None, Some("hdmi-output-0"), "alsa_output.hdmi-stereo", false), DeviceKind::Hdmi);
        assert_eq!(kind_of(None, None, Some("analog-output-headphones"), "x", false), DeviceKind::Headphones);
        assert_eq!(kind_of(Some("speaker"), None, None, "x", false), DeviceKind::Speakers);
        assert_eq!(kind_of(None, None, None, "x", true), DeviceKind::Microphone);
        assert_eq!(kind_of(None, None, None, "x", false), DeviceKind::Other);
    }
}
