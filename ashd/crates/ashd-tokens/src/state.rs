//! The persisted current theme and the `ashd theme apply` file writer.
//!
//! Layout (contract with the NixOS config, see ARCHITECTURE.md "Theme files"):
//!
//! ```text
//! $XDG_STATE_HOME/ashd/theme                    current theme name, e.g. `ash\n`
//! $XDG_STATE_HOME/ashd/generated/kitty.conf     render::Target::Kitty
//! $XDG_STATE_HOME/ashd/generated/starship-palette.toml
//! $XDG_STATE_HOME/ashd/generated/starship.toml  base config (`/etc/ashd/starship-base.toml`) + palette
//! $XDG_STATE_HOME/ashd/generated/nvim.lua
//! $XDG_STATE_HOME/ashd/generated/gtk.css
//! $XDG_STATE_HOME/ashd/generated/niri.kdl
//! ```
//! The theme is resolved as: state `theme` file, then `/etc/ashd/theme` (NixOS; `$ASHD_ETC_DIR`
//! overrides `/etc/ashd`), then Ash.
//! (`$XDG_STATE_HOME` defaults to `~/.local/state`; `$ASHD_STATE_DIR` overrides the whole
//! `…/ashd` directory, used by tests and the nested test session.)

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{ThemeName, TokenSet, render::Target};

/// `$ASHD_STATE_DIR` or `$XDG_STATE_HOME/ashd` or `~/.local/state/ashd`.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ASHD_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("ashd")
}

pub fn theme_file() -> PathBuf {
    state_dir().join("theme")
}

pub fn generated_dir() -> PathBuf {
    state_dir().join("generated")
}

pub fn generated_path(target: Target) -> PathBuf {
    generated_dir().join(target.file_name())
}

/// Directory of the files the NixOS config provides: `/etc/ashd`, or `$ASHD_ETC_DIR`
/// (tests, nested session).
pub fn etc_dir() -> PathBuf {
    std::env::var_os("ASHD_ETC_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc/ashd"))
}

/// System-wide default theme written by the NixOS config (`<etc>/theme`).
pub fn system_theme_file() -> PathBuf {
    etc_dir().join("theme")
}

/// The NixOS starship config without a palette (`<etc>/starship-base.toml`).
pub fn starship_base_file() -> PathBuf {
    etc_dir().join("starship-base.toml")
}

/// The current theme. Resolution order: the persisted state file ([`theme_file`]), then
/// the system file ([`system_theme_file`]), then the tokens.json default (Ash). Unreadable
/// or invalid files are skipped.
pub fn current_theme() -> ThemeName {
    resolve_theme(&theme_file(), &system_theme_file())
}

/// [`current_theme`] with explicit file paths.
pub fn resolve_theme(state_file: &Path, system_file: &Path) -> ThemeName {
    read_theme(state_file)
        .or_else(|| read_theme(system_file))
        .unwrap_or(TokenSet::builtin().default)
}

fn read_theme(path: &Path) -> Option<ThemeName> {
    fs::read_to_string(path).ok().and_then(|s| ThemeName::from_label(&s))
}

/// Persist `name` as the current theme (atomic write).
pub fn set_current_theme(name: ThemeName) -> io::Result<()> {
    write_atomic(&theme_file(), format!("{name}\n").as_bytes())
}

/// Persist `name` as the current theme and render every [`Target`] for it into
/// [`generated_dir`]. Returns the written paths. Does not reload anything: the CLI
/// signals kitty / niri / GTK afterwards.
pub fn apply(name: ThemeName) -> io::Result<Vec<PathBuf>> {
    set_current_theme(name)?;
    apply_to_in(name, &generated_dir(), &etc_dir())
}

/// `ashd theme apply`: resolve the theme ([`current_theme`]: state file, then
/// `<etc>/theme`, then Ash), persist it if the state file did not exist yet, and render.
pub fn apply_current() -> io::Result<(ThemeName, Vec<PathBuf>)> {
    let name = current_theme();
    Ok((name, apply(name)?))
}

