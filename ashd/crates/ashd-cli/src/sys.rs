//! `ashd sys …`: NixOS helpers (hardware config, FlClashX TUN copy).

use std::{
    ffi::OsString,
    fs, io,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, bail};

/// Env var naming the `nixos-generate-config` binary (default: from `$PATH`).
pub const GENERATE_CONFIG_ENV: &str = "ASHD_NIXOS_GENERATE_CONFIG";

// ── generate-hardware-config ──────────────────────────────────────────────────

/// Host names become directory names under `hosts/`: no separators, no traversal.
pub fn validate_host(host: &str) -> anyhow::Result<()> {
    let ok = !host.is_empty()
        && !host.starts_with('.')
        && host.len() <= 255
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!("invalid host name `{host}` (use letters, digits, `-`, `_`, `.`; no slashes)");
    }
    Ok(())
}

/// `<flake_dir>/hosts/<host>/hardware-configuration.nix`. An empty `flake_dir` stays relative.
pub fn hardware_config_path(flake_dir: &Path, host: &str) -> PathBuf {
    flake_dir.join("hosts").join(host).join("hardware-configuration.nix")
}

/// Create `hosts/<host>/` and write `nixos-generate-config --show-hardware-config` into it.
/// Returns the written path. The file is replaced atomically and only if the generator
/// succeeded.
pub fn generate_hardware_config(host: &str, flake_dir: &Path) -> anyhow::Result<PathBuf> {
    validate_host(host)?;
    let bin = std::env::var_os(GENERATE_CONFIG_ENV)
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| OsString::from("nixos-generate-config"));
    let out = Command::new(&bin).arg("--show-hardware-config").output().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            anyhow::anyhow!("`{}` not found in PATH (it is in nixos-install-tools; run this on NixOS)", bin.to_string_lossy())
        } else {
            anyhow::Error::new(e).context(format!("running {}", bin.to_string_lossy()))
        }
    })?;
    if !out.status.success() {
        bail!("{} failed ({}): {}", bin.to_string_lossy(), out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    if out.stdout.is_empty() {
        bail!("{} printed nothing; not overwriting anything", bin.to_string_lossy());
    }
    let path = hardware_config_path(flake_dir, host);
    // `create_dir_all` and `write_atomic` both create the parents.
    ashd_tokens::state::write_atomic(&path, &out.stdout).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

// ── flclashx-sync ─────────────────────────────────────────────────────────────

/// What [`flclashx_sync`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    /// `<dst>/.source` already named `<src>`.
    UpToDate,
    Synced,
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Remove whatever is at `path` (file, symlink, or tree) without following symlinks.
fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// FlClashX only enables TUN when `FlClashCore` next to the executable is root-owned and
/// setuid, which the read-only store cannot hold. This builds `<dst>` from the package at
/// `<src>`: the real `FlClashX`, a setuid copy of `FlClashCore`, and `lib/` + `data/`
/// linked back into the store. Runs as root from NixOS activation, so it is idempotent
/// (`<dst>/.source` records the package it was built from) and never leaves `<dst>`
/// half-built: everything is assembled in `<dst>.new` and swapped in at the end.
pub fn flclashx_sync(src: &Path, dst: &Path) -> anyhow::Result<SyncOutcome> {
    let src = std::path::absolute(src).with_context(|| format!("resolving {}", src.display()))?;
    let dst = std::path::absolute(dst).with_context(|| format!("resolving {}", dst.display()))?;
    let marker = src.to_string_lossy().into_owned();

    if let Ok(current) = fs::read_to_string(dst.join(".source"))
        && current.trim_end_matches('\n') == marker
    {
        return Ok(SyncOutcome::UpToDate);
    }

    let staging = with_suffix(&dst, ".new");
    remove_any(&staging).with_context(|| format!("removing {}", staging.display()))?;
    if let Err(e) = build_staging(&src, &staging, &marker) {
        let _ = remove_any(&staging);
        return Err(e);
    }
    let swap = || -> io::Result<()> {
        remove_any(&dst)?;
        fs::rename(&staging, &dst)
    };
    swap().with_context(|| format!("replacing {} with {}", dst.display(), staging.display()))?;
    Ok(SyncOutcome::Synced)
}

fn build_staging(src: &Path, staging: &Path, marker: &str) -> anyhow::Result<()> {
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::create_dir(staging).with_context(|| format!("creating {}", staging.display()))?;
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;

    let copy = |name: &str, mode: Option<u32>| -> anyhow::Result<()> {
        let from = src.join(name);
        let to = staging.join(name);
        fs::copy(&from, &to).with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
        if let Some(mode) = mode {
            fs::set_permissions(&to, fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    };
    copy("FlClashX", None)?;
    // install -m 4755: owned by whoever runs this (root at activation), setuid.
    copy("FlClashCore", Some(0o4755))?;
    for name in ["lib", "data"] {
        let target = src.join(name);
        symlink(&target, staging.join(name))
            .with_context(|| format!("linking {} -> {}", staging.join(name).display(), target.display()))?;
    }
    fs::write(staging.join(".source"), format!("{marker}\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn host_names() {
        for ok in ["nixos", "Ivant-main", "box_1", "a.b", "x"] {
            validate_host(ok).unwrap();
        }
        for bad in ["", ".", "..", ".hidden", "a/b", "../etc", "a b", "a\nb", "ü", "/abs", "a\\b"] {
            assert!(validate_host(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hardware_config_paths() {
        assert_eq!(hardware_config_path(Path::new(""), "box"), PathBuf::from("hosts/box/hardware-configuration.nix"));
        assert_eq!(
            hardware_config_path(Path::new("/flake"), "box"),
            PathBuf::from("/flake/hosts/box/hardware-configuration.nix")
        );
    }

    fn package(dir: &Path) -> PathBuf {
        let src = dir.join("store/opt/FlClashX");
        fs::create_dir_all(src.join("lib")).unwrap();
        fs::create_dir_all(src.join("data")).unwrap();
        fs::write(src.join("FlClashX"), "#!flclashx\n").unwrap();
        fs::write(src.join("FlClashCore"), "#!core\n").unwrap();
        fs::set_permissions(src.join("FlClashX"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(src.join("FlClashCore"), fs::Permissions::from_mode(0o555)).unwrap();
        src
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn sync_builds_the_tun_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = package(tmp.path());
        let dst = tmp.path().join("var/lib/flclashx");
        assert_eq!(flclashx_sync(&src, &dst).unwrap(), SyncOutcome::Synced);

        assert_eq!(fs::read_to_string(dst.join("FlClashX")).unwrap(), "#!flclashx\n");
        assert_eq!(fs::read_to_string(dst.join("FlClashCore")).unwrap(), "#!core\n");
        assert_eq!(mode(&dst.join("FlClashCore")), 0o4755, "setuid + rwxr-xr-x");
        assert_eq!(mode(&dst.join("FlClashX")), 0o555, "keeps the package's mode");
        assert_eq!(fs::metadata(dst.join("FlClashCore")).unwrap().uid(), nix::unistd::Uid::current().as_raw());
        assert_eq!(fs::read_link(dst.join("lib")).unwrap(), src.join("lib"));
        assert_eq!(fs::read_link(dst.join("data")).unwrap(), src.join("data"));
        assert!(dst.join("lib").is_dir() && dst.join("data").is_dir(), "links resolve");
        assert_eq!(fs::read_to_string(dst.join(".source")).unwrap(), format!("{}\n", src.display()));
        assert!(!with_suffix(&dst, ".new").exists());
        assert_eq!(mode(&dst), 0o755);
    }

    #[test]
    fn sync_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = package(tmp.path());
        let dst = tmp.path().join("flclashx");
        assert_eq!(flclashx_sync(&src, &dst).unwrap(), SyncOutcome::Synced);
        fs::write(dst.join("sentinel"), "still here").unwrap();
        // Same source again (also with a no-newline marker file, as `echo -n` would write).
        assert_eq!(flclashx_sync(&src, &dst).unwrap(), SyncOutcome::UpToDate);
        fs::write(dst.join(".source"), src.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(flclashx_sync(&src, &dst).unwrap(), SyncOutcome::UpToDate);
        assert!(dst.join("sentinel").exists(), "nothing was rebuilt");
    }

    #[test]
    fn sync_replaces_a_stale_install() {
        let tmp = tempfile::tempdir().unwrap();
        let old = package(tmp.path());
        let dst = tmp.path().join("flclashx");
        flclashx_sync(&old, &dst).unwrap();
        fs::write(dst.join("sentinel"), "x").unwrap();

        // A new package version lives somewhere else.
        let new_root = tmp.path().join("new");
        fs::create_dir_all(&new_root).unwrap();
        let new = package(&new_root);
        fs::remove_file(new.join("FlClashX")).unwrap(); // read-only file: replace, not write
        fs::write(new.join("FlClashX"), "v2").unwrap();
        // A crashed earlier run left a half-built staging dir with junk (even a symlink in it).
        let staging = with_suffix(&dst, ".new");
        fs::create_dir_all(staging.join("junk")).unwrap();
        symlink("/nonexistent", staging.join("dangling")).unwrap();

        assert_eq!(flclashx_sync(&new, &dst).unwrap(), SyncOutcome::Synced);
        assert_eq!(fs::read_to_string(dst.join("FlClashX")).unwrap(), "v2");
        assert_eq!(fs::read_link(dst.join("lib")).unwrap(), new.join("lib"));
        assert!(!dst.join("sentinel").exists() && !staging.exists());
        assert_eq!(fs::read_to_string(dst.join(".source")).unwrap().trim_end(), new.to_string_lossy());
    }

    #[test]
    fn sync_replaces_a_plain_file_or_symlink_at_dst() {
        let tmp = tempfile::tempdir().unwrap();
        let src = package(tmp.path());
        let victim = tmp.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep"), "k").unwrap();
        let dst = tmp.path().join("dst");
        symlink(&victim, &dst).unwrap();
        flclashx_sync(&src, &dst).unwrap();
        assert!(dst.join("FlClashX").exists() && !fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
        assert!(victim.join("keep").exists(), "the symlink was removed, not followed");

        let file_dst = tmp.path().join("filedst");
        fs::write(&file_dst, "junk").unwrap();
        flclashx_sync(&src, &file_dst).unwrap();
        assert!(file_dst.join("FlClashCore").exists());
    }

    #[test]
    fn a_broken_package_leaves_the_old_install_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let good = package(tmp.path());
        let dst = tmp.path().join("flclashx");
        flclashx_sync(&good, &dst).unwrap();

        let broken = tmp.path().join("broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("FlClashX"), "x").unwrap(); // no FlClashCore
        let err = flclashx_sync(&broken, &dst).unwrap_err();
        assert!(format!("{err:#}").contains("FlClashCore"), "{err:#}");
        assert_eq!(fs::read_to_string(dst.join("FlClashX")).unwrap(), "#!flclashx\n");
        assert_eq!(fs::read_to_string(dst.join(".source")).unwrap().trim_end(), good.to_string_lossy());
        assert!(!with_suffix(&dst, ".new").exists(), "staging cleaned up");

        // Missing source directory entirely.
        assert!(flclashx_sync(&tmp.path().join("nowhere"), &dst).is_err());
        assert!(dst.join("FlClashX").exists());
    }

    #[test]
    fn relative_paths_are_made_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let src = package(tmp.path());
        let dst = tmp.path().join("d");
        // `..` segments in the source are kept as-is but the link target must be absolute.
        let odd = src.join("..").join("FlClashX");
        flclashx_sync(&odd, &dst).unwrap();
        assert!(fs::read_link(dst.join("lib")).unwrap().is_absolute());
    }
}
