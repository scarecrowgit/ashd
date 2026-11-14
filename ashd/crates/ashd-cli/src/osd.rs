//! `ashd osd …` without a running shell: change the default sink / source directly with
//! `wpctl` so the media keys keep working.

use std::{ffi::OsString, process::Command};

use anyhow::bail;
use ashd_ipc::{OsdRequest, VolumeChange};

/// Env var naming the `wpctl` binary (default: `wpctl` from `$PATH`).
pub const WPCTL_ENV: &str = "ASHD_WPCTL";

/// Maximum volume as a factor, same as the shell's audio service (`MAX_VOLUME`).
const MAX_VOLUME: &str = "1.5";

/// The `wpctl` arguments equivalent to `osd`; `None` if there is no equivalent.
pub fn wpctl_args(osd: &OsdRequest) -> Option<Vec<String>> {
    let args: &[&str] = match osd {
        OsdRequest::Volume(VolumeChange::Step(0)) => return None,
        OsdRequest::Volume(VolumeChange::Step(n)) => {
            let sign = if *n > 0 { '+' } else { '-' };
            return Some(vec![
                "set-volume".into(),
                "-l".into(),
                MAX_VOLUME.into(),
                "@DEFAULT_AUDIO_SINK@".into(),
                format!("{}%{sign}", n.unsigned_abs()),
            ]);
        }
        OsdRequest::Volume(VolumeChange::Mute) => &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"],
        OsdRequest::MicMute => &["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"],
        OsdRequest::Brightness(_) => return None,
    };
    Some(args.iter().map(|s| (*s).to_owned()).collect())
}

/// Whether [`fallback`] can do anything for `osd`.
pub fn has_fallback(osd: &OsdRequest) -> bool {
    !matches!(osd, OsdRequest::Brightness(_))
}

/// Run the `wpctl` equivalent of `osd`. Errors if `wpctl` is missing or fails.
pub fn fallback(osd: &OsdRequest) -> anyhow::Result<()> {
    let Some(args) = wpctl_args(osd) else { return Ok(()) };
    let bin = std::env::var_os(WPCTL_ENV).filter(|b| !b.is_empty()).unwrap_or_else(|| OsString::from("wpctl"));
    let out = Command::new(&bin).args(&args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("`{}` not found (PipeWire's wpctl is needed while ashd-shell is not running)", bin.to_string_lossy())
        } else {
            anyhow::Error::new(e).context(format!("running {}", bin.to_string_lossy()))
        }
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("wpctl {} failed ({}): {}", args.join(" "), out.status, stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(o: OsdRequest) -> Option<Vec<String>> {
        wpctl_args(&o)
    }

    #[test]
    fn volume_steps() {
        assert_eq!(
            args(OsdRequest::Volume(VolumeChange::Step(5))).unwrap(),
            ["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", "5%+"]
        );
        assert_eq!(
            args(OsdRequest::Volume(VolumeChange::Step(-10))).unwrap(),
            ["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", "10%-"]
        );
        assert_eq!(args(OsdRequest::Volume(VolumeChange::Step(0))), None);
        // i32::MIN must not overflow.
        assert!(args(OsdRequest::Volume(VolumeChange::Step(i32::MIN))).unwrap()[4].ends_with("%-"));
    }

    #[test]
    fn mutes() {
        assert_eq!(args(OsdRequest::Volume(VolumeChange::Mute)).unwrap(), ["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
        assert_eq!(args(OsdRequest::MicMute).unwrap(), ["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"]);
    }

    #[test]
    fn brightness_has_no_fallback() {
        assert_eq!(args(OsdRequest::Brightness(5)), None);
        assert!(!has_fallback(&OsdRequest::Brightness(5)));
        assert!(has_fallback(&OsdRequest::MicMute));
    }
}