/// Render every target into `dir`; the starship base config is read from [`etc_dir`].
pub fn apply_to(name: ThemeName, dir: &Path) -> io::Result<Vec<PathBuf>> {
    apply_to_in(name, dir, &etc_dir())
}

/// [`apply_to`] with an explicit `etc` directory (`starship-base.toml` is optional).
pub fn apply_to_in(name: ThemeName, dir: &Path, etc: &Path) -> io::Result<Vec<PathBuf>> {
    apply_selected_to_in(name, dir, etc, &Target::ALL)
}

/// [`apply`] restricted to `targets`: persist `name` as the current theme (always) and
/// render only the given [`Target`]s into [`generated_dir`]; the files of every other
/// target keep their previous content. The theme switcher's "apply to" checkboxes use this.
pub fn apply_selected(name: ThemeName, targets: &[Target]) -> io::Result<Vec<PathBuf>> {
    set_current_theme(name)?;
    apply_selected_to_in(name, &generated_dir(), &etc_dir(), targets)
}

/// Render exactly `targets` into `dir` (others untouched); `etc` as in [`apply_to_in`].
/// Duplicates in `targets` are written once. Returns the written paths in `targets` order.
pub fn apply_selected_to_in(name: ThemeName, dir: &Path, etc: &Path, targets: &[Target]) -> io::Result<Vec<PathBuf>> {
    let tokens = TokenSet::builtin().get(name);
    let base = fs::read_to_string(etc.join("starship-base.toml")).ok();
    let mut unique: Vec<Target> = Vec::with_capacity(targets.len());
    for t in targets {
        if !unique.contains(t) {
            unique.push(*t);
        }
    }
    unique
        .into_iter()
        .map(|target| {
            let path = dir.join(target.file_name());
            let text = match target {
                Target::StarshipConfig => crate::render::starship_full(tokens, base.as_deref()),
                other => other.render(tokens),
            };
            write_atomic(&path, text.as_bytes())?;
            Ok(path)
        })
        .collect()
}

