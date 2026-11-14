//! Applying a theme — the one implementation behind `ashd theme set|cycle|apply`, the
//! quick-settings theme row and the theme switcher. No GPUI: the shell calls it from a
//! background task (it does blocking file I/O and a blocking niri request).
//!
//! The sequence ([`apply`]):
//! 1. persist the theme name (`<state>/theme`, read by `ashd-shell` at startup);
//! 2. write the generated files of the selected consumers ([`ApplyTo`], persisted in
//!    `<state>/apply-to.json` by the theme switcher's "apply to" checkboxes; everything on by
//!    default) — `ashd_tokens::state::apply_selected`; files of unchecked consumers keep
//!    their previous content;
//! 3. reload the consumers — niri `LoadConfigFile` (its config `include`s the generated
//!    `niri.kdl`), kitty `SIGUSR1` (only if kitty is selected) — **unless** `ASHD_NO_RELOAD`
//!    or `ASHD_FAKE_POWER` is set (nested session, tests: signalling would hit the user's
//!    real kitty / niri).
//!
//! Switching the shell's own surfaces (`ashd_ui::set_theme`, or `Request::Theme(Set)` from
//! the CLI) is the caller's part.
//!
//! | consumer | generated files (`ashd_tokens::render::Target`) | reload |
//! |---|---|---|
//! | Shell (always) | `niri.kdl` (+ the `theme` file) | niri `LoadConfigFile` |
//! | kitty | `kitty.conf`, `starship-palette.toml`, `starship.toml` | `SIGUSR1` to kitty |
//! | nvim | `nvim.lua` | none (next `:colorscheme` / start) |
//! | fm | none: `ashd fm` passes `--theme <current>` ([`fm_command`]) | none |
//! | GTK apps | `gtk.css` | none (GTK re-reads on restart) |

use std::{
    ffi::OsString,
    fs, io,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use ashd_tokens::{ThemeName, render::Target, state};
use serde::{Deserialize, Serialize};

// ── consumers / "apply to" ─────────────────────────────────────────────────────

/// A row of the theme switcher's "apply to" strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consumer {
    Shell,
    Kitty,
    Nvim,
    Fm,
    Gtk,
}

impl Consumer {
    pub const ALL: [Consumer; 5] = [Self::Shell, Self::Kitty, Self::Nvim, Self::Fm, Self::Gtk];

    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "Shell",
            Self::Kitty => "kitty",
            Self::Nvim => "nvim",
            Self::Fm => "fm",
            Self::Gtk => "GTK apps",
        }
    }

    /// The shell cannot be switched off: it is what the switcher is part of.
    pub fn always_on(self) -> bool {
        self == Self::Shell
    }

    /// Generated files this consumer reads.
    pub fn targets(self) -> &'static [Target] {
        match self {
            Self::Shell => &[Target::Niri],
            Self::Kitty => &[Target::Kitty, Target::Starship, Target::StarshipConfig],
            Self::Nvim => &[Target::Nvim],
            Self::Fm => &[],
            Self::Gtk => &[Target::Gtk],
        }
    }
}

/// The persisted checkboxes (`<state dir>/apply-to.json`). Everything defaults to on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplyTo {
    pub kitty: bool,
    pub nvim: bool,
    /// `ashd fm` passes `--theme <current theme>` to fm.
    pub fm: bool,
    pub gtk: bool,
}

impl Default for ApplyTo {
    fn default() -> Self {
        Self { kitty: true, nvim: true, fm: true, gtk: true }
    }
}

impl ApplyTo {
    pub fn get(&self, c: Consumer) -> bool {
        match c {
            Consumer::Shell => true,
            Consumer::Kitty => self.kitty,
            Consumer::Nvim => self.nvim,
            Consumer::Fm => self.fm,
            Consumer::Gtk => self.gtk,
        }
    }

    /// Setting the shell is a no-op.
    pub fn set(&mut self, c: Consumer, on: bool) {
        match c {
            Consumer::Shell => {}
            Consumer::Kitty => self.kitty = on,
            Consumer::Nvim => self.nvim = on,
            Consumer::Fm => self.fm = on,
            Consumer::Gtk => self.gtk = on,
        }
    }

    /// The generated files to rewrite, in [`Consumer::ALL`] order.
    pub fn targets(&self) -> Vec<Target> {
        Consumer::ALL.into_iter().filter(|c| self.get(*c)).flat_map(|c| c.targets().iter().copied()).collect()
    }

