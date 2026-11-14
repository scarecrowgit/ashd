//! `cargo xtask smoke`: drive every IPC request the `ashd` CLI exposes against a running
//! `ashd-shell` in a nested session with a private D-Bus bus.
//!
//! Each step: run the CLI, classify the answer (ok / "not implemented yet" / failure),
//! check the shell is alive and did not panic, verify the visible state change where the
//! shell core guarantees one (modal open/closed, scratchpad surface, theme), screenshot.
//! "no panel handles `x` yet" / "unavailable" / "no active media player" are reported as
//! not-impl, not failed. `scratch hide|show` act on a client window (`$SMOKE_CLIENT`, else
//! foot) and need the ashd niri fork with a hidden scratch workspace (`--niri-config`).

use std::{process::ExitCode, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Args;
use ashd_ipc::{Response, ShellStatus};
use ashd_tokens::ThemeName;

use crate::{
    harness::{self, Harness, AshdOut},
    nested::{StartOpts, parse_size},
    shot::Via,
    util::{self, wait_until},
};

#[derive(Args)]
pub struct SmokeArgs {
    #[arg(long, default_value = "1920x1080")]
    pub size: String,
    /// Do not `cargo build -p ashd-shell -p ashd-cli` first.
    #[arg(long)]
    pub no_build: bool,
    #[arg(long, value_enum, default_value = "grim")]
    pub via: Via,
    /// Also run `ashd lock` (locks the nested session; runs last).
    #[arg(long)]
    pub include_lock: bool,
    /// Steps to skip (by name), e.g. `--skip osd-brightness`.
    #[arg(long)]
    pub skip: Vec<String>,
    /// Keep the session root (logs) instead of removing it.
    #[arg(long)]
    pub keep: bool,
    /// niri config replacing the built-in one; the `scratch hide|show` steps need the ashd
    /// fork (`NIRI=`) and `workspace "scratch" { hidden }` in it.
    #[arg(long)]
    pub niri_config: Option<std::path::PathBuf>,
}

impl SmokeArgs {
    pub fn defaults() -> Self {
        Self { size: "1920x1080".into(), no_build: false, via: Via::Grim, include_lock: false, skip: vec![], keep: false, niri_config: None }
    }
}

pub fn dispatch(args: SmokeArgs) -> Result<ExitCode> {
    Ok(if run(&args)? { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// What must be true after a step (only things the shell core itself guarantees).
#[derive(Clone, Copy, Debug, PartialEq)]
enum After {
    Nothing,
    /// A modal is open (only checked when the answer was `ok`).
    ModalOpen,
    ModalClosed,
    Scratch(bool),
    /// The scratchpad surface appeared or disappeared (toggle).
    ScratchFlip,
    /// The shell's theme became this one.
    ThemeIs(ThemeName),
    /// The shell's theme became the next of the one before the step.
    ThemeNext,
}

#[derive(Clone, Debug)]
struct Step {
    name: &'static str,
    args: &'static [&'static str],
    after: After,
    /// Wait before the screenshot (animations, OSD hold is 1.2 s).
    settle_ms: u64,
}

const fn step(name: &'static str, args: &'static [&'static str], after: After, settle_ms: u64) -> Step {
    Step { name, args, after, settle_ms }
}

fn steps() -> Vec<Step> {
    use After::*;
    vec![
        step("ping", &["ping"], Nothing, 100),
        step("status", &["status"], Nothing, 100),
        step("launcher", &["launcher"], ModalOpen, 500),
        step("dismiss-1", &["dismiss"], ModalClosed, 300),
        step("launcher-calc", &["launcher", "--mode", "calc"], ModalOpen, 500),
        step("dismiss-2", &["dismiss"], ModalClosed, 300),
        step("quick-settings", &["quick-settings"], ModalOpen, 500),
        step("quick-settings-close", &["quick-settings", "--close"], ModalClosed, 300),
        step("session", &["session"], ModalOpen, 500),
        step("dismiss-3", &["dismiss"], ModalClosed, 300),
        step("notifications", &["notifications"], ModalOpen, 500),
        step("dismiss-4", &["dismiss"], ModalClosed, 300),
        step("clipboard", &["clipboard"], ModalOpen, 500),
        step("dismiss-5", &["dismiss"], ModalClosed, 300),
        step("cheatsheet", &["cheatsheet"], ModalOpen, 500),
        step("dismiss-6", &["dismiss"], ModalClosed, 300),
        step("osd-volume-up", &["osd", "volume", "+5"], Nothing, 350),
        step("osd-volume-down", &["osd", "volume", "-5"], Nothing, 350),
        step("osd-volume-mute", &["osd", "volume", "mute"], Nothing, 350),
        step("osd-mic-mute", &["osd", "mic-mute"], Nothing, 350),
        step("osd-brightness", &["osd", "brightness", "+5"], Nothing, 350),
        step("scratch-toggle", &["scratch", "toggle"], ScratchFlip, 400),
        step("scratch-toggle-again", &["scratch", "toggle"], ScratchFlip, 400),
        step("scratch-close", &["scratch", "close"], Scratch(false), 400),
        step("scratch-open", &["scratch", "open"], Scratch(true), 400),
        step("canvas-toggle", &["canvas", "toggle"], Nothing, 500),
        step("canvas-zoom-in", &["canvas", "zoom-in"], Nothing, 300),
        step("canvas-zoom-out", &["canvas", "zoom-out"], Nothing, 300),
        step("canvas-fit", &["canvas", "fit"], Nothing, 300),
        step("canvas-home", &["canvas", "home"], Nothing, 300),
        step("canvas-toggle-back", &["canvas", "toggle"], Nothing, 500),
        step("theme-get", &["theme", "get"], Nothing, 100),
        step("theme-cycle", &["theme", "cycle", "--no-reload"], ThemeNext, 900),
        step("theme-set-moss", &["theme", "set", "moss", "--no-reload"], ThemeIs(ThemeName::Moss), 900),
        step("theme-set-dusk", &["theme", "set", "dusk", "--no-reload"], ThemeIs(ThemeName::Dusk), 900),
        step("theme-set-ash", &["theme", "set", "ash", "--no-reload"], ThemeIs(ThemeName::Ash), 900),
        // popups and overlays added after M1
        step("calendar", &["calendar"], ModalOpen, 500),
        step("dismiss-7", &["dismiss"], ModalClosed, 300),
        step("media", &["media"], ModalOpen, 500),
        step("media-play-pause", &["media", "play-pause"], Nothing, 200),
        step("dismiss-8", &["dismiss"], ModalClosed, 300),
        step("theme-pick", &["theme", "pick"], ModalOpen, 700),
        step("dismiss-9", &["dismiss"], ModalClosed, 300),
        step("screenshot-region", &["screenshot", "--region"], ModalOpen, 900),
        step("dismiss-10", &["dismiss"], ModalClosed, 400),
        // Opens the overlay in "Record"; nothing records until a region is confirmed.
        step("record-toggle", &["record", "toggle"], ModalOpen, 900),
        step("dismiss-11", &["dismiss"], ModalClosed, 400),
        // hide / show the client window started before the steps (ashd fork + a
        // `workspace "scratch" { hidden }` niri config)
        step("scratch-hide", &["scratch", "hide"], Nothing, 500),
        step("scratch-show", &["scratch", "show"], Nothing, 500),
        step("app-quit", &["app", "quit"], Nothing, 200),
        step("dismiss-idle", &["dismiss"], ModalClosed, 200),
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Ok,
    NotImplemented(String),
    Fail(String),
    Skipped(String),
}

impl Outcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotImplemented(_) => "not-impl",
            Self::Fail(_) => "FAIL",
            Self::Skipped(_) => "skipped",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Ok => "",
            Self::NotImplemented(s) | Self::Fail(s) | Self::Skipped(s) => s,
        }
    }
}

/// Classify the CLI's answer.
pub fn classify(o: &AshdOut) -> Outcome {
    if o.timed_out {
        return Outcome::Fail("timed out".into());
    }
    match (o.code, o.response()) {
        (Some(0), _) => Outcome::Ok,
        (Some(1), Some(Response::Error(e))) if is_not_implemented(&e) => Outcome::NotImplemented(e),
        (Some(1), Some(Response::Error(e))) => Outcome::Fail(format!("error response: {e}")),
        (Some(2), _) => Outcome::Fail("ashd-shell is not running (exit 2)".into()),
        (code, resp) => Outcome::Fail(format!("exit {code:?}, response {resp:?}, stderr `{}`", o.stderr.trim())),
    }
}

/// The documented "nothing handles this yet / service absent on this machine" answers.
pub fn is_not_implemented(e: &str) -> bool {
    e.contains("no panel handles")
        || e.contains("not implemented")
        || e.contains("unavailable")
        // nothing to control in the nested session (no MPRIS player)
        || e.contains("no active media player")
}

struct Row {
    name: String,
    cmd: String,
    outcome: Outcome,
    note: String,
    alive: bool,
}

fn surfaces_of(st: &ShellStatus, kind: &str) -> usize {
    st.surfaces.iter().filter(|s| s.kind == kind).count()
}

/// Check the post-condition; returns a failure description.
fn check_after(after: After, resp_ok: bool, before_theme: ThemeName, before_scratch: bool, st: &ShellStatus) -> Option<String> {
    match after {
        After::Nothing => None,
        After::ModalOpen => (resp_ok && st.modal.is_none()).then(|| "answered ok but no modal is open".into()),
        After::ModalClosed => st.modal.as_ref().map(|m| format!("modal `{m}` is still open")),
        After::Scratch(want) => {
            let have = surfaces_of(st, "scratchpad") > 0;
            (have != want).then(|| format!("scratchpad surface present = {have}, expected {want}"))
        }
        After::ScratchFlip => {
            let have = surfaces_of(st, "scratchpad") > 0;
            (have == before_scratch).then(|| format!("scratchpad surface present = {have}, expected it to flip"))
        }
        After::ThemeIs(t) => (st.theme != t).then(|| format!("shell theme is {}, expected {t}", st.theme)),
        After::ThemeNext => {
            let want = before_theme.next();
            (st.theme != want).then(|| format!("shell theme is {}, expected {want}", st.theme))
        }
    }
}

pub fn run(args: &SmokeArgs) -> Result<bool> {
    let (w, h) = parse_size(&args.size)?;
    if !args.no_build {
        harness::build_bins()?;
    }
    let opts = StartOpts { size: (w, h), with_bus: true, theme: ThemeName::Ash, die_with_parent: true, niri_config: args.niri_config.clone() };
    let mut hs = Harness::new(&opts, args.via)?;
    let notes = util::notes_dir();
    std::fs::create_dir_all(&notes)?;
    hs.start_shell(&[])?;
    hs.wait_ping(Duration::from_secs(90)).context("waiting for ashd ping")?;
    // Never drive anything that could power off / reboot / log out the real machine
    // unless the shell itself is in fake-power mode.
    for (var, want) in [("ASHD_FAKE_POWER", "1"), ("ASHD_NO_RELOAD", "1")] {
        if hs.shell_env(var).as_deref() != Some(want) {
            hs.stop_shell();
            bail!("refusing to run: {var}={want} is not in ashd-shell's environment");
        }
    }
    println!("smoke: ashd-shell is up in {} (private bus {})", hs.session.root.display(), hs.session.bus.as_deref().unwrap_or("-"));
    util::sleep_ms(500);

    let mut rows: Vec<Row> = Vec::new();
    let mut dead = false;
    let mut seen_panics = 0usize;
    let first = hs.status().ok();
    let mut theme_now = first.as_ref().map(|s| s.theme).unwrap_or(ThemeName::Ash);
    let mut scratch_now = first.as_ref().is_some_and(|s| surfaces_of(s, "scratchpad") > 0);
    // A plain client window for `scratch hide|show` (and `app quit`).
    let client = start_client(&hs);
    if client.is_some() {
        util::sleep_ms(1500);
    }
    let mut all_steps = steps();
    if args.include_lock {
        all_steps.push(step("lock", &["lock"], After::Nothing, 800));
    }

    for s in all_steps {
        let cmd = format!("ashd {}", s.args.join(" "));
        if args.skip.iter().any(|k| k == s.name) {
            rows.push(Row { name: s.name.into(), cmd, outcome: Outcome::Skipped("--skip".into()), note: String::new(), alive: true });
            continue;
        }
        if client.is_none() && matches!(s.name, "scratch-hide" | "scratch-show") {
            let why = "no client window ($SMOKE_CLIENT or foot in PATH)".into();
            rows.push(Row { name: s.name.into(), cmd, outcome: Outcome::Skipped(why), note: String::new(), alive: true });
            continue;
        }
        if dead {
            rows.push(Row {
                name: s.name.into(),
                cmd,
                outcome: Outcome::Skipped("shell is dead".into()),
                note: String::new(),
                alive: false,
            });
            continue;
        }
        let o = hs.ashd(s.args)?;
        let mut outcome = classify(&o);
        let mut notes_txt: Vec<String> = Vec::new();
        util::sleep_ms(s.settle_ms);

        let alive = hs.shell_alive();
        if !alive {
            dead = true;
            outcome = Outcome::Fail(format!("ashd-shell died ({:?})", hs.shell_exit()));
        } else {
            // The shell applies some changes a frame or two later: poll briefly.
            let mut last: Option<Result<ShellStatus>> = None;
            let mut failure: Option<String> = None;
            let resp_ok = matches!(outcome, Outcome::Ok);
            let _ = wait_until(Duration::from_millis(2500), Duration::from_millis(150), || {
                let r = hs.status();
                let done = match &r {
                    Ok(st) => {
                        failure = check_after(s.after, resp_ok, theme_now, scratch_now, st);
                        failure.is_none()
                    }
                    Err(_) => true,
                };
                last = Some(r);
                done.then_some(())
            });
            match last {
                Some(Ok(st)) => {
                    if let Some(f) = failure
                        && !matches!(outcome, Outcome::Fail(_))
                    {
                        outcome = Outcome::Fail(f);
                    }
                    theme_now = st.theme;
                    scratch_now = surfaces_of(&st, "scratchpad") > 0;
                }
                Some(Err(e)) => outcome = Outcome::Fail(format!("status after step: {e:#}")),
                None => {}
            }
            if s.name == "theme-get"
                && let Some(Response::Theme(t)) = o.response()
                && t != theme_now
            {
                outcome = Outcome::Fail(format!("theme get says {t}, status says {theme_now}"));
            }
        }
        let panics = hs.panics();
        if panics.len() > seen_panics {
            outcome = Outcome::Fail(format!("shell panicked: {}", panics[seen_panics]));
            seen_panics = panics.len();
        }
        if alive {
            let path = notes.join(format!("smoke-{}.png", s.name));
            if let Err(e) = hs.screenshot(&path) {
                notes_txt.push(format!("screenshot failed: {e:#}"));
            }
        }
        rows.push(Row { name: s.name.into(), cmd, outcome, note: notes_txt.join("; "), alive });
    }

    // Last: quit-shell must make the shell exit.
    if !dead {
        let o = hs.ashd(&["quit-shell"])?;
        let mut outcome = classify(&o);
        let gone = wait_until(Duration::from_secs(8), Duration::from_millis(50), || (!hs.shell_alive()).then_some(()));
        if gone.is_none() {
            outcome = Outcome::Fail("ashd-shell did not exit after quit-shell".into());
        } else if let Some(st) = hs.shell_exit()
            && !st.success()
        {
            outcome = Outcome::Fail(format!("ashd-shell exited with {st} after quit-shell"));
        }
        rows.push(Row { name: "quit-shell".into(), cmd: "ashd quit-shell".into(), outcome, note: String::new(), alive: false });
    }

    let panics = hs.panics();
    let final_panics = panics.len();
    print_table(&rows);
    if final_panics > 0 {
        println!("panics in the shell log:");
        for p in panics.iter().take(5) {
            println!("  {p}");
        }
    }
    let failed = rows.iter().filter(|r| matches!(r.outcome, Outcome::Fail(_))).count();
    let not_impl = rows.iter().filter(|r| matches!(r.outcome, Outcome::NotImplemented(_))).count();
    let ok = rows.iter().filter(|r| r.outcome == Outcome::Ok).count();
    println!(
        "\nsmoke: {} steps: {ok} ok, {not_impl} not implemented yet, {failed} failed — screenshots smoke-*.png in {}",
        rows.len(),
        notes.display()
    );
    if failed > 0 || final_panics > 0 {
        hs.save_log("smoke-shell.log");
    }
    if args.keep {
        hs.session.stop_keep();
        println!("session root kept: {}", hs.session.root.display());
    }
    if rows.is_empty() {
        bail!("no steps ran");
    }
    Ok(failed == 0 && final_panics == 0)
}

/// A plain Wayland client window inside the session (`$SMOKE_CLIENT`, else `foot` from
/// `PATH`); torn down with the session.
fn start_client(hs: &Harness) -> Option<std::process::Child> {
    let bin = std::env::var_os("SMOKE_CLIENT").map(std::path::PathBuf::from).or_else(|| util::find_in_path("foot"))?;
    let mut c = hs.session.command(&bin);
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    match c.spawn() {
        Ok(child) => {
            println!("smoke: client window {}", bin.display());
            Some(child)
        }
        Err(e) => {
            println!("smoke: cannot start {}: {e}", bin.display());
            None
        }
    }
}

fn print_table(rows: &[Row]) {
    let wn = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let wc = rows.iter().map(|r| r.cmd.len()).max().unwrap_or(7).max(7);
    println!("\n{:<wn$}  {:<wc$}  {:<8}  alive  detail", "step", "command", "result");
    for r in rows {
        let detail = if r.note.is_empty() { r.outcome.detail().to_string() } else { format!("{} [{}]", r.outcome.detail(), r.note) };
        println!(
            "{:<wn$}  {:<wc$}  {:<8}  {:<5}  {detail}",
            r.name,
            r.cmd,
            r.outcome.label(),
            if r.alive { "yes" } else { "no" }
        );
    }
}

#[cfg(test)]
mod tests {
    use ashd_ipc::SurfaceStatus;

    use super::*;

    fn out(code: i32, stdout: &str) -> AshdOut {
        AshdOut { code: Some(code), stdout: stdout.into(), stderr: String::new(), timed_out: false }
    }

    #[test]
    fn classification() {
        assert_eq!(classify(&out(0, "\"ok\"")), Outcome::Ok);
        assert_eq!(classify(&out(0, "{\"theme\":\"ink\"}")), Outcome::Ok);
        assert!(matches!(
            classify(&out(1, "{\"error\":\"no panel handles `clipboard` yet\"}")),
            Outcome::NotImplemented(_)
        ));
        assert!(matches!(classify(&out(1, "{\"error\":\"brightness unavailable\"}")), Outcome::NotImplemented(_)));
        assert!(matches!(classify(&out(1, "{\"error\":\"boom\"}")), Outcome::Fail(_)));
        assert!(matches!(classify(&out(2, "")), Outcome::Fail(_)));
        assert!(matches!(classify(&out(1, "garbage")), Outcome::Fail(_)));
        let mut t = out(0, "");
        t.timed_out = true;
        assert!(matches!(classify(&t), Outcome::Fail(_)));
    }

    #[test]
    fn every_cli_request_is_covered() {
        let all: Vec<String> = steps().iter().map(|s| s.args.join(" ")).collect();
        for needle in [
            "ping", "status", "launcher", "launcher --mode calc", "quick-settings", "session", "notifications",
            "clipboard", "cheatsheet", "osd volume +5", "osd volume mute", "osd mic-mute", "osd brightness +5",
            "scratch toggle", "scratch close", "scratch open", "canvas toggle", "canvas zoom-in", "canvas zoom-out",
            "canvas fit", "canvas home", "theme get", "theme cycle --no-reload", "dismiss", "app quit",
            "calendar", "media", "media play-pause", "theme pick", "screenshot --region", "record toggle",
            "scratch hide", "scratch show",
        ] {
            assert!(all.iter().any(|a| a == needle), "no smoke step for `ashd {needle}`");
        }
        let mut names: Vec<_> = steps().iter().map(|s| s.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len(), "step names must be unique (they name the screenshots)");
    }

    #[test]
    fn post_conditions() {
        let mut st = ShellStatus {
            theme: ThemeName::Ash,
            surfaces: vec![SurfaceStatus { kind: "scratchpad".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(check_after(After::Scratch(true), true, ThemeName::Ash, false, &st).is_none());
        assert!(check_after(After::Scratch(false), true, ThemeName::Ash, false, &st).is_some());
        assert!(check_after(After::ScratchFlip, true, ThemeName::Ash, true, &st).is_some());
        assert!(check_after(After::ScratchFlip, true, ThemeName::Ash, false, &st).is_none());
        assert!(check_after(After::ModalOpen, true, ThemeName::Ash, false, &st).is_some());
        // a not-implemented answer opens nothing, which is fine
        assert!(check_after(After::ModalOpen, false, ThemeName::Ash, false, &st).is_none());
        st.modal = Some("launcher".into());
        assert!(check_after(After::ModalClosed, true, ThemeName::Ash, false, &st).is_some());
        assert!(check_after(After::ThemeNext, true, ThemeName::Moss, false, &st).is_some());
        st.theme = ThemeName::Dusk;
        assert!(check_after(After::ThemeNext, true, ThemeName::Moss, false, &st).is_none());
    }
}