/// Write via a temp file + rename so readers (niri's config watcher) never see a
/// half-written file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_writes_all_targets() {
        let dir = tempfile::tempdir().unwrap();
        let written = apply_to(ThemeName::Moss, dir.path()).unwrap();
        assert_eq!(written.len(), Target::ALL.len());
        let niri = fs::read_to_string(dir.path().join("niri.kdl")).unwrap();
        assert!(niri.contains("#A6D1A4"));
        assert!(read_theme(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn apply_selected_leaves_other_targets_untouched() {
        let (out, etc) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        apply_to_in(ThemeName::Ash, out.path(), etc.path()).unwrap();
        let before = |t: Target| fs::read_to_string(out.path().join(t.file_name())).unwrap();
        let (kitty_ash, nvim_ash) = (before(Target::Kitty), before(Target::Nvim));
        // Moss for the niri file and (listed twice) the GTK file only.
        let written =
            apply_selected_to_in(ThemeName::Moss, out.path(), etc.path(), &[Target::Niri, Target::Gtk, Target::Niri]).unwrap();
        assert_eq!(written, vec![out.path().join("niri.kdl"), out.path().join("gtk.css")]);
        assert!(before(Target::Niri).contains("#A6D1A4"), "niri follows Moss");
        assert!(before(Target::Gtk).to_lowercase().contains("a6d1a4"), "gtk follows Moss");
        assert_eq!(before(Target::Kitty), kitty_ash, "kitty keeps Ash");
        assert_eq!(before(Target::Nvim), nvim_ash, "nvim keeps Ash");
        // Nothing selected: nothing written.
        assert!(apply_selected_to_in(ThemeName::Dusk, out.path(), etc.path(), &[]).unwrap().is_empty());
        assert_eq!(before(Target::Kitty), kitty_ash);
        // A missing directory is created for the first write.
        let fresh = out.path().join("nested").join("generated");
        apply_selected_to_in(ThemeName::Ink, &fresh, etc.path(), &[Target::Kitty]).unwrap();
        assert!(fresh.join("kitty.conf").exists() && !fresh.join("nvim.lua").exists());
    }

    #[test]
    fn resolution_order_state_system_default() {
        let dir = tempfile::tempdir().unwrap();
        let (state, system) = (dir.path().join("state-theme"), dir.path().join("system-theme"));
        assert_eq!(resolve_theme(&state, &system), ThemeName::Ash, "nothing stored");
        fs::write(&system, "midnight\n").unwrap();
        assert_eq!(resolve_theme(&state, &system), ThemeName::Midnight, "system file");
        fs::write(&state, " Dusk \n").unwrap();
        assert_eq!(resolve_theme(&state, &system), ThemeName::Dusk, "state wins");
        fs::write(&state, "garbage").unwrap();
        assert_eq!(resolve_theme(&state, &system), ThemeName::Midnight, "invalid state skipped");
        fs::write(&system, "garbage").unwrap();
        assert_eq!(resolve_theme(&state, &system), ThemeName::Ash, "both invalid");
    }

    #[test]
    fn apply_to_writes_starship_config_with_and_without_base() {
        let (out, etc) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // no base file
        apply_to_in(ThemeName::Dusk, out.path(), etc.path()).unwrap();
        let minimal = fs::read_to_string(out.path().join("starship.toml")).unwrap();
        assert!(minimal.starts_with("palette = \"ashd\"\n") && minimal.contains("accent = \"#B7A4FF\""));
        assert!(out.path().join("starship-palette.toml").exists());
        // with the NixOS base
        fs::write(etc.path().join("starship-base.toml"), "format = \"$directory\"\n[directory]\nstyle = \"fg:accent\"\n").unwrap();
        let written = apply_to_in(ThemeName::Dusk, out.path(), etc.path()).unwrap();
        assert!(written.iter().any(|p| p.ends_with("starship.toml")));
        let full = fs::read_to_string(out.path().join("starship.toml")).unwrap();
        assert!(full.contains("style = \"fg:accent\"") && full.contains("[palettes.ashd]"));
        assert!(full.find("[directory]").unwrap() < full.find("[palettes.ashd]").unwrap());
    }

    /// Env vars are process-global: this is the only test touching `ASHD_*` variables.
    #[test]
    fn apply_current_reads_etc_theme_and_persists() {
        let (state, etc) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = state.path().join("ashd");
        // SAFETY: single-threaded access to these variables within this one test.
        unsafe {
            std::env::set_var("ASHD_STATE_DIR", &state);
            std::env::set_var("ASHD_ETC_DIR", etc.path());
        }
        // fresh state dir, no /etc theme -> Ash
        assert_eq!(current_theme(), ThemeName::Ash);
        // fresh state dir + /etc/ashd/theme -> that theme, persisted, files rendered with it
        fs::write(etc.path().join("theme"), "midnight\n").unwrap();
        assert_eq!(current_theme(), ThemeName::Midnight);
        let (name, written) = apply_current().unwrap();
        assert_eq!(name, ThemeName::Midnight);
        assert_eq!(written.len(), Target::ALL.len());
        assert_eq!(fs::read_to_string(state.join("theme")).unwrap(), "midnight\n");
        assert!(fs::read_to_string(state.join("generated/niri.kdl")).unwrap().contains("#7088FF"));
        // the persisted state now wins over /etc
        fs::write(etc.path().join("theme"), "moss\n").unwrap();
        assert_eq!(current_theme(), ThemeName::Midnight);
        // `apply(name)` persists too
        apply(ThemeName::Ink).unwrap();
        assert_eq!(current_theme(), ThemeName::Ink);
        unsafe {
            std::env::remove_var("ASHD_STATE_DIR");
            std::env::remove_var("ASHD_ETC_DIR");
        }
    }

    #[test]
    fn set_current_theme_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ashd").join("theme");
        write_atomic(&file, b"moss\n").unwrap();
        assert_eq!(read_theme(&file), Some(ThemeName::Moss));
        // no stray temp files
        assert_eq!(fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
    }
}