    /// Stored selection, or the defaults when the file is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// The selection stored in [`apply_to_path`].
    pub fn load_saved() -> Self {
        Self::load(&apply_to_path())
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        state::write_atomic(path, format!("{json}\n").as_bytes())
    }
}

/// `<state dir>/apply-to.json`.
pub fn apply_to_path() -> PathBuf {
    state::state_dir().join("apply-to.json")
}

// ── reload ─────────────────────────────────────────────────────────────────────

/// Consumers are signalled unless a test / nested session switched it off: `ASHD_NO_RELOAD`
/// or `ASHD_FAKE_POWER` set and not `0` (the nested session sets both).
pub fn reload_enabled() -> bool {
    let on = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty() && v != "0");
    !on("ASHD_NO_RELOAD") && !on("ASHD_FAKE_POWER")
}

/// What to poke after the files are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReloadPlan {
    /// niri re-reads its config (`include`s the generated `niri.kdl`).
    pub niri: bool,
    /// kitty gets `SIGUSR1`.
    pub kitty: bool,
}

pub fn reload_plan(enabled: bool, sel: &ApplyTo) -> ReloadPlan {
    ReloadPlan { niri: enabled, kitty: enabled && sel.kitty }
}

/// What a reload did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReloadReport {
    /// kitty processes that got SIGUSR1.
    pub kitty: usize,
    /// niri was asked (false when `$NIRI_SOCKET` is not set).
    pub niri_reloaded: bool,
    /// `Some(err)` if niri could not be asked to reload its config.
    pub niri_error: Option<String>,
}

impl ReloadReport {
    /// Human-readable warnings for a reload that did not fully work.
    pub fn warnings(&self) -> Vec<String> {
        self.niri_error.iter().map(|e| format!("niri did not reload its config: {e}")).collect()
    }
}

/// `comm` names kitty runs under (`.kitty-wrapped` is the nixpkgs wrapper's real binary).
const KITTY_COMMS: [&str; 2] = ["kitty", ".kitty-wrapped"];

/// PIDs of running kitty processes under `proc_root` (normally `/proc`) owned by `uid`,
/// excluding `skip_pid`.
pub fn kitty_pids(proc_root: &Path, uid: u32, skip_pid: i32) -> Vec<i32> {
    let Ok(dir) = fs::read_dir(proc_root) else { return Vec::new() };
    let mut pids: Vec<i32> = dir
        .flatten()
        .filter_map(|e| {
            let pid: i32 = e.file_name().to_str()?.parse().ok()?;
            if pid <= 1 || pid == skip_pid {
                return None;
            }
            let path = e.path();
            if fs::metadata(&path).ok()?.uid() != uid {
                return None;
            }
            let comm = fs::read_to_string(path.join("comm")).ok()?;
            KITTY_COMMS.contains(&comm.trim()).then_some(pid)
        })
        .collect();
    pids.sort_unstable();
    pids
}

/// Carry out `plan`. Blocking (/proc scan, niri socket).
pub fn reload(plan: ReloadPlan) -> ReloadReport {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::{Pid, Uid},
    };
    let mut report = ReloadReport::default();
    if plan.kitty {
        for pid in kitty_pids(Path::new("/proc"), Uid::current().as_raw(), std::process::id() as i32) {
            if kill(Pid::from_raw(pid), Signal::SIGUSR1).is_ok() {
                report.kitty += 1;
            }
        }
    }
    if plan.niri && std::env::var_os(ashd_niri::SOCKET_ENV).is_some_and(|s| !s.is_empty()) {
        match ashd_niri::blocking::action(ashd_niri::niri_ipc::Action::LoadConfigFile { path: None }) {
            Ok(()) => report.niri_reloaded = true,
            Err(e) => report.niri_error = Some(e.to_string()),
        }
    }
    report
}

// ── apply ──────────────────────────────────────────────────────────────────────

/// What [`apply`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub theme: ThemeName,
    /// Generated files written, in [`ApplyTo::targets`] order.
    pub written: Vec<PathBuf>,
    /// `None` when reloading was not asked for or is switched off ([`reload_enabled`]).
    pub reload: Option<ReloadReport>,
}

/// Persist `name`, write the files of `sel`, and (if `reload` and [`reload_enabled`])
/// reload the consumers. Blocking: call it off the GPUI main thread.
pub fn apply(name: ThemeName, sel: &ApplyTo, reload: bool) -> io::Result<Applied> {
    let written = state::apply_selected(name, &sel.targets())?;
    let reload = (reload && reload_enabled()).then(|| self::reload(reload_plan(true, sel)));
    Ok(Applied { theme: name, written, reload })
}

