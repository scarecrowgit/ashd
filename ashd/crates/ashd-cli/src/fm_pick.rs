//! `ashd sys fm-pick`: the file-chooser backend for xdg-desktop-portal-termfilechooser.
//! The portal calls it as `<multiple> <directory> <save> <path> <outfile> [<debug>]`
//! with `0`/`1` flags.

use std::{os::unix::process::CommandExt as _, process::Command};

/// `--save` if saving, else `--directory`, else `--multiple`, else no mode.
pub fn mode(multiple: &str, directory: &str, save: &str) -> Option<&'static str> {
    if save == "1" {
        Some("--save")
    } else if directory == "1" {
        Some("--directory")
    } else if multiple == "1" {
        Some("--multiple")
    } else {
        None
    }
}

/// Arguments of `fm`: `--pick [mode] --out <outfile> <path or home>`.
pub fn fm_args(multiple: &str, directory: &str, save: &str, path: &str, outfile: &str, home: &str) -> Vec<String> {
    let mut args = vec!["--pick".to_owned()];
    args.extend(mode(multiple, directory, save).map(str::to_owned));
    args.push("--out".into());
    args.push(outfile.into());
    args.push(if path.is_empty() { home.into() } else { path.into() });
    args
}

/// Replace this process with fm (like `ashd fm`: `$ASHD_FM_BIN` or `fm`, `--theme
/// <current>` unless fm is unchecked in the theme switcher). Only returns on failure.
pub fn exec(args: &[String], debug: bool) -> anyhow::Error {
    let (bin, argv) = ashd_theme::fm_command(args);
    if debug {
        eprintln!("{} {}", bin.to_string_lossy(), argv.iter().map(|a| shell_words(a)).collect::<Vec<_>>().join(" "));
    }
    let err = Command::new(&bin).args(&argv).exec();
    if err.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("`{}` not found (set $ASHD_FM_BIN or put fm in PATH)", bin.to_string_lossy())
    } else {
        anyhow::Error::new(err).context("running fm")
    }
}

/// Quote for display only.
fn shell_words(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=+:@%,".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_priority() {
        assert_eq!(mode("0", "0", "0"), None);
        assert_eq!(mode("1", "0", "0"), Some("--multiple"));
        assert_eq!(mode("0", "1", "0"), Some("--directory"));
        assert_eq!(mode("0", "0", "1"), Some("--save"));
        assert_eq!(mode("1", "1", "0"), Some("--directory"));
        assert_eq!(mode("1", "1", "1"), Some("--save"));
        assert_eq!(mode("1", "0", "1"), Some("--save"));
        assert_eq!(mode("yes", "true", ""), None, "only the literal 1 counts");
    }

    #[test]
    fn argv() {
        assert_eq!(
            fm_args("0", "0", "0", "/data", "/tmp/out", "/home/u"),
            ["--pick", "--out", "/tmp/out", "/data"]
        );
        assert_eq!(
            fm_args("0", "0", "1", "", "/tmp/out", "/home/u"),
            ["--pick", "--save", "--out", "/tmp/out", "/home/u"]
        );
        assert_eq!(
            fm_args("1", "0", "0", "/a b", "/tmp/o", "/h"),
            ["--pick", "--multiple", "--out", "/tmp/o", "/a b"]
        );
    }

    #[test]
    fn display_quoting() {
        assert_eq!(shell_words("/tmp/out"), "/tmp/out");
        assert_eq!(shell_words("/a b"), "'/a b'");
        assert_eq!(shell_words(""), "''");
    }
}