/// [`apply`] with the persisted "apply to" selection — what every entry point uses.
pub fn set(name: ThemeName, reload: bool) -> io::Result<Applied> {
    apply(name, &ApplyTo::load_saved(), reload)
}

/// `ashd theme apply` (login, after a rebuild): rewrite the files of the current theme
/// (state file, then `/etc/ashd/theme`, then Ash) and persist it.
pub fn reapply_current(reload: bool) -> io::Result<Applied> {
    set(state::current_theme(), reload)
}

/// [`apply`]'s file part with explicit directories (tests): `<state_dir>/theme` and
/// `<state_dir>/generated/*`; never reloads.
pub fn write_files_in(name: ThemeName, state_dir: &Path, etc: &Path, sel: &ApplyTo) -> io::Result<Vec<PathBuf>> {
    state::write_atomic(&state_dir.join("theme"), format!("{name}\n").as_bytes())?;
    state::apply_selected_to_in(name, &state_dir.join("generated"), etc, &sel.targets())
}

// ── fm ─────────────────────────────────────────────────────────────────────────

/// The fm binary: `$ASHD_FM_BIN`, else `fm` (looked up in `PATH`).
pub fn fm_bin() -> OsString {
    std::env::var_os("ASHD_FM_BIN").filter(|s| !s.is_empty()).unwrap_or_else(|| "fm".into())
}

/// fm's argv after the binary: `--theme <theme>` when fm follows the theme (`fm` checked
/// in "apply to"), then `args` unchanged.
pub fn fm_args(theme: Option<ThemeName>, args: &[String]) -> Vec<String> {
    let mut v = Vec::with_capacity(args.len() + 2);
    if let Some(t) = theme {
        v.push("--theme".into());
        v.push(t.label().into());
    }
    v.extend(args.iter().cloned());
    v
}

/// `ashd fm [-- args…]`: binary + argv, with the current theme unless fm is unchecked.
pub fn fm_command(args: &[String]) -> (OsString, Vec<String>) {
    let theme = ApplyTo::load_saved().fm.then(state::current_theme);
    (fm_bin(), fm_args(theme, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_everywhere() {
        let all = ApplyTo::default().targets();
        for target in Target::ALL {
            assert!(all.contains(&target), "{target:?} is written when everything is checked");
        }
        assert!(Consumer::Shell.always_on() && !Consumer::Kitty.always_on());
    }

    #[test]
    fn shell_is_always_on_and_unchecked_consumers_drop_their_files() {
        let mut sel = ApplyTo::default();
        sel.set(Consumer::Shell, false);
        assert!(sel.get(Consumer::Shell), "the shell cannot be unchecked");
        sel.set(Consumer::Kitty, false);
        sel.set(Consumer::Gtk, false);
        assert_eq!(sel.targets(), vec![Target::Niri, Target::Nvim]);
        sel.set(Consumer::Nvim, false);
        sel.set(Consumer::Fm, false);
        assert_eq!(sel.targets(), vec![Target::Niri], "only the shell's file is left");
    }

    #[test]
    fn selection_roundtrips_and_tolerates_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ashd").join("apply-to.json");
        assert_eq!(ApplyTo::load(&path), ApplyTo::default(), "missing file");
        let sel = ApplyTo { kitty: false, nvim: true, fm: false, gtk: true };
        sel.save(&path).unwrap();
        assert_eq!(ApplyTo::load(&path), sel);
        fs::write(&path, "not json").unwrap();
        assert_eq!(ApplyTo::load(&path), ApplyTo::default(), "garbage");
        fs::write(&path, r#"{"gtk": false}"#).unwrap();
        assert_eq!(ApplyTo::load(&path), ApplyTo { gtk: false, ..Default::default() }, "partial file keeps defaults");
    }

    #[test]
    fn unchecked_consumers_keep_their_previous_files() {
        let (state_dir, etc) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let gen_dir = state_dir.path().join("generated");
        write_files_in(ThemeName::Ash, state_dir.path(), etc.path(), &ApplyTo::default()).unwrap();
        let read = |t: Target| fs::read_to_string(gen_dir.join(t.file_name())).unwrap();
        let (kitty_ash, nvim_ash, gtk_ash) = (read(Target::Kitty), read(Target::Nvim), read(Target::Gtk));
        let sel = ApplyTo { kitty: false, nvim: false, fm: true, gtk: true };
        let written = write_files_in(ThemeName::Moss, state_dir.path(), etc.path(), &sel).unwrap();
        assert_eq!(written, vec![gen_dir.join("niri.kdl"), gen_dir.join("gtk.css")]);
        assert_eq!(fs::read_to_string(state_dir.path().join("theme")).unwrap(), "moss\n", "shell: persisted");
        assert!(read(Target::Niri).contains("#A6D1A4"));
        assert_ne!(read(Target::Gtk), gtk_ash, "gtk followed");
        assert_eq!(read(Target::Kitty), kitty_ash, "kitty kept Ash");
        assert_eq!(read(Target::Nvim), nvim_ash, "nvim kept Ash");
        write_files_in(ThemeName::Ash, state_dir.path(), etc.path(), &ApplyTo::default()).unwrap();
        assert_eq!(read(Target::Kitty), kitty_ash);
    }

    #[test]
    fn reload_plan_follows_the_switches() {
        let sel = ApplyTo::default();
        assert_eq!(reload_plan(true, &sel), ReloadPlan { niri: true, kitty: true });
        assert_eq!(reload_plan(true, &ApplyTo { kitty: false, ..sel }), ReloadPlan { niri: true, kitty: false });
        assert_eq!(reload_plan(false, &sel), ReloadPlan { niri: false, kitty: false }, "ASHD_NO_RELOAD wins");
    }

    fn fake_proc(dir: &Path, entries: &[(&str, Option<&str>)]) {
        for (name, comm) in entries {
            let p = dir.join(name);
            fs::create_dir_all(&p).unwrap();
            if let Some(comm) = comm {
                fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            }
        }
    }

    #[test]
    fn finds_only_kitty_processes() {
        let dir = tempfile::tempdir().unwrap();
        fake_proc(
            dir.path(),
            &[
                ("100", Some("kitty")),
                ("101", Some(".kitty-wrapped")),
                ("102", Some("zsh")),
                ("103", Some("kitten")),
                ("104", Some("kitty-extra")),
                ("105", None),           // vanished between readdir and read
                ("self", Some("kitty")), // not a pid
                ("1", Some("kitty")),    // never signal init
                ("300", Some("kitty")),  // ourselves
            ],
        );
        fs::write(dir.path().join("stat"), "x").unwrap();
        let me = nix::unistd::Uid::current().as_raw();
        assert_eq!(kitty_pids(dir.path(), me, 300), vec![100, 101]);
        assert!(kitty_pids(dir.path(), me.wrapping_add(1), 300).is_empty(), "different owner");
        assert!(kitty_pids(&dir.path().join("missing"), me, 0).is_empty());
    }

    #[test]
    fn warnings() {
        assert!(ReloadReport::default().warnings().is_empty());
        let w = ReloadReport { niri_error: Some("boom".into()), ..Default::default() }.warnings();
        assert_eq!(w, vec!["niri did not reload its config: boom"]);
    }

    #[test]
    fn fm_args_put_the_theme_first() {
        let args = vec!["--pick".to_string(), "/tmp".to_string()];
        assert_eq!(fm_args(Some(ThemeName::Moss), &args), ["--theme", "moss", "--pick", "/tmp"]);
        assert_eq!(fm_args(None, &args), args, "fm unchecked: no theme");
    }

    /// Env vars are process-global: this is the only test that touches them.
    #[test]
    fn env_switches() {
        // SAFETY: single-threaded access to these variables within this one test.
        unsafe {
            std::env::remove_var("ASHD_NO_RELOAD");
            std::env::remove_var("ASHD_FAKE_POWER");
            assert!(reload_enabled());
            std::env::set_var("ASHD_NO_RELOAD", "1");
            assert!(!reload_enabled());
            std::env::set_var("ASHD_NO_RELOAD", "0");
            assert!(reload_enabled(), "0 means reload");
            std::env::remove_var("ASHD_NO_RELOAD");
            std::env::set_var("ASHD_FAKE_POWER", "1");
            assert!(!reload_enabled());
            std::env::remove_var("ASHD_FAKE_POWER");

            std::env::set_var("ASHD_FM_BIN", "/opt/fm");
            assert_eq!(fm_bin(), "/opt/fm");
            std::env::set_var("ASHD_FM_BIN", "");
            assert_eq!(fm_bin(), "fm");
            std::env::remove_var("ASHD_FM_BIN");
        }
    }
}
